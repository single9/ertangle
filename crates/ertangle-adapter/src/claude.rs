//! Claude Code 的原生 adapter (`--input-format stream-json --output-format stream-json`)。
//!
//! 這是雙向 NDJSON：我們往 stdin 寫「使用者訊息」，Claude 往 stdout 吐事件。
//! 因為 Claude Code 沒有原生 ACP，這是它唯一「零額外相依性」的結構化介面。
//!
//! 觀察到的 wire 形狀 (claude 2.1.x)：
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

/// 讀取執行中的 Claude 事件流。
struct Conn {
    stdin: Mutex<ChildStdin>,
    stream: Mutex<StreamBuf>,
    /// 這一則訊息是否已經用 delta 串流過 (避免與完整 assistant 訊息重複)。
    saw_delta: AtomicBool,
    tx: Sender<AgentEvent>,
}

impl Conn {
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
            "rate_limit_event" => {
                if let Some(q) = parse_rate_limit_event(&msg) {
                    let _ = self.tx.send(AgentEvent::Quota(q));
                }
            }
            // hook_started / hook_response / rate_limit_event / system.status…
            _ => {}
        }
    }

    fn on_system(&self, msg: &Value) {
        if msg["subtype"].as_str() == Some("init") {
            if let Some(model) = msg["model"].as_str() {
                let _ = self.tx.send(AgentEvent::Model(model.to_string()));
            }
            if let Some(cmds) = msg["slash_commands"].as_array() {
                let names: Vec<String> = cmds
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect();
                if !names.is_empty() {
                    let _ = self.tx.send(AgentEvent::Commands(names));
                }
            }
            // 每次都送：接續後 id 可能換新，以最新的為準。
            if let Some(id) = msg["session_id"].as_str().filter(|s| !s.is_empty()) {
                let _ = self.tx.send(AgentEvent::NativeId(id.to_string()));
            }
            // 狀態由 daemon 在啟動完成時設為 Idle，回合結束由 `result` 設回 Idle；
            // 這裡不能送 Idle，否則初始 prompt 還在處理時畫面會顯示閒置。
        }
    }

    /// 逐字串流 (需要 `--include-partial-messages`)。
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
                                let _ = self.tx.send(AgentEvent::Output(out));
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

    /// 完整的 assistant 訊息：補上工具呼叫，以及 (沒開 partial 時的)文字。
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
        // context 用量 = 這一則 assistant 訊息的輸入側 token (含快取)；視窗大小在 `result` 才知道。
        let u = &msg["message"]["usage"];
        if !u.is_null() {
            let used = u["input_tokens"].as_u64().unwrap_or(0)
                + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                + u["cache_read_input_tokens"].as_u64().unwrap_or(0);
            if used > 0 {
                let _ = self.tx.send(AgentEvent::Context { used, window: None });
            }
        }
        // 不在這裡送逐訊息的 usage：它只有單則訊息的 token 數，會把累計值蓋成很小的數字，
        // 讓畫面上的成本與 token 在回合中途往回跳。累計值只以 `result` 為準。
    }

    /// 工具結果 (Claude 用 `user` 訊息回報)。
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
                // MCP 工具回傳 [{"type":"text","text":...}]：取出文字，不顯示 JSON 外殼。
                Value::Array(parts) if parts.iter().all(|p| p["text"].is_string()) => parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
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
        if let Some(window) = msg["modelUsage"]
            .as_object()
            .and_then(|m| m.values().filter_map(|v| v["contextWindow"].as_u64()).max())
        {
            // 用量沿用最近一則 assistant 訊息的值，這裡只補上視窗大小。
            let _ = self.tx.send(AgentEvent::Context {
                used: 0,
                window: Some(window),
            });
        }
        if let Some(cost) = result_cost(msg) {
            let _ = self.tx.send(AgentEvent::Cost(cost));
        }
        if msg["is_error"].as_bool().unwrap_or(false) {
            let detail = msg["result"].as_str().unwrap_or("").to_string();
            if ertangle_core::looks_like_limit(&detail) {
                let _ = self.tx.send(AgentEvent::Limited(detail));
            } else if !detail.trim().is_empty() {
                let note = tr!("claude 錯誤：{detail}", detail = detail.trim());
                let _ = self.tx.send(AgentEvent::Output(format!("⚠ {note}\n")));
                let _ = self.tx.send(AgentEvent::Note(note));
            }
            let _ = self.tx.send(AgentEvent::Status(SessionStatus::Failed));
        } else {
            let _ = self.tx.send(AgentEvent::Status(SessionStatus::Idle));
        }
    }
}

/// 解析 `rate_limit_event`：`unifiedWindows` 每個視窗有 `utilization`(0..1)與 `resetsAt` (秒)。
pub(crate) fn parse_rate_limit_event(msg: &Value) -> Option<ertangle_core::Quota> {
    let info = &msg["rate_limit_info"];
    let mut windows: Vec<ertangle_core::QuotaWindow> = Vec::new();
    // `allowed_warning` (接近上限的提醒) 仍可使用，只有 `rejected` 之類非 `allowed*` 的狀態才算用完。
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
                resets_at_ms: w["resetsAt"].as_u64().map(|s| s * 1000),
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

/// 從 Anthropic 的 usage 區塊估算成本 (Claude 只在 `result` 給總額)。
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

/// 切換 model 的控制訊息。
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
        let _ = self.conn.tx.send(AgentEvent::Status(SessionStatus::Busy));
        self.conn.send_raw(&json!({
            "type": "user",
            "message": { "role": "user", "content": user_content(text, atts) }
        }))
    }

    /// 送一個 interrupt 控制訊息 (best-effort；Claude 沒有公開的取消 API)。
    fn cancel(&self) -> Result<()> {
        self.conn.flush_stream();
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

    /// Claude 的逐項核准透過 `--permission-prompt-tool` (MCP `ertangle_approve`) 由 daemon 處理。
    fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
        bail!(
            "{}",
            tr!("claude 逐項核准已由 MCP (ertangle_approve) 轉發，此請求無效或已逾時")
        )
    }

    fn transport(&self) -> &'static str {
        "stream-json"
    }

    /// `control_request` 的 `set_model` (實測可用)與 `apply_flag_settings`(effortLevel)。
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
// 啟動
// ---------------------------------------------------------------------------

/// Claude 的 user content：文字 (含內嵌的文字檔)加上 base64 圖片區塊。
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

/// 權限與系統提示相關的命令列參數 (唯讀、放行／禁止工具、固定放行的 ertangle MCP 工具、附加系統提示)。
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
    // 非 bypass 時，需要核准的工具呼叫交給 ertangle (規則引擎／使用者)決定。
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

/// 固定的 stream-json 參數，加上 model 與接續 (`--resume <id>`：接回原對話，實測 id 不變)。
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
