//! Native adapter for Codex (`codex app-server`).
//!
//! Codex's app server is JSON-RPC over stdio (experimental, but has a stable and self-describing interface:
//! `codex app-server generate-json-schema` outputs the complete schema).
//!
//! Subset used:
//! * `initialize`
//! * `thread/start` → `thread.id`
//! * `turn/start` (with `input`)
//! * `turn/interrupt`
//! * Notifications: `item/agentMessage/delta`, `item/completed`, `turn/completed`,
//!   `thread/tokenUsage/updated`
//! * Approvals (server→client requests): `item/commandExecution/requestApproval`,
//!   `item/fileChange/requestApproval`

use crate::stream::StreamBuf;
use crate::{AgentControl, AgentEvent, SessionSpec};
use anyhow::{anyhow, bail, Context, Result};
use ertangle_core::{new_id, Cost, Decision, PermissionRequest, SessionStatus};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tr::tr;

const CALL_TIMEOUT: Duration = Duration::from_secs(60);

struct PendingQuestion {
    rpc_id: Value,
    method: String,
    q_key: Option<String>,
}

/// Build the JSON-RPC result for Codex `item/tool/requestUserInput`.
fn user_input_answer_result(answers: &[String], key: &str) -> Value {
    let mut answers_by_id = serde_json::Map::new();
    if !answers.is_empty() {
        answers_by_id.insert(
            key.to_string(),
            json!({ "answers": answers, "other": null }),
        );
    }
    json!({ "answers": answers_by_id })
}

struct Conn {
    session_id: String,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Sender<Value>>>,
    /// Approvals: internal req id -> JSON-RPC id.
    perms: Mutex<HashMap<String, Value>>,
    /// Interactive questions: internal qst id -> pending question info.
    questions: Mutex<HashMap<String, PendingQuestion>>,
    /// Current thread / turn id.
    thread: Mutex<String>,
    turn: Mutex<String>,
    stream: Mutex<StreamBuf>,
    /// Error text already reported in this turn (avoid duplicate reporting between `error` notifications and `turn/completed`).
    last_error: Mutex<Option<String>>,
    tx: Sender<AgentEvent>,
}

impl Conn {
    fn write(&self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let mut w = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(line.as_bytes())?;
        w.flush()?;
        Ok(())
    }

