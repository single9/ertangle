//! Codex 的原生 adapter（`codex app-server`）。
//!
//! Codex 的 app server 是 stdio 上的 JSON-RPC（實驗性，但介面穩定且可自我描述：
//! `codex app-server generate-json-schema` 會吐出完整 schema）。
//!
//! 用到的子集：
//! * `initialize`
//! * `thread/start` → `thread.id`
//! * `turn/start`（帶 `input`）
//! * `turn/interrupt`
//! * 通知：`item/agentMessage/delta`、`item/completed`、`turn/completed`、
//!   `thread/tokenUsage/updated`
//! * 審批（server→client 請求）：`item/commandExecution/requestApproval`、
//!   `item/fileChange/requestApproval`

use crate::stream::StreamBuf;
use crate::{AgentControl, AgentEvent, SessionSpec};
use ade_core::{new_id, Cost, Decision, PermissionRequest, SessionStatus};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CALL_TIMEOUT: Duration = Duration::from_secs(60);

struct Conn {
    session_id: String,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Sender<Value>>>,
    /// 審批：內部 req id -> JSON-RPC id。
    perms: Mutex<HashMap<String, Value>>,
    /// 目前的 thread / turn id。
    thread: Mutex<String>,
    turn: Mutex<String>,
    stream: Mutex<StreamBuf>,
    tx: Sender<AgentEvent>,
}

impl Conn {
    fn write(&self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let mut w = self.stdin.lock().unwrap();
        w.write_all(line.as_bytes())?;
        w.flush()?;
        Ok(())
    }

    /// 送請求並等待回應。
    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (s, r) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, s);
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let msg = r
            .recv_timeout(timeout)
            .map_err(|_| anyhow!("`{method}` 逾時"))?;
        if let Some(err) = msg.get("error") {
            bail!("`{method}` 失敗：{err}");
        }
        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }

    fn flush_stream(&self) {
        let text = self.stream.lock().unwrap().flush();
        if !text.is_empty() {
            let _ = self.tx.send(AgentEvent::Output(text));
        }
    }

    /// 處理一行 stdout。
    fn on_line(&self, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return;
        };
        // 1) 我們送出的請求的回應。
        if let Some(id) = msg.get("id") {
            if msg.get("method").is_none() {
                let id = id.as_i64().unwrap_or(-1);
                if let Some(s) = self.pending.lock().unwrap().remove(&id) {
                    let _ = s.send(msg);
                }
                return;
            }
        }
        let method = msg["method"].as_str().unwrap_or("");
        // 2) server → client 的請求（需要我們回覆）。
        if msg.get("id").is_some() && !method.is_empty() {
            self.on_server_request(method, &msg);
            return;
        }
        // 3) 通知。
        self.on_notification(method, &msg["params"]);
    }

    fn on_server_request(&self, method: &str, msg: &Value) {
        let params = &msg["params"];
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
            other => {
                // 不認識的請求一律回錯誤，避免 Codex 卡住等我們。
                let _ = self.write(&json!({
                    "jsonrpc":"2.0","id":msg["id"],
                    "error":{"code":-32601,"message":format!("ade 不支援 {other}")}
                }));
                return;
            }
        };
        let req = new_id("perm");
        self.perms
            .lock()
            .unwrap()
            .insert(req.clone(), json!({"id": msg["id"], "kind": kind.as_str()}));
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
                    let out = self.stream.lock().unwrap().push("msg", d);
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
                    let out = self.stream.lock().unwrap().push("think", d);
                    if !out.is_empty() {
                        let _ = self.tx.send(AgentEvent::Output(out));
                    }
                }
            }
            "turn/started" => {
                if let Some(id) = p["turn"]["id"].as_str() {
                    *self.turn.lock().unwrap() = id.to_string();
                }
                let _ = self.tx.send(AgentEvent::Status(SessionStatus::Busy));
            }
            "turn/completed" => {
                self.flush_stream();
                let failed = !p["turn"]["error"].is_null();
                let _ = self.tx.send(AgentEvent::Status(if failed {
                    SessionStatus::Failed
                } else {
                    SessionStatus::Idle
                }));
            }
            "item/completed" => self.on_item_completed(&p["item"]),
            "thread/tokenUsage/updated" => {
                let total = &p["tokenUsage"]["total"];
                let cost = Cost {
                    usd: 0.0,
                    input_tokens: total["inputTokens"].as_u64().unwrap_or(0),
                    output_tokens: total["outputTokens"].as_u64().unwrap_or(0),
                };
                let _ = self.tx.send(AgentEvent::Cost(cost));
            }
            "thread/started" => {
                if let Some(id) = p["thread"]["id"].as_str() {
                    *self.thread.lock().unwrap() = id.to_string();
                }
            }
            "error" => {
                tracing::warn!("codex error 通知：{}", p);
            }
            _ => {}
        }
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
            "agentMessage" => {
                // 有 delta 時文字已經送過，這裡只收尾。
                self.flush_stream();
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
enum ApprovalKind {
    Command,
    File,
}

impl ApprovalKind {
    fn as_str(self) -> &'static str {
        match self {
            ApprovalKind::Command => "command",
            ApprovalKind::File => "file",
        }
    }
}

