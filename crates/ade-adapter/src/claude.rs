//! Claude Code 的原生 adapter（`--input-format stream-json --output-format stream-json`）。
//!
//! 這是雙向 NDJSON：我們往 stdin 寫「使用者訊息」，Claude 往 stdout 吐事件。
//! 因為 Claude Code 沒有原生 ACP，這是它唯一「零額外依賴」的結構化介面。
//!
//! 觀察到的 wire 形狀（claude 2.1.x）：
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
use ade_core::{Cost, Decision, SessionStatus};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 讀取執行中的 Claude 事件流。
struct Conn {
    stdin: Mutex<ChildStdin>,
    stream: Mutex<StreamBuf>,
    /// 這一則訊息是否已經用 delta 串流過（避免與完整 assistant 訊息重複）。
    saw_delta: AtomicBool,
    tx: Sender<AgentEvent>,
}

impl Conn {
    fn send_raw(&self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let mut w = self.stdin.lock().unwrap();
        w.write_all(line.as_bytes())?;
        w.flush()?;
        Ok(())
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
        match msg["type"].as_str().unwrap_or("") {
            "system" => self.on_system(&msg),
            "stream_event" => self.on_stream_event(&msg),
            "assistant" => self.on_assistant(&msg),
            "user" => self.on_tool_results(&msg),
            "result" => self.on_result(&msg),
            // hook_started / hook_response / rate_limit_event / system.status…
            _ => {}
        }
    }

    fn on_system(&self, msg: &Value) {
        if msg["subtype"].as_str() == Some("init") {
            if let Some(model) = msg["model"].as_str() {
                let _ = self.tx.send(AgentEvent::Model(model.to_string()));
            }
            let _ = self.tx.send(AgentEvent::Status(SessionStatus::Idle));
        }
    }

    /// 逐字串流（需要 `--include-partial-messages`）。
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
                            let out = self.stream.lock().unwrap().push("msg", t);
                            if !out.is_empty() {
                                let _ = self.tx.send(AgentEvent::Output(out));
                            }
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta["thinking"].as_str() {
                            let out = self.stream.lock().unwrap().push("think", t);
                            if !out.is_empty() {
                                let _ = self.tx.send(AgentEvent::Output(out));
                            }
                        }
                    }
                    _ => {}
                }
            }
            // 工具呼叫開始時先把文字收尾，順序才對。
            "content_block_start" if ev["content_block"]["type"].as_str() == Some("tool_use") => {
                self.flush_stream();
            }
            _ => {}
        }
    }

    /// 完整的 assistant 訊息：補上工具呼叫，以及（沒開 partial 時的）文字。
    fn on_assistant(&self, msg: &Value) {
        let content = msg["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for block in &content {
            match block["type"].as_str().unwrap_or("") {
                "text" => {
                    // 已經用 delta 串流過就不要再輸出一次。
                    if self.saw_delta.load(Ordering::SeqCst) {
                        self.flush_stream();
                    } else {
                        let text = block["text"].as_str().unwrap_or("");
                        if !text.is_empty() {
                            let _ = self.tx.send(AgentEvent::Output(format!("{text}\n")));
                        }
                    }
                }
                "tool_use" => {
                    self.flush_stream();
                    let _ = self.tx.send(AgentEvent::ToolCall {
                        name: block["name"].as_str().unwrap_or("tool").to_string(),
                        input: block["input"].to_string(),
                        ok: None,
                        output: None,
                    });
                }
                "thinking" => {}
                _ => {}
            }
        }
        if let Some(cost) = usage_cost(&msg["message"]["usage"]) {
            let _ = self.tx.send(AgentEvent::Cost(cost));
        }
    }

    /// 工具結果（Claude 用 `user` 訊息回報）。
    fn on_tool_results(&self, msg: &Value) {
        let content = msg["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for block in &content {
            if block["type"].as_str() != Some("tool_result") {
                continue;
            }
            let ok = !block["is_error"].as_bool().unwrap_or(false);
            let output = match &block["content"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let _ = self.tx.send(AgentEvent::ToolCall {
                name: String::new(),
                input: String::new(),
                ok: Some(ok),
                output: Some(truncate(&output, 4000)),
            });
        }
    }

    fn on_result(&self, msg: &Value) {
        self.flush_stream();
        if let Some(cost) = result_cost(msg) {
            let _ = self.tx.send(AgentEvent::Cost(cost));
        }
        if msg["is_error"].as_bool().unwrap_or(false) {
            let _ = self.tx.send(AgentEvent::Status(SessionStatus::Failed));
        } else {
            let _ = self.tx.send(AgentEvent::Status(SessionStatus::Idle));
        }
    }
}