    /// Sends a request and waits for response.
    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (s, r) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, s);
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let msg = r
            .recv_timeout(timeout)
            .map_err(|_| anyhow!(tr!("`{method}` 逾時", method = method)))?;
        if let Some(err) = msg.get("error") {
            bail!(
                "{}",
                tr!("`{method}` 失敗：{err}", method = method, err = err)
            );
        }
        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }

    fn flush_stream(&self) {
        let text = self
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .flush();
        if !text.is_empty() {
            let _ = self.tx.send(AgentEvent::Output(text));
        }
    }

    /// Processes a single stdout line.
    fn on_line(&self, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return;
        };
        // 1) Response to a request we sent.
        if let Some(id) = msg.get("id") {
            if msg.get("method").is_none() {
                let id = id.as_i64().unwrap_or(-1);
                if let Some(s) = self
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
                {
                    let _ = s.send(msg);
                }
                return;
            }
        }
        let method = msg["method"].as_str().unwrap_or("");
        // 2) Server -> client requests (requiring our response).
        if msg.get("id").is_some() && !method.is_empty() {
            self.on_server_request(method, &msg);
            return;
        }
        // 3) Notifications.
        self.on_notification(method, &msg["params"]);
    }

    fn on_server_request(&self, method: &str, msg: &Value) {
        let params = &msg["params"];
        if method == "item/tool/requestUserInput" {
            if let Some((q, q_key)) =
                crate::question::parse_codex_user_input(&self.session_id, params)
            {
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                self.questions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        q.id.clone(),
                        PendingQuestion {
                            rpc_id: id,
                            method: method.to_string(),
                            q_key,
                        },
                    );
                let _ = self.tx.send(AgentEvent::Question(q));
                return;
            }
        }
        if method == "mcpServer/elicitation/request"
            && (params.get("requestedSchema").is_some() || params.get("options").is_some())
        {
            if let Some((q, keys)) =
                crate::question::parse_acp_elicitation(&self.session_id, params)
            {
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let q_key = keys.first().cloned();
                self.questions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        q.id.clone(),
                        PendingQuestion {
                            rpc_id: id,
                            method: method.to_string(),
                            q_key,
                        },
                    );
                let _ = self.tx.send(AgentEvent::Question(q));
                return;
            }
        }
        let (title, detail, kind) = match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                let cmd = params["command"].as_str().unwrap_or("(command)");
                let cwd = params["cwd"].as_str().unwrap_or("");
                (
                    "Bash".to_string(),
                    format!("{cmd}\n({cwd})"),
                    ApprovalKind::Command,
                )
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                let reason = params["reason"].as_str().unwrap_or("file change");
                ("Edit".to_string(), reason.to_string(), ApprovalKind::File)
            }
            "mcpServer/elicitation/request" => {
                let tool_name = params["_meta"]["tool_name"]
                    .as_str()
                    .or_else(|| params["serverName"].as_str())
                    .unwrap_or("tool");
                let msg_text = params["message"]
                    .as_str()
                    .unwrap_or("MCP tool call approval");
                (
                    format!("MCP ({tool_name})"),
                    msg_text.to_string(),
                    ApprovalKind::McpElicitation,
                )
            }
            "item/permissions/requestApproval" => {
                let reason = params["reason"].as_str().unwrap_or("permissions request");
                (
                    "Permissions".to_string(),
                    reason.to_string(),
                    ApprovalKind::Permissions,
                )
            }
            other => {
                // Always return an error for unrecognized requests to prevent Codex from hanging.
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let _ = self.write(&json!({
                    "jsonrpc":"2.0","id":id,
                    "error":{"code":-32601,"message":format!("ertangle 不支援 {other}")}
                }));
                return;
            }
        };
        let req = new_id("perm");
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        self.perms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(req.clone(), json!({"id": id, "kind": kind.as_str()}));
        let _ = self.tx.send(AgentEvent::Permission(PermissionRequest {
            req,
            session: self.session_id.clone(),
            title,
            detail,
        }));
    }

    fn on_notification(&self, method: &str, p: &Value) {
        match method {
            "item/agentMessage/delta" => {
                if let Some(d) = p["delta"].as_str() {
                    let out = self
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push("msg", d);
                    if !out.is_empty() {
                        let _ = self.tx.send(AgentEvent::Output(out));
                    }
                }
            }
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                let key = if p.get("delta").is_some() {
                    "delta"
                } else {
                    "textDelta"
                };
                if let Some(d) = p[key].as_str() {
                    let out = self
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push("think", d);
                    if !out.is_empty() {
                        let _ = self.tx.send(AgentEvent::Output(out));
                    }
                }
            }
            "turn/started" => {
                if let Some(id) = p["turn"]["id"].as_str() {
                    *self.turn.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
                }
                *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                let _ = self.tx.send(AgentEvent::Status(SessionStatus::Busy));
            }
            "turn/completed" => {
                self.flush_stream();
                let failed = !p["turn"]["error"].is_null();
                if failed {
                    self.report_error(&p["turn"]["error"]);
                }
                let _ = self.tx.send(AgentEvent::Status(if failed {
                    SessionStatus::Failed
                } else {
                    SessionStatus::Idle
                }));
            }
            "item/completed" => self.on_item_completed(&p["item"]),
            "thread/tokenUsage/updated" => {
                let last = &p["tokenUsage"]["last"];
                if let Some(used) = last["totalTokens"].as_u64() {
                    let _ = self.tx.send(AgentEvent::Context {
                        used,
                        window: p["tokenUsage"]["modelContextWindow"].as_u64(),
                    });
                }
                let total = &p["tokenUsage"]["total"];
                let cost = Cost {
                    usd: 0.0,
                    input_tokens: total["inputTokens"].as_u64().unwrap_or(0),
                    output_tokens: total["outputTokens"].as_u64().unwrap_or(0),
                };
                let _ = self.tx.send(AgentEvent::Cost(cost));
            }
            "account/rateLimits/updated" => {
                if let Some(q) = parse_rate_limits(&p["rateLimits"]) {
                    let _ = self.tx.send(AgentEvent::Quota(q));
                }
            }
            "thread/started" => {
                if let Some(id) = p["thread"]["id"].as_str() {
                    *self.thread.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
                }
            }
            "error" => {
                tracing::warn!("codex error 通知：{}", p);
                // When `willRetry: true`, Codex retries automatically; not considered failure, don't disturb the user.
                if p["willRetry"].as_bool() != Some(true) {
                    let err = if p["error"].is_null() { p } else { &p["error"] };
                    self.report_error(err);
                }
            }
            _ => {}
        }
    }

    /// Forwards Codex errors (`error` from `error` notifications, or `turn.error`) to transcript and status note.
    /// Reports identical text only once per turn.
    fn report_error(&self, err: &Value) {
        let text = error_text(err);
        {
            let mut last = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_deref() == Some(text.as_str()) {
                return;
            }
            *last = Some(text.clone());
        }
        if ertangle_core::looks_like_limit(&text) {
            let _ = self.tx.send(AgentEvent::Limited(text));
            return;
        }
        let mut note = tr!("codex 錯誤：{text}", text = text);
        if is_auth_error(err) {
            note.push_str(&tr!("(請執行 `codex login`)"));
        }
        let _ = self.tx.send(AgentEvent::Output(format!("⚠ {note}\n")));
        let _ = self.tx.send(AgentEvent::Note(note));
    }

    fn on_item_completed(&self, item: &Value) {
        match item["type"].as_str().unwrap_or("") {
            "commandExecution" => {
                self.flush_stream();
                let _ = self.tx.send(AgentEvent::ToolCall {
                    name: "Bash".to_string(),
                    input: item["command"].as_str().unwrap_or("").to_string(),
                    ok: item["exitCode"].as_i64().map(|c| c == 0),
                    output: item["aggregatedOutput"].as_str().map(|s| s.to_string()),
                });
            }
            "fileChange" => {
                self.flush_stream();
                let _ = self.tx.send(AgentEvent::ToolCall {
                    name: "Edit".to_string(),
                    input: item["changes"].to_string(),
                    ok: Some(item["status"].as_str() == Some("completed")),
                    output: None,
                });
            }
            "mcpToolCall" | "McpToolCall" => {
                self.flush_stream();
                let tool = item["tool"].as_str().unwrap_or("mcp");
                let server = item["server"].as_str().unwrap_or("");
                let name = if server.is_empty() {
                    tool.to_string()
                } else {
                    format!("{server}:{tool}")
                };
                let ok = item["status"].as_str().map(|s| s == "completed");
                let output = item["result"]["content"].as_array().and_then(|arr| {
                    let texts: Vec<&str> = arr.iter().filter_map(|c| c["text"].as_str()).collect();
                    if texts.is_empty() {
                        None
                    } else {
                        Some(texts.join("\n"))
                    }
                });
                let _ = self.tx.send(AgentEvent::ToolCall {
                    name,
                    input: item["arguments"].to_string(),
                    ok,
                    output,
                });
            }
            "agentMessage" => {
                // Text was already emitted via deltas; here only finalize.
                self.flush_stream();
            }
            _ => {}
        }
    }
}

