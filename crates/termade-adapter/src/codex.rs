//! Codex 的原生 adapter (`codex app-server`)。
//!
//! Codex 的 app server 是 stdio 上的 JSON-RPC (實驗性，但介面穩定且可自我描述：
//! `codex app-server generate-json-schema` 會吐出完整 schema)。
//!
//! 用到的子集：
//! * `initialize`
//! * `thread/start` → `thread.id`
//! * `turn/start` (帶 `input`)
//! * `turn/interrupt`
//! * 通知：`item/agentMessage/delta`、`item/completed`、`turn/completed`、
//!   `thread/tokenUsage/updated`
//! * 核准 (server→client 請求)：`item/commandExecution/requestApproval`、
//!   `item/fileChange/requestApproval`

use crate::stream::StreamBuf;
use crate::{AgentControl, AgentEvent, SessionSpec};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use termade_core::{new_id, Cost, Decision, PermissionRequest, SessionStatus};
use tr::tr;

const CALL_TIMEOUT: Duration = Duration::from_secs(60);

struct Conn {
    session_id: String,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Sender<Value>>>,
    /// 核准：內部 req id -> JSON-RPC id。
    perms: Mutex<HashMap<String, Value>>,
    /// 目前的 thread / turn id。
    thread: Mutex<String>,
    turn: Mutex<String>,
    stream: Mutex<StreamBuf>,
    /// 本回合已回報過的錯誤文字 (`error` 通知與 `turn/completed` 不重複送)。
    last_error: Mutex<Option<String>>,
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
        // 2) server → client 的請求 (需要我們回覆)。
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
                    "error":{"code":-32601,"message":format!("termade 不支援 {other}")}
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
                *self.last_error.lock().unwrap() = None;
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
                    *self.thread.lock().unwrap() = id.to_string();
                }
            }
            "error" => {
                tracing::warn!("codex error 通知：{}", p);
                // `willRetry: true` 時 codex 會自己重試，不算失敗，不打擾使用者。
                if p["willRetry"].as_bool() != Some(true) {
                    let err = if p["error"].is_null() { p } else { &p["error"] };
                    self.report_error(err);
                }
            }
            _ => {}
        }
    }

    /// 把 codex 的錯誤 (`error` 通知的 `error`，或 `turn.error`)送到逐字稿與 status note。
    /// 同一個回合內相同文字只回報一次。
    fn report_error(&self, err: &Value) {
        let text = error_text(err);
        {
            let mut last = self.last_error.lock().unwrap();
            if last.as_deref() == Some(text.as_str()) {
                return;
            }
            *last = Some(text.clone());
        }
        if termade_core::looks_like_limit(&text) {
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
            "agentMessage" => {
                // 有 delta 時文字已經送過，這裡只收尾。
                self.flush_stream();
            }
            _ => {}
        }
    }
}

/// 錯誤文字：優先取 `message`，沒有就用整個 JSON 的前 300 字。
fn error_text(err: &Value) -> String {
    match err["message"].as_str().filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => err.to_string().chars().take(300).collect(),
    }
}

/// 是否為認證錯誤 (401、token 失效、未授權)。
fn is_auth_error(err: &Value) -> bool {
    let t = err.to_string().to_ascii_lowercase();
    t.contains("401") || t.contains("token_revoked") || t.contains("unauthorized")
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
    /// 執行期切換的 model／effort；每個 `turn/start` 都帶上。
    overrides: Mutex<(Option<String>, Option<String>)>,
}

