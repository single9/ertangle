//! Native adapter for Claude Code (`--input-format stream-json --output-format stream-json`).
//!
//! This is bidirectional NDJSON: we write user messages to stdin, and Claude emits events on stdout.
//! Since Claude Code does not provide native ACP, this is its only structured interface with zero extra dependencies.
//!
//! Observed wire shape (claude 2.1.x):
//!
//! ```text
//! → {"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}
//! ← {"type":"system","subtype":"init","session_id":...,"model":...,"tools":[...]}
//! ← {"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"..."}}}
//! ← {"type":"assistant","message":{"content":[{"type":"text","text":...},{"type":"tool_use","name":...,"input":...}]}}
//! ← {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":...,"content":...,"is_error":false}]}}
//! ← {"type":"result","subtype":"success","total_cost_usd":...,"usage":{...},"is_error":false}
//! ```

use crate::stream::StreamBuf;
use crate::{AgentControl, AgentEvent, SessionSpec};
use anyhow::{bail, Context, Result};
use ertangle_core::{Cost, Decision, SessionStatus};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tr::tr;

/// Reads the running Claude event stream.
struct Conn {
    stdin: Mutex<ChildStdin>,
    stream: Mutex<StreamBuf>,
    /// Whether this message has already streamed via deltas (avoids duplicate output with the full assistant message).
    saw_delta: AtomicBool,
    /// Set by `cancel()`: the next `error_during_execution` result is the user's interrupt, not a failure.
    interrupted: AtomicBool,
    tx: Sender<AgentEvent>,
}

impl Conn {
    fn emit(&self, ev: AgentEvent) {
        // 寫入失敗代表接收端通道已關閉，無需處理
        let _ = self.tx.send(ev);
    }

    fn send_raw(&self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let mut w = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(line.as_bytes())?;
        w.flush()?;
        Ok(())
    }

    fn flush_stream(&self) {
        let text = self
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .flush();
        if !text.is_empty() {
            self.emit(AgentEvent::Output(text));
        }
    }