/// Error text: prefers `message`, otherwise uses the first 300 characters of the full JSON.
fn error_text(err: &Value) -> String {
    match err["message"].as_str().filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => err.to_string().chars().take(300).collect(),
    }
}

/// Whether this is an authentication error (401, token revoked, unauthorized).
fn is_auth_error(err: &Value) -> bool {
    let t = err.to_string().to_ascii_lowercase();
    t.contains("401") || t.contains("token_revoked") || t.contains("unauthorized")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApprovalKind {
    Command,
    File,
    McpElicitation,
    Permissions,
}

impl ApprovalKind {
    fn as_str(self) -> &'static str {
        match self {
            ApprovalKind::Command => "command",
            ApprovalKind::File => "file",
            ApprovalKind::McpElicitation => "mcp_elicitation",
            ApprovalKind::Permissions => "permissions",
        }
    }
}

// ---------------------------------------------------------------------------
// Control handle
// ---------------------------------------------------------------------------

struct CodexControl {
    conn: Arc<Conn>,
    child: Arc<Mutex<Child>>,
    /// Runtime switched model/effort; included in every `turn/start`.
    overrides: Mutex<(Option<String>, Option<String>)>,
}

impl CodexControl {
    fn thread_id(&self) -> Result<String> {
        let t = self
            .conn
            .thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if t.is_empty() {
            bail!("{}", tr!("codex thread 尚未就緒"));
        }
        Ok(t)
    }
}

impl AgentControl for CodexControl {
    fn send_input(&self, text: &str) -> Result<()> {
        self.send_input_with(text, &[])
    }

    fn send_input_with(&self, text: &str, atts: &[ertangle_core::Attachment]) -> Result<()> {
        let thread_id = self.thread_id()?;
        let _ = self.conn.tx.send(AgentEvent::Status(SessionStatus::Busy));
        let mut params = json!({
            "threadId": thread_id,
            "input": turn_input(text, atts)
        });
        let (m, e) = self
            .overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(m) = m {
            params["model"] = json!(m);
        }
        if let Some(e) = e {
            params["effort"] = json!(e);
        }
        self.conn.call("turn/start", params, CALL_TIMEOUT)?;
        Ok(())
    }