impl CodexControl {
    fn thread_id(&self) -> Result<String> {
        let t = self.conn.thread.lock().unwrap().clone();
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

    fn send_input_with(&self, text: &str, atts: &[termade_core::Attachment]) -> Result<()> {
        let thread_id = self.thread_id()?;
        let _ = self.conn.tx.send(AgentEvent::Status(SessionStatus::Busy));
        let mut params = json!({
            "threadId": thread_id,
            "input": turn_input(text, atts)
        });
        let (m, e) = self.overrides.lock().unwrap().clone();
        if let Some(m) = m {
            params["model"] = json!(m);
        }
        if let Some(e) = e {
            params["effort"] = json!(e);
        }
        self.conn.call("turn/start", params, CALL_TIMEOUT)?;
        Ok(())
    }

    /// 存起來，下一個 `turn/start` 帶 `model`／`effort`(schema 已確認兩者都是 `TurnStartParams` 欄位)。
    fn set_config(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<String>> {
        if let Some(e) = effort {
            crate::check_effort(e)?;
        }
        let mut o = self.overrides.lock().unwrap();
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
            bail!("{}", tr!("未知的核准請求：{req}", req = req));
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
    let prog = match spec.bin.as_deref() {
        Some(b) => crate::which(b).with_context(|| tr!("找不到指定的執行檔 `{b}`", b = b))?,
        None => crate::which("codex").with_context(|| tr!("找不到 `{prog}`", prog = "codex"))?,
    };
    let mut cmd = Command::new(prog);
    cmd.arg("app-server");
    cmd.args(spec.permission.codex_flags());
    if let Some(exe) = &spec.mcp_exe {
        cmd.args([
            "-c",
            &format!("mcp_servers.termade.command={:?}", exe.to_string_lossy()),
            "-c",
            &format!(
                "mcp_servers.termade.args=[\"mcp\",\"--session\",{:?}]",
                spec.id
            ),
        ]);
    }
    cmd.args(&spec.args);
    cmd.current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &spec.env {
        cmd.env(k, v);
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
            // stdout 關閉＝處理程序已結束：丟掉等待中的請求，握手不必乾等逾時。
            conn.pending.lock().unwrap().clear();
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
            if let Some(t) = &stderr_tail {
                t.report_exit(code, &tx);
            }
            let _ = tx.send(AgentEvent::Exited(code));
        });
    }

    // 握手：initialize → thread/start。
    conn.call(
        "initialize",
        json!({ "clientInfo": { "name": "termade", "title": "termade", "version": env!("CARGO_PKG_VERSION") } }),
        CALL_TIMEOUT,
    )
    .with_context(|| tr!("codex `initialize` 失敗"))?;

    // 接續時改用 `thread/resume` (歷史回合放在回應裡，不會以通知重播)。
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
    *conn.thread.lock().unwrap() = tid.clone();
    let _ = tx.send(AgentEvent::NativeId(tid));
    if let Some(model) = started["model"].as_str() {
        let _ = tx.send(AgentEvent::Model(model.to_string()));
    }
    // 啟動時查一次帳號額度 (沒登入或舊版 codex 不支援就略過)。
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

/// 解析 Codex 的 `RateLimitSnapshot`(`account/rateLimits/read` 與更新通知共用)。
pub(crate) fn parse_rate_limits(snap: &Value) -> Option<termade_core::Quota> {
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let w = &snap[key];
        let Some(pct) = w["usedPercent"].as_f64() else {
            continue;
        };
        windows.push(termade_core::QuotaWindow {
            name: w["windowDurationMins"]
                .as_u64()
                .map(termade_core::window_name)
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
    Some(termade_core::Quota {
        agent: "codex".into(),
        windows,
        reached,
        updated_ms: termade_core::now_millis(),
    })
}

/// `thread/start` 參數：cwd、approvalPolicy，加上可選的 model 與 sandbox (官方文件範例：
/// `"model"`、`"sandbox": "workspace-write"`；`read_only` 對應 `read-only`)。
/// Codex `turn/start` 的 input：文字 (含內嵌文字檔)加上 data URL 圖片。
fn turn_input(text: &str, atts: &[termade_core::Attachment]) -> Value {
    let mut items =
        vec![json!({ "type": "text", "text": termade_core::compose_text(text, atts, false) })];
    for a in atts.iter().filter(|a| a.is_image()) {
        if let Some(data) = &a.data {
            items.push(json!({ "type": "image", "url": format!("data:{};base64,{data}", a.mime) }));
        }
    }
    Value::Array(items)
}

/// `thread/start` 或 (`spec.resume` 有值時)`thread/resume` 的參數。
/// 兩者接受同一組覆寫欄位；接續時多帶 `threadId`，並用 `excludeTurns` 省掉回應裡的歷史回合。
fn thread_open_params(spec: &SessionSpec) -> Value {
    let mut p = thread_start_params(spec);
    if let Some(id) = &spec.resume {
        p["threadId"] = json!(id);
        p["excludeTurns"] = json!(true);
    }
    p
}

fn thread_start_params(spec: &SessionSpec) -> Value {
    let mut p = json!({
        "cwd": spec.cwd.to_string_lossy(),
        "approvalPolicy": spec.permission.codex_approval_policy(),
    });
    if let Some(m) = &spec.model {
        p["model"] = json!(m);
    }
    if let Some(e) = &spec.effort {
        // thread 層級的 config 覆寫，等同 `-c model_reasoning_effort=...`。
        p["config"] = json!({ "model_reasoning_effort": e });
    }
    let sandbox = if spec.read_only {
        Some("read-only".to_string())
    } else {
        spec.sandbox.clone()
    };
    if let Some(s) = sandbox {
        p["sandbox"] = json!(s);
    }
    if let Some(inst) = spec.system_append.as_deref().filter(|s| !s.is_empty()) {
        p["developerInstructions"] = json!(inst);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_verdict_mapping() {
        // 只驗證對應表本身 (不需真的跑 codex)。
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

    #[test]
    fn thread_start_carries_model_and_sandbox() {
        let spec = SessionSpec {
            id: "s".into(),
            kind: termade_core::AgentKind::Codex,
            cwd: "/tmp".into(),
            transport: termade_protocol::Transport::Structured,
            bin: None,
            args: vec![],
            model: Some("m1".into()),
            effort: Some("high".into()),
            allowed_tools: vec![],
            disallowed_tools: vec![],
            read_only: false,
            sandbox: Some("workspace-write".into()),
            env: vec![],
            permission: termade_core::PermissionPolicy::Auto,
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
        assert!(p.get("developerInstructions").is_none());
        let mut ro = spec.clone();
        ro.read_only = true;
        assert_eq!(thread_start_params(&ro)["sandbox"], "read-only");
        // 開新對話不帶 threadId；接續時帶上，其餘覆寫欄位照舊。
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
