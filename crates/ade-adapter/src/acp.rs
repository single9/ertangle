//! 原生 Agent Client Protocol adapter。
//!
//! M0 用於 OpenCode（`opencode acp`），這是四家 agent 中唯一「原生、零額外依賴」
//! 就提供結構化協定的一條，因此當作第一個結構化 adapter。
//!
//! 涵蓋的 ACP 子集：`initialize`、`session/new`、`session/prompt`、`session/cancel`，
//! 以及 server→client 的 `session/update`（串流）與 `session/request_permission`（審批）。

use crate::stream::StreamBuf;
use crate::{AgentControl, AgentEvent, SessionSpec};
use ade_core::{new_id, Decision, PermissionRequest, SessionStatus};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3600);

struct Conn {
    /// 我們自己的 session id（事件關聯用）。
    session_id: String,
    /// agent 回傳的 ACP session id（prompt / cancel 用）。
    acp_session: Mutex<String>,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Sender<Value>>>,
    /// 審批：內部 req id -> (JSON-RPC id, options)
    perms: Mutex<HashMap<String, (Value, Vec<Value>)>>,
    /// 串流輸出的行緩衝（把 token 級 chunk 併成完整行）。
    stream: Mutex<StreamBuf>,
    /// 權限政策：`Auto` / `Bypass` 時由 adapter 直接代為核准。
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

    fn respond(&self, id: &Value, result: Value) {
        let _ = self.write(&json!({"jsonrpc":"2.0","id":id,"result":result}));
    }

    fn respond_error(&self, id: &Value, code: i64, message: &str) {
        let _ = self.write(&json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":code,"message":message}
        }));
    }

    /// 送一個請求並等待回應。
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

    /// 送出一段 prompt，等整個回合結束。
    fn prompt(&self, text: &str) -> Result<()> {
        let sid = self.acp_session.lock().unwrap().clone();
        if sid.is_empty() {
            bail!("session 尚未就緒");
        }
        let _ = self.tx.send(AgentEvent::Status(SessionStatus::Busy));
        let res = self.call(
            "session/prompt",
            json!({ "sessionId": sid, "prompt": [{ "type": "text", "text": text }] }),
            PROMPT_TIMEOUT,
        );
        self.flush_stream();
        let _ = self.tx.send(AgentEvent::Status(SessionStatus::Idle));
        res.map(|_| ())
    }

    fn on_update(&self, msg: &Value) {
        let update = &msg["params"]["update"];
        let kind = update["sessionUpdate"].as_str().unwrap_or("");
        match kind {
            "agent_message_chunk" => {
                if let Some(t) = update["content"]["text"].as_str() {
                    let text = self.stream.lock().unwrap().push("msg", t);
                    if !text.is_empty() {
                        let _ = self.tx.send(AgentEvent::Output(text));
                    }
                }
            }
            "agent_thought_chunk" => {
                if let Some(t) = update["content"]["text"].as_str() {
                    let text = self.stream.lock().unwrap().push("think", t);
                    if !text.is_empty() {
                        let _ = self.tx.send(AgentEvent::Output(text));
                    }
                }
            }
            "tool_call" => {
                self.flush_stream();
                let name = tool_name(update);
                let input = update["rawInput"].to_string();
                let _ = self.tx.send(AgentEvent::ToolCall {
                    name,
                    input,
                    ok: None,
                    output: None,
                });
            }
            "tool_call_update" => {
                let name = tool_name(update);
                let status = update["status"].as_str().unwrap_or("");
                let ok = Some(status == "completed");
                let output = update
                    .get("content")
                    .filter(|c| !c.is_null())
                    .map(|c| c.to_string());
                let _ = self.tx.send(AgentEvent::ToolCall {
                    name,
                    input: String::new(),
                    ok,
                    output,
                });
            }
            _ => {}
        }
    }

    /// 把行緩衝的殘留送出（換種類或工具呼叫前呼叫）。
    fn flush_stream(&self) {
        let text = self.stream.lock().unwrap().flush();
        if !text.is_empty() {
            let _ = self.tx.send(AgentEvent::Output(text));
        }
    }

    fn on_permission(&self, msg: &Value) {
        let jid = msg["id"].clone();
        let params = &msg["params"];
        let options = params["options"].as_array().cloned().unwrap_or_default();

        // 一律轉給 daemon：規則引擎與 session 政策（auto／bypass 自動核准）都在那裡決定。
        let req = new_id("perm");
        let title = params["toolCall"]["title"]
            .as_str()
            .unwrap_or("permission")
            .to_string();
        let detail = params["toolCall"]["rawInput"].to_string();
        self.perms
            .lock()
            .unwrap()
            .insert(req.clone(), (jid, options));
        let _ = self.tx.send(AgentEvent::Permission(PermissionRequest {
            req,
            session: self.session_id.clone(),
            title,
            detail,
        }));
    }

    fn resolve(&self, req: &str, decision: Decision) -> Result<()> {
        let entry = self.perms.lock().unwrap().remove(req);
        let Some((jid, options)) = entry else {
            bail!("未知的審批請求：{req}");
        };
        self.respond(&jid, decision_result(decision, &options));
        Ok(())
    }
}