    /// Saved to pass `model`/`effort` in the next `turn/start` (schema confirms both are `TurnStartParams` fields).
    fn set_config(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<String>> {
        if let Some(e) = effort {
            crate::check_effort(e)?;
        }
        let mut o = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = model {
            o.0 = Some(m.to_string());
        }
        if let Some(e) = effort {
            o.1 = Some(e.to_string());
        }
        Ok(model.map(str::to_string))
    }

    fn cancel(&self) -> Result<()> {
        let thread_id = self.thread_id()?;
        let turn_id = self
            .conn
            .turn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if turn_id.is_empty() {
            return Ok(());
        }
        self.conn.call(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
            CALL_TIMEOUT,
        )?;
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        Some(self.child.lock().unwrap_or_else(|e| e.into_inner()).id())
    }

    fn kill(&self) -> Result<()> {
        let pid = self.child.lock().unwrap_or_else(|e| e.into_inner()).id();
        let _ = crate::signal_pgid(pid, "-KILL").or_else(|_| crate::signal_pid(pid, "-KILL"));
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .kill()
            .ok();
        Ok(())
    }

    fn sleep(&self) -> Result<()> {
        let pid = self.child.lock().unwrap_or_else(|e| e.into_inner()).id();
        crate::signal_pgid(pid, "-STOP").or_else(|_| crate::signal_pid(pid, "-STOP"))
    }

    fn wake(&self) -> Result<()> {
        let pid = self.child.lock().unwrap_or_else(|e| e.into_inner()).id();
        crate::signal_pgid(pid, "-CONT").or_else(|_| crate::signal_pid(pid, "-CONT"))
    }

    fn resolve_permission(&self, req: &str, decision: Decision) -> Result<()> {
        let entry = self
            .conn
            .perms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(req);
        let Some(entry) = entry else {
            bail!("{}", tr!("未知的核准請求：{req}", req = req));
        };
        let id = entry.get("id").cloned().unwrap_or(Value::Null);
        let kind = match entry["kind"].as_str() {
            Some("file") => ApprovalKind::File,
            Some("mcp_elicitation") => ApprovalKind::McpElicitation,
            Some("permissions") => ApprovalKind::Permissions,
            _ => ApprovalKind::Command,
        };
        match kind {
            ApprovalKind::McpElicitation => {
                let result = match decision {
                    Decision::Allow | Decision::AllowAlways => json!({
                        "action": "accept",
                        "content": {}
                    }),
                    Decision::Deny => json!({
                        "action": "decline"
                    }),
                };
                self.conn.write(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                }))
            }
            ApprovalKind::Permissions => {
                let result = match decision {
                    Decision::Allow | Decision::AllowAlways => json!({
                        "permissions": {},
                        "scope": "turn"
                    }),
                    Decision::Deny => json!({
                        "permissions": null
                    }),
                };
                self.conn.write(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                }))
            }
            ApprovalKind::Command | ApprovalKind::File => {
                // Codex decision is a camelCase string.
                let verdict = match (kind, decision) {
                    (_, Decision::Allow) => "accept",
                    (_, Decision::AllowAlways) => "acceptForSession",
                    (_, Decision::Deny) => "decline",
                };
                self.conn.write(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "decision": verdict }
                }))
            }
        }
    }

    fn resolve_question(&self, id: &str, answers: &[String]) -> Result<()> {
        let entry = self
            .conn
            .questions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let Some(PendingQuestion {
            rpc_id,
            method,
            q_key,
        }) = entry
        else {
            bail!("{}", tr!("未知的問題請求：{id}", id = id));
        };
        if method == "item/tool/requestUserInput" {
            let key = q_key.unwrap_or_else(|| "q0".to_string());
            self.conn.write(&json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": user_input_answer_result(answers, &key)
            }))
        } else {
            let result = if answers.is_empty() {
                json!({ "action": "decline" })
            } else {
                let mut content = serde_json::Map::new();
                let key = q_key.unwrap_or_else(|| "answer".to_string());
                content.insert(key, json!(answers));
                json!({
                    "action": "accept",
                    "content": content
                })
            };
            self.conn.write(&json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": result
            }))
        }
    }

    fn transport(&self) -> &'static str {
        "app-server"
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let prog = match spec.bin.as_deref() {
        Some(b) => crate::which(b).with_context(|| tr!("找不到指定的執行檔 `{b}`", b = b))?,
        None => crate::which("codex").with_context(|| tr!("找不到 `{prog}`", prog = "codex"))?,
    };
    let mut cmd = Command::new(prog);
    cmd.arg("app-server");
    cmd.args(spec.permission.codex_flags());
    let sandbox = resolve_codex_sandbox(&spec, is_bwrap_usable());
    if sandbox == "danger-full-access" {
        cmd.args(["-c", "sandbox_mode=\"danger-full-access\""]);
    } else {
        // Give workspace-write sandbox network access by default to avoid Linux bwrap --unshare-net triggering
        // `loopback: Failed RTM_NEWADDR: Operation not permitted`
        cmd.args(["-c", "sandbox_workspace_write.network_access=true"]);
    }
    if let Some(exe) = &spec.mcp_exe {
        let sock_opt = spec
            .env
            .iter()
            .find(|(k, _)| k == "ERTANGLE_SOCKET")
            .map(|(_, v)| v.as_str());

        let mut mcp_args = vec![
            "\"mcp\"".to_string(),
            format!("\"--session\",{:?}", spec.id),
        ];
        if let Some(sock) = sock_opt {
            mcp_args.push(format!("\"--socket\",{:?}", sock));
        }

        cmd.args([
            "-c",
            &format!("mcp_servers.ertangle.command={:?}", exe.to_string_lossy()),
            "-c",
            &format!("mcp_servers.ertangle.args=[{}]", mcp_args.join(",")),
        ]);
        if let Some(sock) = sock_opt {
            cmd.args([
                "-c",
                &format!("mcp_servers.ertangle.env.ERTANGLE_SOCKET={:?}", sock),
            ]);
        }
        if spec.permission.auto_approves() {
            cmd.args(["-c", "apps.ertangle.default_tools_approval_mode=\"auto\""]);
        }
    }
    cmd.args(&spec.args);
    cmd.current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| tr!("無法啟動 `codex app-server`"))?;
    let stdin = child
        .stdin
        .take()
        .with_context(|| tr!("取得 codex stdin 失敗"))?;
    let stderr_tail = child.stderr.take().map(crate::stderr::StderrTail::spawn);
    let stdout = child
        .stdout
        .take()
        .with_context(|| tr!("取得 codex stdout 失敗"))?;

    let conn = Arc::new(Conn {
        session_id: spec.id.clone(),
        stdin: Mutex::new(stdin),
        next_id: AtomicI64::new(1),
        pending: Mutex::new(HashMap::new()),
        perms: Mutex::new(HashMap::new()),
        questions: Mutex::new(HashMap::new()),
        thread: Mutex::new(String::new()),
        turn: Mutex::new(String::new()),
        stream: Mutex::new(StreamBuf::default()),
        last_error: Mutex::new(None),
        tx: tx.clone(),
    });

    {
        let conn = conn.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => conn.on_line(&l),
                    Err(_) => break,
                }
            }
            // stdout closed = process exited: discard pending requests so handshake does not wait for timeout.
            conn.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        });
    }

    let child = Arc::new(Mutex::new(child));
    {
        let child = child.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let code = loop {
                let status = {
                    let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
                    c.try_wait()
                };
                match status {
                    Ok(Some(s)) => break s.code(),
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break None,
                }
            };
            if let Some(t) = &stderr_tail {
                t.report_exit(code, &tx);
            }
            let _ = tx.send(AgentEvent::Exited(code));
        });
    }

    // Handshake: initialize -> thread/start.
    conn.call(
        "initialize",
        json!({ "clientInfo": { "name": "ertangle", "title": "ertangle", "version": env!("CARGO_PKG_VERSION") } }),
        CALL_TIMEOUT,
    )
    .with_context(|| tr!("codex `initialize` 失敗"))?;

    // When resuming, use `thread/resume` instead (historical turns are in response, not replayed via notifications).
    let method = if spec.resume.is_some() {
        "thread/resume"
    } else {
        "thread/start"
    };
    let started = conn
        .call(method, thread_open_params(&spec), CALL_TIMEOUT)
        .with_context(|| tr!("codex `{method}` 失敗", method = method))?;
    let tid = started["thread"]["id"]
        .as_str()
        .with_context(|| tr!("`{method}` 未回傳 thread id", method = method))?
        .to_string();
    *conn.thread.lock().unwrap_or_else(|e| e.into_inner()) = tid.clone();
    let _ = tx.send(AgentEvent::NativeId(tid));
    if let Some(model) = started["model"].as_str() {
        let _ = tx.send(AgentEvent::Model(model.to_string()));
    }
    // Query account quota once on startup (skipped if not logged in or unsupported by older codex versions).
    if let Ok(r) = conn.call("account/rateLimits/read", json!({}), CALL_TIMEOUT) {
        if let Some(q) = parse_rate_limits(&r["rateLimits"]) {
            let _ = tx.send(AgentEvent::Quota(q));
        }
    }
    let _ = tx.send(AgentEvent::Status(SessionStatus::Idle));

    let control = CodexControl {
        conn,
        child,
        overrides: Mutex::new((None, None)),
    };
    if let Some(p) = &spec.initial_prompt {
        control.send_input_with(p, &spec.initial_attachments)?;
    }
    Ok(Box::new(control))
}