/// 從 Anthropic 的 usage 區塊估算成本（Claude 只在 `result` 給總額）。
fn result_cost(msg: &Value) -> Option<Cost> {
    let usd = msg["total_cost_usd"].as_f64()?;
    let usage = &msg["usage"];
    Some(Cost {
        usd,
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
    })
}

fn usage_cost(usage: &Value) -> Option<Cost> {
    if usage.is_null() {
        return None;
    }
    Some(Cost {
        usd: 0.0,
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
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
// 控制把手
// ---------------------------------------------------------------------------

struct ClaudeControl {
    conn: Arc<Conn>,
    child: Arc<Mutex<Child>>,
}

impl AgentControl for ClaudeControl {
    fn send_input(&self, text: &str) -> Result<()> {
        let _ = self.conn.tx.send(AgentEvent::Status(SessionStatus::Busy));
        self.conn.send_raw(&json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
        }))
    }

    /// 送一個 interrupt 控制訊息（best-effort；Claude 沒有公開的取消 API）。
    fn cancel(&self) -> Result<()> {
        self.conn.flush_stream();
        self.conn.send_raw(&json!({
            "type": "control_request",
            "request_id": format!("ade-cancel-{}", std::process::id()),
            "request": { "subtype": "interrupt" }
        }))
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

    /// M1 尚未支援逐項審批：Claude 的權限在啟動時以 `--permission-mode` 決定。
    fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
        bail!("claude adapter 尚未支援逐項審批（請用 --permission auto/bypass）")
    }

    fn transport(&self) -> &'static str {
        "stream-json"
    }
}

// ---------------------------------------------------------------------------
// 啟動
// ---------------------------------------------------------------------------

/// 權限相關的命令列參數（唯讀、放行／禁止工具，與固定放行的 ade MCP 工具合併）。
fn permission_args(spec: &SessionSpec) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if spec.read_only {
        out.extend(["--permission-mode".into(), "plan".into()]);
    } else {
        out.extend(spec.permission.claude_flags().into_iter().map(String::from));
    }
    let mut allowed = spec.allowed_tools.clone();
    if spec.mcp_exe.is_some() && spec.permission.auto_approves() {
        allowed.push("mcp__ade__*".into());
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

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let prog = crate::agent_bin("claude", "ADE_CLAUDE_BIN").context("找不到 `claude`")?;
    let mut cmd = Command::new(prog);
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]);
    if let Some(m) = &spec.model {
        cmd.args(["--model", m]);
    }
    cmd.args(permission_args(&spec));
    if let Some(exe) = &spec.mcp_exe {
        let mcp_cfg = json!({
            "mcpServers": {
                "ade": {
                    "command": exe.to_string_lossy(),
                    "args": ["mcp", "--session", &spec.id]
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
        .stderr(Stdio::null());
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().context("無法啟動 `claude`")?;
    let stdin = child.stdin.take().context("取得 claude stdin 失敗")?;
    let stdout = child.stdout.take().context("取得 claude stdout 失敗")?;

    let _ = tx.send(AgentEvent::Status(SessionStatus::Starting));

    let conn = Arc::new(Conn {
        stdin: Mutex::new(stdin),
        stream: Mutex::new(StreamBuf::default()),
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
            // try_wait 輪詢：不能在阻塞 wait 期間握著鎖，否則 kill() 會死鎖。
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

    let control = ClaudeControl { conn, child };
    if let Some(p) = &spec.initial_prompt {
        control.send_input(p)?;
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

    fn spec_for_args() -> SessionSpec {
        SessionSpec {
            id: "s".into(),
            kind: ade_core::AgentKind::ClaudeCode,
            cwd: std::env::temp_dir(),
            transport: ade_protocol::Transport::Structured,
            args: vec![],
            model: None,
            allowed_tools: vec!["Read".into(), "Grep".into()],
            disallowed_tools: vec!["Bash".into()],
            read_only: false,
            sandbox: None,
            env: vec![],
            permission: ade_core::PermissionPolicy::Auto,
            mcp_exe: Some("/bin/ade".into()),
            initial_prompt: None,
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn permission_args_merge_allowed_tools_with_mcp() {
        let a = permission_args(&spec_for_args());
        assert!(
            a.contains(&"--allowedTools=Read,Grep,mcp__ade__*".to_string()),
            "{a:?}"
        );
        assert!(a.contains(&"--disallowedTools=Bash".to_string()));
        let mut s = spec_for_args();
        s.read_only = true;
        let a = permission_args(&s);
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
    }

    #[test]
    fn truncates_long_tool_output() {
        let s = "x".repeat(10);
        assert_eq!(truncate(&s, 4), "xxxx…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