    /// Processes a single stdout line.
    fn on_line(&self, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match msg.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "system" => self.on_system(&msg),
            "stream_event" => self.on_stream_event(&msg),
            "assistant" => self.on_assistant(&msg),
            "user" => self.on_tool_results(&msg),
            "result" => self.on_result(&msg),
            "rate_limit_event" => {
                if let Some(q) = parse_rate_limit_event(&msg) {
                    self.emit(AgentEvent::Quota(q));
                }
            }
            // hook_started / hook_response / rate_limit_event / system.status…
            _ => {}
        }
    }

    fn on_system(&self, msg: &Value) {
        if msg["subtype"].as_str() == Some("init") {
            if let Some(model) = msg["model"].as_str() {
                self.emit(AgentEvent::Model(model.to_string()));
            }
            if let Some(cmds) = msg["slash_commands"].as_array() {
                let names: Vec<String> = cmds
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect();
                if !names.is_empty() {
                    self.emit(AgentEvent::Commands(names));
                }
            }
            // Send every time: after resuming the ID may change; the latest takes precedence.
            if let Some(id) = msg["session_id"].as_str().filter(|s| !s.is_empty()) {
                self.emit(AgentEvent::NativeId(id.to_string()));
            }
            // Status is set to Idle by the daemon upon startup completion and restored to Idle by `result` at turn end;
            // do not emit Idle here, otherwise the UI would appear idle while the initial prompt is still being processed.
        }
    }

    /// Word-by-word streaming (requires `--include-partial-messages`).
    fn on_stream_event(&self, msg: &Value) {
        let ev = &msg["event"];
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.saw_delta.store(false, Ordering::SeqCst);
            }
            "content_block_delta" => {
                let delta = &ev["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(t) = delta["text"].as_str() {
                            self.saw_delta.store(true, Ordering::SeqCst);
                            let out = self
                                .stream
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push("msg", t);
                            if !out.is_empty() {
                                self.emit(AgentEvent::Output(out));
                            }
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta["thinking"].as_str() {
                            let out = self
                                .stream
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push("think", t);
                            if !out.is_empty() {
                                self.emit(AgentEvent::Output(out));
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Finish text output when a tool call starts so the order is correct.
            "content_block_start"
                if ev
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("tool_use") =>
            {
                self.flush_stream();
            }
            _ => {}
        }
    }

    /// Full assistant message: attaches tool calls and text (when partial messages are disabled).
    fn on_assistant(&self, msg: &Value) {
        let content = msg
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        for block in &content {
            match block["type"].as_str().unwrap_or("") {
                "text" => {
                    // Do not output again if already streamed via delta.
                    if self.saw_delta.load(Ordering::SeqCst) {
                        self.flush_stream();
                    } else {
                        let text = block["text"].as_str().unwrap_or("");
                        if !text.is_empty() {
                            self.emit(AgentEvent::Output(format!("{text}\n")));
                        }
                    }
                }
                "tool_use" => {
                    self.flush_stream();
                    let name = block["name"].as_str().unwrap_or("tool").to_string();
                    let input = block["input"].to_string();
                    self.emit(AgentEvent::ToolCall {
                        name,
                        input,
                        ok: None,
                        output: None,
                    });
                }
                "thinking" => {}
                _ => {}
            }
        }
        // Context usage = input tokens for this assistant message (including cache); window size is only known at `result`.
        let u = msg.get("message").and_then(|m| m.get("usage"));
        if let Some(u) = u {
            if !u.is_null() {
                let used = u
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .saturating_add(
                        u.get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                    )
                    .saturating_add(
                        u.get("cache_read_input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                    );
                if used > 0 {
                    self.emit(AgentEvent::Context { used, window: None });
                }
            }
        }
        // Do not emit per-message usage here: it only has tokens for a single message, which would overwrite
        // cumulative totals with smaller numbers causing costs and tokens to jump backward mid-turn. Cumulative values rely on `result`.
    }

    /// Tool result (Claude reports this via a `user` message).
    fn on_tool_results(&self, msg: &Value) {
        let content = msg
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        for block in &content {
            if block["type"].as_str() != Some("tool_result") {
                continue;
            }
            let ok = !block["is_error"].as_bool().unwrap_or(false);
            let output = match &block["content"] {
                Value::String(s) => s.clone(),
                // MCP tool returns [{"type":"text","text":...}]: extract text, do not show JSON envelope.
                Value::Array(parts) if parts.iter().all(|p| p["text"].is_string()) => parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            };
            self.emit(AgentEvent::ToolCall {
                name: String::new(),
                input: String::new(),
                ok: Some(ok),
                output: Some(truncate(&output, 4000)),
            });
        }
    }

    fn on_result(&self, msg: &Value) {
        self.flush_stream();
        if let Some(window) = msg["modelUsage"]
            .as_object()
            .and_then(|m| m.values().filter_map(|v| v["contextWindow"].as_u64()).max())
        {
            // Reuse usage from the latest assistant message; here only fill in the window size.
            self.emit(AgentEvent::Context {
                used: 0,
                window: Some(window),
            });
        }
        if let Some(cost) = result_cost(msg) {
            self.emit(AgentEvent::Cost(cost));
        }
        let was_interrupted = self.interrupted.swap(false, Ordering::SeqCst);
        if is_interrupt_result(msg, was_interrupted) {
            // A user interrupt makes Claude report an errored result; the session itself is still healthy.
            self.emit(AgentEvent::Status(SessionStatus::Idle));
        } else if msg["is_error"].as_bool().unwrap_or(false) {
            let detail = msg["result"].as_str().unwrap_or("").to_string();
            if ertangle_core::looks_like_limit(&detail) {
                self.emit(AgentEvent::Limited(detail));
            } else if !detail.trim().is_empty() {
                let note = tr!("claude 錯誤：{detail}", detail = detail.trim());
                self.emit(AgentEvent::Output(format!("⚠ {note}\n")));
                self.emit(AgentEvent::Note(note));
            }
            self.emit(AgentEvent::Status(SessionStatus::Failed));
        } else {
            self.emit(AgentEvent::Status(SessionStatus::Idle));
        }
    }
}

/// Whether `msg` is the errored `result` Claude emits after an interrupt we requested.
fn is_interrupt_result(msg: &Value, interrupted: bool) -> bool {
    interrupted
        && msg["is_error"].as_bool().unwrap_or(false)
        && msg["subtype"].as_str() == Some("error_during_execution")
}

/// Parses `rate_limit_event`: each window in `unifiedWindows` has `utilization` (0..1) and `resetsAt` (seconds).
pub(crate) fn parse_rate_limit_event(msg: &Value) -> Option<ertangle_core::Quota> {
    let info = &msg["rate_limit_info"];
    let mut windows: Vec<ertangle_core::QuotaWindow> = Vec::new();
    // `allowed_warning` (approaching limit warning) is still usable; only non-`allowed*` statuses like `rejected` count as exhausted.
    let blocked = |v: &Value| v.as_str().is_some_and(|s| !s.starts_with("allowed"));
    let mut reached = blocked(&info["status"]);
    let warns = |v: &Value| v.as_str() == Some("allowed_warning");
    let mut warning = warns(&info["status"]);
    if let Some(map) = info["unifiedWindows"].as_object() {
        for (key, w) in map {
            let name = match key.as_str() {
                "five_hour" => "5h".to_string(),
                "seven_day" => "7d".to_string(),
                other => other.to_string(),
            };
            let used = w["utilization"].as_f64()? as f32 * 100.0;
            if blocked(&w["status"]) {
                reached = true;
            }
            warning |= warns(&w["status"]);
            windows.push(ertangle_core::QuotaWindow {
                name,
                used_percent: used,
                resets_at_ms: w["resetsAt"].as_u64().map(|s| s.saturating_mul(1000)),
            });
        }
    }
    if windows.is_empty() {
        return None;
    }
    windows.sort_by(|a, b| a.name.cmp(&b.name));
    Some(ertangle_core::Quota {
        agent: "claude".into(),
        windows,
        reached,
        warning,
        updated_ms: ertangle_core::now_millis(),
    })
}

/// Estimates cost from Anthropic's usage block (Claude only provides totals in `result`).
fn result_cost(msg: &Value) -> Option<Cost> {
    let usd = msg["total_cost_usd"].as_f64()?;
    let usage = &msg["usage"];
    Some(Cost {
        usd,
        input_tokens: usage["input_tokens"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(usage["cache_read_input_tokens"].as_u64().unwrap_or(0)),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Control handle
// ---------------------------------------------------------------------------

/// Control message for switching models.
fn set_model_request(model: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": format!("ertangle-model-{}", std::process::id()),
        "request": { "subtype": "set_model", "model": model }
    })
}

struct ClaudeControl {
    conn: Arc<Conn>,
    child: Arc<Mutex<Child>>,
}

impl AgentControl for ClaudeControl {
    fn send_input(&self, text: &str) -> Result<()> {
        self.send_input_with(text, &[])
    }

    fn send_input_with(&self, text: &str, atts: &[ertangle_core::Attachment]) -> Result<()> {
        self.conn.emit(AgentEvent::Status(SessionStatus::Busy));
        self.conn.send_raw(&json!({
            "type": "user",
            "message": { "role": "user", "content": user_content(text, atts) }
        }))
    }

    /// Sends an interrupt control message (best-effort; Claude has no public cancellation API).
    fn cancel(&self) -> Result<()> {
        self.conn.flush_stream();
        self.conn.interrupted.store(true, Ordering::SeqCst);
        self.conn.send_raw(&json!({
            "type": "control_request",
            "request_id": format!("ertangle-cancel-{}", std::process::id()),
            "request": { "subtype": "interrupt" }
        }))
    }

    fn pid(&self) -> Option<u32> {
        Some(self.child.lock().unwrap_or_else(|e| e.into_inner()).id())
    }

    fn kill(&self) -> Result<()> {
        let pid = self.child.lock().unwrap_or_else(|e| e.into_inner()).id();
        // 終止處理程序群組或處理程序，忽略訊號發送錯誤
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

    /// Claude's per-item permissions are handled by the daemon via `--permission-prompt-tool` (MCP `ertangle_approve`).
    fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
        bail!(
            "{}",
            tr!("claude 逐項核准已由 MCP (ertangle_approve) 轉發，此請求無效或已逾時")
        )
    }

    fn transport(&self) -> &'static str {
        "stream-json"
    }

    /// `control_request` for `set_model` (verified working) and `apply_flag_settings` (effortLevel).
    fn set_config(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<String>> {
        if let Some(e) = effort {
            crate::check_effort(e)?;
        }
        if let Some(m) = model {
            self.conn.send_raw(&set_model_request(m))?;
        }
        if let Some(e) = effort {
            self.conn.send_raw(&json!({
                "type": "control_request",
                "request_id": format!("ertangle-effort-{}", std::process::id()),
                "request": { "subtype": "apply_flag_settings", "settings": { "effortLevel": e } }
            }))?;
        }
        Ok(model.map(str::to_string))
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Claude's user content: text (including inlined text files) plus base64 image blocks.
fn user_content(text: &str, atts: &[ertangle_core::Attachment]) -> Value {
    let mut blocks =
        vec![json!({ "type": "text", "text": ertangle_core::compose_text(text, atts, false) })];
    for a in atts.iter().filter(|a| a.is_image()) {
        if let Some(data) = &a.data {
            blocks.push(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": a.mime, "data": data }
            }));
        }
    }
    Value::Array(blocks)
}

/// Command-line arguments related to permissions and system prompt (read-only, allowed/disallowed tools, fixed ertangle MCP tools, appended system prompt).
pub fn permission_args(spec: &SessionSpec) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(e) = &spec.effort {
        out.extend(["--effort".to_string(), e.clone()]);
    }
    if let Some(s) = spec.system_append.as_deref().filter(|s| !s.is_empty()) {
        out.extend(["--append-system-prompt".to_string(), s.to_string()]);
    }
    if spec.read_only {
        out.extend(["--permission-mode".into(), "plan".into()]);
    } else {
        out.extend(spec.permission.claude_flags().into_iter().map(String::from));
    }
    // When not bypass, tool calls requiring approval are deferred to ertangle (rule engine / user).
    if spec.mcp_exe.is_some() && spec.permission != ertangle_core::PermissionPolicy::Bypass {
        out.extend([
            "--permission-prompt-tool".into(),
            "mcp__ertangle__ertangle_approve".into(),
            "--permission-prompts".into(),
            "host".into(),
        ]);
    }
    let mut allowed = spec.allowed_tools.clone();
    if spec.mcp_exe.is_some() && spec.permission.auto_approves() {
        allowed.push("mcp__ertangle__*".into());
    }
    if !allowed.is_empty() {
        out.push(format!("--allowedTools={}", allowed.join(",")));
    }
    if !spec.disallowed_tools.is_empty() {
        out.push(format!(
            "--disallowedTools={}",
            spec.disallowed_tools.join(",")
        ));
    }
    out
}

/// Fixed stream-json arguments plus model and resume (`--resume <id>`: reconnects to original conversation; verified ID does not change).
fn base_args(spec: &SessionSpec) -> Vec<String> {
    let mut out: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if let Some(m) = &spec.model {
        out.extend(["--model".to_string(), m.clone()]);
    }
    if let Some(id) = &spec.resume {
        out.extend(["--resume".to_string(), id.clone()]);
    }
    out
}

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let prog = match spec.bin.as_deref() {
        Some(b) => crate::which(b).with_context(|| tr!("找不到指定的執行檔 `{b}`", b = b))?,
        None => crate::agent_bin("claude", "ERTANGLE_CLAUDE_BIN")
            .with_context(|| tr!("找不到 `{prog}`", prog = "claude"))?,
    };
    let mut cmd = Command::new(prog);
    cmd.args(base_args(&spec));
    cmd.args(permission_args(&spec));
    if let Some(exe) = &spec.mcp_exe {
        let mut args = vec!["mcp".to_string(), "--session".to_string(), spec.id.clone()];
        if let Some((_, sock)) = spec.env.iter().find(|(k, _)| k == "ERTANGLE_SOCKET") {
            args.push("--socket".to_string());
            args.push(sock.clone());
        }
        let mcp_cfg = json!({
            "mcpServers": {
                "ertangle": {
                    "command": exe.to_string_lossy(),
                    "args": args
                }
            }
        });
        cmd.arg(format!("--mcp-config={mcp_cfg}"));
    }
    cmd.args(&spec.args);
    cmd.current_dir(&spec.cwd)
        .env_remove("CLAUDECODE")
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

    let mut child = cmd.spawn().with_context(|| tr!("無法啟動 `claude`"))?;
    let stdin = child
        .stdin
        .take()
        .with_context(|| tr!("取得 claude stdin 失敗"))?;
    let stderr_tail = child.stderr.take().map(crate::stderr::StderrTail::spawn);
    let stdout = child
        .stdout
        .take()
        .with_context(|| tr!("取得 claude stdout 失敗"))?;

    let conn = Arc::new(Conn {
        stdin: Mutex::new(stdin),
        stream: Mutex::new(StreamBuf::default()),
        interrupted: AtomicBool::new(false),
        saw_delta: AtomicBool::new(false),
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
            // Poll with try_wait: do not hold lock during blocking wait, otherwise kill() will deadlock.
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
            // 接收端若已關閉則忽略退出事件
            let _ = tx.send(AgentEvent::Exited(code));
        });
    }

    let control = ClaudeControl { conn, child };
    if let Some(p) = &spec.initial_prompt {
        control.send_input_with(p, &spec.initial_attachments)?;
    }
    Ok(Box::new(control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_result_cost() {
        let msg = json!({
            "type":"result","subtype":"success","is_error":false,
            "total_cost_usd":0.5,
            "usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":20}
        });
        let c = result_cost(&msg).unwrap();
        assert_eq!(c.input_tokens, 15);
        assert_eq!(c.output_tokens, 20);
        assert!((c.usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn interrupt_result_is_not_a_failure() {
        let err = json!({"type":"result","subtype":"error_during_execution","is_error":true});
        assert!(is_interrupt_result(&err, true));
        assert!(!is_interrupt_result(&err, false));
        let ok = json!({"type":"result","subtype":"success","is_error":false});
        assert!(!is_interrupt_result(&ok, true));
        let other = json!({"type":"result","subtype":"error_max_turns","is_error":true});
        assert!(!is_interrupt_result(&other, true));
    }

    fn spec_for_args() -> SessionSpec {
        SessionSpec {
            id: "s".into(),
            kind: ertangle_core::AgentKind::ClaudeCode,
            cwd: std::env::temp_dir(),
            transport: ertangle_protocol::Transport::Structured,
            bin: None,
            args: vec![],
            model: None,
            effort: None,
            allowed_tools: vec!["Read".into(), "Grep".into()],
            disallowed_tools: vec!["Bash".into()],
            read_only: false,
            sandbox: None,
            env: vec![],
            permission: ertangle_core::PermissionPolicy::Auto,
            mcp_exe: Some("/bin/ertangle".into()),
            system_append: None,
            initial_prompt: None,
            initial_attachments: vec![],
            resume: None,
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn permission_args_merge_allowed_tools_with_mcp() {
        let a = permission_args(&spec_for_args());
        assert!(
            a.contains(&"--allowedTools=Read,Grep,mcp__ertangle__*".to_string()),
            "{a:?}"
        );
        assert!(a.contains(&"--disallowedTools=Bash".to_string()));
        let mut s = spec_for_args();
        s.read_only = true;
        let a = permission_args(&s);
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
    }

    #[test]
    fn user_content_adds_image_block_and_embeds_text_files() {
        let atts = vec![
            ertangle_core::Attachment {
                name: "p.png".into(),
                mime: "image/png".into(),
                data: Some("QUJD".into()),
                ..Default::default()
            },
            ertangle_core::Attachment {
                name: "n.txt".into(),
                mime: "text/plain".into(),
                text: Some("hi".into()),
                ..Default::default()
            },
        ];
        let c = user_content("看", &atts);
        let arr = c.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0]["text"].as_str().unwrap().contains("hi"));
        assert_eq!(arr[1]["type"], "image");
        assert_eq!(arr[1]["source"]["media_type"], "image/png");
        assert_eq!(arr[1]["source"]["data"], "QUJD");
    }

    #[test]
    fn parses_real_rate_limit_event() {
        let msg: Value = serde_json::from_str(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1794543000,"rateLimitType":"five_hour","unifiedWindows":{"five_hour":{"utilization":0.39,"resetsAt":1794543000,"status":"allowed"},"seven_day":{"utilization":0.22,"resetsAt":1795042800,"status":"allowed"}}}}"#,
        )
        .unwrap();
        let q = parse_rate_limit_event(&msg).unwrap();
        assert_eq!(q.agent, "claude");
        assert_eq!(q.windows.len(), 2);
        let h5 = q.windows.iter().find(|w| w.name == "5h").unwrap();
        assert!((h5.used_percent - 39.0).abs() < 0.01);
        assert_eq!(h5.resets_at_ms, Some(1794543000 * 1000));
        assert!(!q.reached);
        let blocked = serde_json::json!({"rate_limit_info":{"status":"rejected","unifiedWindows":{"five_hour":{"utilization":1.0,"resetsAt":1,"status":"rejected"}}}});
        assert!(parse_rate_limit_event(&blocked).unwrap().reached);
        let warning = serde_json::json!({"rate_limit_info":{"status":"allowed_warning","unifiedWindows":{"seven_day":{"utilization":0.76,"resetsAt":1,"status":"allowed_warning"}}}});
        let wq = parse_rate_limit_event(&warning).unwrap();
        assert!(!wq.reached && wq.warning);
        assert!(!q.warning);
    }

    #[test]
    fn resume_flag_added_only_when_resuming() {
        let mut s = spec_for_args();
        assert!(!base_args(&s).iter().any(|a| a == "--resume"));
        s.resume = Some("abc-123".into());
        let a = base_args(&s);
        assert!(a.windows(2).any(|w| w == ["--resume", "abc-123"]), "{a:?}");
    }

    #[test]
    fn effort_flag_added() {
        let mut s = spec_for_args();
        s.effort = Some("high".into());
        let a = permission_args(&s);
        assert!(a.windows(2).any(|w| w == ["--effort", "high"]), "{a:?}");
    }

    #[test]
    fn system_append_flag_added_only_when_present() {
        let s = spec_for_args();
        let a = permission_args(&s);
        assert!(!a.iter().any(|x| x == "--append-system-prompt"), "{a:?}");

        let mut s2 = spec_for_args();
        s2.system_append = Some("collab text".into());
        let a2 = permission_args(&s2);
        assert!(
            a2.windows(2)
                .any(|w| w == ["--append-system-prompt", "collab text"]),
            "{a2:?}"
        );
    }

    #[test]
    fn prompt_tool_flag_added_unless_bypass() {
        let a = permission_args(&spec_for_args());
        assert!(a.windows(2).any(|w| w
            == [
                "--permission-prompt-tool",
                "mcp__ertangle__ertangle_approve"
            ]));
        assert!(a.windows(2).any(|w| w == ["--permission-prompts", "host"]));
        let mut s = spec_for_args();
        s.permission = ertangle_core::PermissionPolicy::Bypass;
        assert!(!permission_args(&s)
            .iter()
            .any(|x| x.contains("permission-prompt-tool")));
        assert!(!permission_args(&s)
            .iter()
            .any(|x| x.contains("permission-prompts")));
    }

    #[test]
    fn truncates_long_tool_output() {
        let s = "x".repeat(10);
        assert_eq!(truncate(&s, 4), "xxxx…");
        assert_eq!(truncate("abc", 4), "abc");
    }

    #[test]
    fn set_model_request_shape() {
        let v = set_model_request("opus");
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request"]["subtype"], "set_model");
        assert_eq!(v["request"]["model"], "opus");
    }
}