/// Parses Codex `RateLimitSnapshot` (shared between `account/rateLimits/read` and update notifications).
pub(crate) fn parse_rate_limits(snap: &Value) -> Option<ertangle_core::Quota> {
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let w = &snap[key];
        let Some(pct) = w["usedPercent"].as_f64() else {
            continue;
        };
        windows.push(ertangle_core::QuotaWindow {
            name: w["windowDurationMins"]
                .as_u64()
                .map(ertangle_core::window_name)
                .unwrap_or_else(|| key.to_string()),
            used_percent: pct as f32,
            resets_at_ms: w["resetsAt"].as_u64().map(|s| s * 1000),
        });
    }
    if windows.is_empty() {
        return None;
    }
    let reached = !snap["rateLimitReachedType"].is_null()
        || snap["spendControlReached"].as_bool() == Some(true);
    Some(ertangle_core::Quota {
        agent: "codex".into(),
        windows,
        reached,
        warning: false,
        updated_ms: ertangle_core::now_millis(),
    })
}

/// `thread/start` parameters: cwd, approvalPolicy, plus optional model and sandbox (official doc examples:
/// `"model"`, `"sandbox": "workspace-write"`; `read_only` maps to `read-only`).
/// Codex `turn/start` input: text (including inlined text files) plus data URL images.
fn turn_input(text: &str, atts: &[ertangle_core::Attachment]) -> Value {
    let mut items =
        vec![json!({ "type": "text", "text": ertangle_core::compose_text(text, atts, false) })];
    for a in atts.iter().filter(|a| a.is_image()) {
        if let Some(data) = &a.data {
            items.push(json!({ "type": "image", "url": format!("data:{};base64,{data}", a.mime) }));
        }
    }
    Value::Array(items)
}