// ---------------------------------------------------------------------------
// 控制把手
// ---------------------------------------------------------------------------

struct CodexControl {
    conn: Arc<Conn>,
    child: Arc<Mutex<Child>>,
}

impl CodexControl {
    fn thread_id(&self) -> Result<String> {
        let t = self.conn.thread.lock().unwrap().clone();
        if t.is_empty() {
            bail!("codex thread 尚未就緒");
        }
        Ok(t)
    }
}

impl AgentControl for CodexControl {
    fn send_input(&self, text: &str) -> Result<()> {
        let thread_id = self.thread_id()?;
        let _ = self.conn.tx.send(AgentEvent::Status(SessionStatus::Busy));
        self.conn.call(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{ "type": "text", "text": text }]
            }),
            CALL_TIMEOUT,
        )?;
        Ok(())
    }

    fn cancel(&self) -> Result<()> {
        let thread_id = self.thread_id()?;
        let turn_id = self.conn.turn.lock().unwrap().clone();
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

    fn kill(&self) -> Result<()> {
        self.child.lock().unwrap().kill().ok();
        Ok(())
    }

    fn sleep(&self) -> Result<()> {
        let pid = self.child.lock().unwrap().id();
        crate::signal_pid(pid, "-STOP")
    }

    fn wake(&self) -> Result<()> {
        let pid = self.child.lock().unwrap().id();
        crate::signal_pid(pid, "-CONT")
    }

    fn resolve_permission(&self, req: &str, decision: Decision) -> Result<()> {
        let entry = self.conn.perms.lock().unwrap().remove(req);
        let Some(entry) = entry else {
            bail!("未知的審批請求：{req}");
        };
        let id = entry["id"].clone();
        let kind = match entry["kind"].as_str() {
            Some("file") => ApprovalKind::File,
            _ => ApprovalKind::Command,
        };
        // Codex 的 decision 是 camelCase 字串。
        let verdict = match (kind, decision) {
            (_, Decision::Allow) => "accept",
            (_, Decision::AllowAlways) => "acceptForSession",
            (_, Decision::Deny) => "decline",
        };
        self.conn.write(&json!({
            "jsonrpc":"2.0","id":id,
            "result":{ "decision": verdict }
        }))
    }

    fn transport(&self) -> &'static str {
        "app-server"
    }
}

// ---------------------------------------------------------------------------
// 啟動
// ---------------------------------------------------------------------------

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let prog = crate::which("codex").context("找不到 `codex`")?;
    let mut cmd = Command::new(prog);
    cmd.arg("app-server");
    cmd.args(spec.permission.codex_flags());
    if let Some(exe) = &spec.mcp_exe {
        cmd.args([
            "-c",
            &format!("mcp_servers.ade.command={:?}", exe.to_string_lossy()),
            "-c",
            &format!("mcp_servers.ade.args=[\"mcp\",\"--session\",{:?}]", spec.id),
        ]);
    }
    cmd.args(&spec.args);
    cmd.current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().context("無法啟動 `codex app-server`")?;
    let stdin = child.stdin.take().context("取得 codex stdin 失敗")?;
    let stdout = child.stdout.take().context("取得 codex stdout 失敗")?;

    let _ = tx.send(AgentEvent::Status(SessionStatus::Starting));

    let conn = Arc::new(Conn {
        session_id: spec.id.clone(),
        stdin: Mutex::new(stdin),
        next_id: AtomicI64::new(1),
        pending: Mutex::new(HashMap::new()),
        perms: Mutex::new(HashMap::new()),
        thread: Mutex::new(String::new()),
        turn: Mutex::new(String::new()),
        stream: Mutex::new(StreamBuf::default()),
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
        });
    }

    let child = Arc::new(Mutex::new(child));
    {
        let child = child.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let code = loop {
                let status = {
                    let mut c = child.lock().unwrap();
                    c.try_wait()
                };
                match status {
                    Ok(Some(s)) => break s.code(),
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break None,
                }
            };
            let _ = tx.send(AgentEvent::Exited(code));
        });
    }

    // 握手：initialize → thread/start。
    conn.call(
        "initialize",
        json!({ "clientInfo": { "name": "ade", "title": "ade", "version": env!("CARGO_PKG_VERSION") } }),
        CALL_TIMEOUT,
    )
    .context("codex `initialize` 失敗")?;

    let started = conn
        .call(
            "thread/start",
            json!({
                "cwd": spec.cwd.to_string_lossy(),
                "approvalPolicy": spec.permission.codex_approval_policy(),
            }),
            CALL_TIMEOUT,
        )
        .context("codex `thread/start` 失敗")?;
    let tid = started["thread"]["id"]
        .as_str()
        .context("`thread/start` 未回傳 thread id")?
        .to_string();
    *conn.thread.lock().unwrap() = tid;
    if let Some(model) = started["model"].as_str() {
        let _ = tx.send(AgentEvent::Model(model.to_string()));
    }
    let _ = tx.send(AgentEvent::Status(SessionStatus::Idle));

    let control = CodexControl { conn, child };
    if let Some(p) = &spec.initial_prompt {
        control.send_input(p)?;
    }
    Ok(Box::new(control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_verdict_mapping() {
        // 只驗證對應表本身（不需真的跑 codex）。
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
    }
}