/// 把正規化的 [`Decision`] 轉成 ACP 的 `outcome`。
fn decision_result(decision: Decision, options: &[Value]) -> Value {
    let wanted: &[&str] = match decision {
        Decision::Allow => &["allow_once", "allow_always"],
        Decision::AllowAlways => &["allow_always", "allow_once"],
        Decision::Deny => &["reject_once", "reject_always"],
    };
    let chosen = options
        .iter()
        .find(|o| wanted.contains(&o["kind"].as_str().unwrap_or("")))
        .or_else(|| options.first());

    match (decision, chosen) {
        (Decision::Deny, None) => json!({"outcome":{"outcome":"cancelled"}}),
        (_, Some(o)) => json!({"outcome":{"outcome":"selected","optionId": o["optionId"]}}),
        _ => json!({"outcome":{"outcome":"cancelled"}}),
    }
}

fn tool_name(update: &Value) -> String {
    update["title"]
        .as_str()
        .or_else(|| update["kind"].as_str())
        .or_else(|| update["toolCallId"].as_str())
        .unwrap_or("tool")
        .to_string()
}

fn reader_loop(conn: Arc<Conn>, stdout: impl BufRead) {
    for line in stdout.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg): Result<Value, _> = serde_json::from_str(&line) else {
            continue;
        };

        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            if msg.get("id").is_some() {
                // server → client 請求
                match method {
                    "session/request_permission" => conn.on_permission(&msg),
                    _ => conn.respond_error(msg.get("id").unwrap(), -32601, "not supported"),
                }
            } else if method == "session/update" {
                conn.on_update(&msg);
            }
        } else if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            if let Some(s) = conn.pending.lock().unwrap().remove(&id) {
                let _ = s.send(msg);
            }
        }
    }
    let _ = conn.tx.send(AgentEvent::Exited(None));
}

struct AcpControl {
    conn: Arc<Conn>,
    child: Arc<Mutex<Child>>,
}

impl AgentControl for AcpControl {
    fn send_input(&self, text: &str) -> Result<()> {
        let conn = self.conn.clone();
        let text = text.to_string();
        std::thread::spawn(move || {
            if let Err(e) = conn.prompt(&text) {
                tracing::warn!("ACP prompt 失敗：{e}");
            }
        });
        Ok(())
    }

    fn cancel(&self) -> Result<()> {
        let sid = self.conn.acp_session.lock().unwrap().clone();
        if sid.is_empty() {
            return Ok(());
        }
        self.conn.write(&json!({
            "jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}
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

    fn resolve_permission(&self, req: &str, decision: Decision) -> Result<()> {
        self.conn.resolve(req, decision)
    }

    fn transport(&self) -> &'static str {
        "acp"
    }
}

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let (prog, args) = spec
        .kind
        .acp_command()
        .context("此 agent 不支援原生結構化協定")?;

    let mut cmd = Command::new(prog);
    cmd.args(args)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("無法啟動 `{prog} acp`"))?;

    let stdin = child.stdin.take().context("取得 stdin 失敗")?;
    let stdout = child.stdout.take().context("取得 stdout 失敗")?;
    let _ = tx.send(AgentEvent::Status(SessionStatus::Starting));

    let conn = Arc::new(Conn {
        session_id: spec.id.clone(),
        acp_session: Mutex::new(String::new()),
        stdin: Mutex::new(stdin),
        next_id: AtomicI64::new(1),
        pending: Mutex::new(HashMap::new()),
        perms: Mutex::new(HashMap::new()),
        stream: Mutex::new(StreamBuf::default()),
        tx: tx.clone(),
    });

    {
        let conn = conn.clone();
        std::thread::spawn(move || reader_loop(conn, BufReader::new(stdout)));
    }

    let child = Arc::new(Mutex::new(child));
    {
        let child = child.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            // 用 try_wait 輪詢（而非阻塞 wait），否則會一直握著鎖，讓 `kill()` 永遠拿不到鎖而死鎖。
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

    // 握手。
    let init = conn.call(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } }
        }),
        CALL_TIMEOUT,
    )?;
    tracing::debug!("ACP initialize: {init}");

    let mcp_servers = match &spec.mcp_exe {
        Some(exe) => json!([{
            "name": "ade",
            "command": exe.to_string_lossy(),
            "args": ["mcp", "--session", &spec.id],
            "env": []
        }]),
        None => json!([]),
    };
    let new = conn.call(
        "session/new",
        json!({ "cwd": spec.cwd.to_string_lossy(), "mcpServers": mcp_servers }),
        CALL_TIMEOUT,
    )?;
    let sid = new["sessionId"]
        .as_str()
        .context("`session/new` 未回傳 sessionId")?
        .to_string();
    *conn.acp_session.lock().unwrap() = sid;
    let _ = tx.send(AgentEvent::Status(SessionStatus::Idle));

    let control = AcpControl { conn, child };

    if let Some(p) = &spec.initial_prompt {
        control.send_input(p)?;
    }

    Ok(Box::new(control))
}