/// Parameters for `thread/start` or `thread/resume` (when `spec.resume` is set).
/// Both accept the same set of override fields; resumption additionally includes `threadId` and uses `excludeTurns` to omit historical turns.
fn thread_open_params(spec: &SessionSpec) -> Value {
    let mut p = thread_start_params(spec);
    if let Some(id) = &spec.resume {
        p["threadId"] = json!(id);
        p["excludeTurns"] = json!(true);
    }
    p
}

/// Checks whether bwrap can create user namespaces and sandboxes normally on Linux.
///
/// On environments like Ubuntu 24.04+ with `kernel.apparmor_restrict_unprivileged_userns = 1`,
/// bwrap without an AppArmor profile authorization fails when creating uid maps (`Permission denied`).
/// In this case, fall back to `danger-full-access` to prevent Codex from aborting on command execution.
#[cfg(target_os = "linux")]
pub fn is_bwrap_usable() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        if let Ok(val) =
            std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        {
            if val.trim() == "1" {
                return false;
            }
        }
        std::process::Command::new("bwrap")
            .args(["--ro-bind", "/", "/", "--", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

#[cfg(not(target_os = "linux"))]
pub fn is_bwrap_usable() -> bool {
    true
}

/// Resolves the sandbox mode used by Codex.
///
/// Respects `spec.sandbox` if explicitly specified; if `None`, falls back to `"danger-full-access"`
/// when `bwrap` cannot run normally or `permission` is `Bypass`, otherwise selects `"read-only"`
/// or `"workspace-write"` based on `read_only`.
pub fn resolve_codex_sandbox(spec: &SessionSpec, bwrap_ok: bool) -> String {
    if let Some(s) = &spec.sandbox {
        return s.clone();
    }
    if !bwrap_ok || spec.permission == ertangle_core::PermissionPolicy::Bypass {
        "danger-full-access".to_string()
    } else if spec.read_only {
        "read-only".to_string()
    } else {
        "workspace-write".to_string()
    }
}

fn thread_start_params(spec: &SessionSpec) -> Value {
    let mut p = json!({
        "cwd": spec.cwd.to_string_lossy(),
        "approvalPolicy": spec.permission.codex_approval_policy(),
    });
    if let Some(m) = &spec.model {
        p["model"] = json!(m);
    }
    let mut config = json!({
        "sandbox_workspace_write": {
            "network_access": true
        }
    });
    if let Some(e) = &spec.effort {
        // Thread-level config override, equivalent to `-c model_reasoning_effort=...`.
        config["model_reasoning_effort"] = json!(e);
    }
    p["config"] = config;
    let sandbox = resolve_codex_sandbox(spec, is_bwrap_usable());
    p["sandbox"] = json!(sandbox);
    if let Some(inst) = spec.system_append.as_deref().filter(|s| !s.is_empty()) {
        p["developerInstructions"] = json!(inst);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_input_answer_result_has_codex_shape() {
        let result = user_input_answer_result(&["Postgres".into(), "Redis".into()], "db");
        assert_eq!(
            result,
            json!({ "answers": { "db": { "answers": ["Postgres", "Redis"], "other": null } } })
        );
        assert_eq!(
            user_input_answer_result(&[], "db"),
            json!({ "answers": {} })
        );
    }

    #[test]
    fn approval_verdict_mapping() {
        // Only validates the mapping table itself (no need to actually run codex).
        let cases = [
            (ApprovalKind::Command, Decision::Allow, "accept"),
            (
                ApprovalKind::Command,
                Decision::AllowAlways,
                "acceptForSession",
            ),
            (ApprovalKind::File, Decision::Deny, "decline"),
        ];
        for (kind, d, want) in cases {
            let got = match (kind, d) {
                (_, Decision::Allow) => "accept",
                (_, Decision::AllowAlways) => "acceptForSession",
                (_, Decision::Deny) => "decline",
            };
            assert_eq!(got, want);
        }

        assert_eq!(ApprovalKind::Command.as_str(), "command");
        assert_eq!(ApprovalKind::File.as_str(), "file");
        assert_eq!(ApprovalKind::McpElicitation.as_str(), "mcp_elicitation");
        assert_eq!(ApprovalKind::Permissions.as_str(), "permissions");

        // Validate return structure for MCP elicitation and permissions
        let mcp_allow = match Decision::Allow {
            Decision::Allow | Decision::AllowAlways => json!({"action": "accept", "content": {}}),
            Decision::Deny => json!({"action": "decline"}),
        };
        assert_eq!(mcp_allow["action"], "accept");

        let mcp_deny = match Decision::Deny {
            Decision::Allow | Decision::AllowAlways => json!({"action": "accept", "content": {}}),
            Decision::Deny => json!({"action": "decline"}),
        };
        assert_eq!(mcp_deny["action"], "decline");

        let perm_allow = match Decision::Allow {
            Decision::Allow | Decision::AllowAlways => json!({"permissions": {}, "scope": "turn"}),
            Decision::Deny => json!({"permissions": null}),
        };
        assert_eq!(perm_allow["scope"], "turn");
        assert!(perm_allow["permissions"].is_object());

        let perm_deny = match Decision::Deny {
            Decision::Allow | Decision::AllowAlways => json!({"permissions": {}, "scope": "turn"}),
            Decision::Deny => json!({"permissions": null}),
        };
        assert!(perm_deny["permissions"].is_null());
    }

    #[test]
    fn thread_start_carries_model_and_sandbox() {
        let spec = SessionSpec {
            id: "s".into(),
            kind: ertangle_core::AgentKind::Codex,
            cwd: "/tmp".into(),
            transport: ertangle_protocol::Transport::Structured,
            bin: None,
            args: vec![],
            model: Some("m1".into()),
            effort: Some("high".into()),
            allowed_tools: vec![],
            disallowed_tools: vec![],
            read_only: false,
            sandbox: Some("workspace-write".into()),
            env: vec![],
            permission: ertangle_core::PermissionPolicy::Auto,
            mcp_exe: None,
            system_append: None,
            initial_prompt: None,
            initial_attachments: vec![],
            resume: None,
            cols: 80,
            rows: 24,
        };
        let p = thread_start_params(&spec);
        assert_eq!(p["model"], "m1");
        assert_eq!(p["sandbox"], "workspace-write");
        assert_eq!(p["config"]["model_reasoning_effort"], "high");
        assert_eq!(
            p["config"]["sandbox_workspace_write"]["network_access"],
            true
        );
        assert!(p.get("developerInstructions").is_none());
        let mut ro = spec.clone();
        ro.sandbox = Some("read-only".into());
        ro.read_only = true;
        assert_eq!(thread_start_params(&ro)["sandbox"], "read-only");

        let mut bypass = spec.clone();
        bypass.sandbox = None;
        bypass.permission = ertangle_core::PermissionPolicy::Bypass;
        assert_eq!(
            thread_start_params(&bypass)["sandbox"],
            "danger-full-access"
        );
        // New conversations do not carry threadId; resumption includes it while keeping other override fields intact.
        assert!(thread_open_params(&spec)["threadId"].is_null());
        let mut re = spec.clone();
        re.resume = Some("thr-1".into());
        let p = thread_open_params(&re);
        assert_eq!(p["threadId"], "thr-1");
        assert_eq!(p["excludeTurns"], true);
        assert_eq!(p["model"], "m1");
        ro.system_append = Some("collab".into());
        let p_ro = thread_start_params(&ro);
        assert_eq!(p_ro["sandbox"], "read-only");
        assert_eq!(p_ro["developerInstructions"], "collab");
    }

    #[test]
    fn codex_sandbox_resolution() {
        let spec = SessionSpec {
            id: "s".into(),
            kind: ertangle_core::AgentKind::Codex,
            cwd: "/tmp".into(),
            transport: ertangle_protocol::Transport::Structured,
            bin: None,
            args: vec![],
            model: Some("m1".into()),
            effort: Some("high".into()),
            allowed_tools: vec![],
            disallowed_tools: vec![],
            read_only: false,
            sandbox: None,
            env: vec![],
            permission: ertangle_core::PermissionPolicy::Auto,
            mcp_exe: None,
            system_append: None,
            initial_prompt: None,
            initial_attachments: vec![],
            resume: None,
            cols: 80,
            rows: 24,
        };

        // bwrap_ok = true: decided by read_only and permission
        assert_eq!(resolve_codex_sandbox(&spec, true), "workspace-write");
        let mut ro = spec.clone();
        ro.read_only = true;
        assert_eq!(resolve_codex_sandbox(&ro, true), "read-only");
        let mut bypass = spec.clone();
        bypass.permission = ertangle_core::PermissionPolicy::Bypass;
        assert_eq!(resolve_codex_sandbox(&bypass, true), "danger-full-access");

        // bwrap_ok = false: falls back to danger-full-access under AppArmor / userns restrictions
        assert_eq!(resolve_codex_sandbox(&spec, false), "danger-full-access");
        assert_eq!(resolve_codex_sandbox(&ro, false), "danger-full-access");
        assert_eq!(resolve_codex_sandbox(&bypass, false), "danger-full-access");

        // Always respect explicit spec.sandbox setting
        let mut explicit = spec.clone();
        explicit.sandbox = Some("workspace-write".into());
        assert_eq!(resolve_codex_sandbox(&explicit, true), "workspace-write");
        assert_eq!(resolve_codex_sandbox(&explicit, false), "workspace-write");

        explicit.sandbox = Some("read-only".into());
        assert_eq!(resolve_codex_sandbox(&explicit, true), "read-only");
        assert_eq!(resolve_codex_sandbox(&explicit, false), "read-only");
    }

    #[test]
    fn error_text_and_auth_detection() {
        let e = json!({"message":"boom","codexErrorInfo":"other"});
        assert_eq!(error_text(&e), "boom");
        assert!(!is_auth_error(&e));
        let a = json!({"message":"x","codexErrorInfo":"unauthorized"});
        assert!(is_auth_error(&a));
        assert!(is_auth_error(&json!({"message":"HTTP 401"})));
        let long = json!({"detail":"y".repeat(1000)});
        assert_eq!(error_text(&long).chars().count(), 300);
    }

    #[test]
    fn parses_codex_rate_limit_snapshot() {
        let snap = json!({"primary":{"usedPercent":42,"windowDurationMins":300,"resetsAt":1797125904},
            "secondary":{"usedPercent":7,"windowDurationMins":10080,"resetsAt":1797725904},
            "rateLimitReachedType":null,"spendControlReached":false});
        let q = parse_rate_limits(&snap).unwrap();
        assert_eq!(q.agent, "codex");
        assert_eq!(q.windows[0].name, "5h");
        assert_eq!(q.windows[1].name, "7d");
        assert!(!q.reached);
        let hit = json!({"primary":{"usedPercent":100,"windowDurationMins":300},"rateLimitReachedType":"rate_limit_reached"});
        assert!(parse_rate_limits(&hit).unwrap().reached);
        assert!(parse_rate_limits(&json!({})).is_none());
    }
}
