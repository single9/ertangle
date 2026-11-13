//! 原生 Agent Client Protocol adapter。
//!
//! M0 用於 OpenCode（`opencode acp`），這是四家 agent 中唯一「原生、零額外依賴」
//! 就提供結構化協定的一條，因此當作第一個結構化 adapter。
//!
//! 涵蓋的 ACP 子集：`initialize`、`session/new`、`session/load`（接續）、`session/prompt`、
//! `session/cancel`，
//! 以及 server→client 的 `session/update`（串流）與 `session/request_permission`（審批）。

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
use termade_core::{new_id, Decision, PermissionRequest, SessionStatus};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3600);
/// `session/load` 要等 agent 把整段歷史重播完，對話長時比一般呼叫久。
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);

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
    /// 第一則 prompt 前置的協作說明（取出一次即清空）。
    prepend_once: Mutex<Option<String>>,
    /// agent 在 initialize 宣告支援圖片區塊（`promptCapabilities.image`）。
    image_ok: std::sync::atomic::AtomicBool,
    /// `session/load` 進行中：agent 會把歷史以 `session/update` 重播，這段期間的更新一律丟掉，
    /// 否則逐字稿會重複。由 reader 在收到 load 的回應時清除（重播一定在回應之前）。
    loading: std::sync::atomic::AtomicBool,
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
    fn prompt(&self, text: &str, atts: &[termade_core::Attachment]) -> Result<()> {
        let sid = self.acp_session.lock().unwrap().clone();
        if sid.is_empty() {
            bail!("session 尚未就緒");
        }
        let effective = take_effective_prompt(&self.prepend_once, text);
        let _ = self.tx.send(AgentEvent::Status(SessionStatus::Busy));
        let res = self.call(
            "session/prompt",
            json!({ "sessionId": sid, "prompt": prompt_blocks(&effective, atts, self.image_ok.load(Ordering::SeqCst)) }),
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
            "usage_update" => {
                // ACP: `used`（目前 context 佔用）與 `size`（視窗大小）。
                if let Some(used) = update["used"].as_u64() {
                    let _ = self.tx.send(AgentEvent::Context {
                        used,
                        window: update["size"].as_u64(),
                    });
                }
            }
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
            } else if method == "session/update" && !conn.loading.load(Ordering::SeqCst) {
                conn.on_update(&msg);
            }
        } else if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            // 載入期間唯一在等的請求就是 `session/load`；在同一條讀取執行緒上清旗標，
            // 回應之後的更新才不會被誤丟。
            conn.loading.store(false, Ordering::SeqCst);
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
        self.send_input_with(text, &[])
    }

    fn send_input_with(&self, text: &str, atts: &[termade_core::Attachment]) -> Result<()> {
        let conn = self.conn.clone();
        let text = text.to_string();
        let atts = atts.to_vec();
        std::thread::spawn(move || {
            if let Err(e) = conn.prompt(&text, &atts) {
                tracing::warn!("ACP prompt 失敗：{e}");
                if termade_core::looks_like_limit(&e.to_string()) {
                    let _ = conn.tx.send(AgentEvent::Limited(e.to_string()));
                }
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

/// 取出只用一次的前置說明，接在第一則 prompt 前面。
fn take_effective_prompt(prepend_once: &Mutex<Option<String>>, text: &str) -> String {
    match prepend_once.lock().unwrap().take() {
        Some(prefix) if !prefix.trim().is_empty() => {
            format!("{}\n\n{}", prefix.trim_end(), text)
        }
        _ => text.to_string(),
    }
}

/// ACP prompt 區塊：文字（含內嵌文字檔）；agent 支援圖片時另加 image 區塊，否則圖片改成一行說明。
fn prompt_blocks(text: &str, atts: &[termade_core::Attachment], image_ok: bool) -> Value {
    let mut blocks =
        vec![json!({ "type": "text", "text": termade_core::compose_text(text, atts, !image_ok) })];
    if image_ok {
        for a in atts.iter().filter(|a| a.is_image()) {
            if let Some(data) = &a.data {
                blocks.push(json!({ "type": "image", "data": data, "mimeType": a.mime }));
            }
        }
    }
    Value::Array(blocks)
}

/// 從 `session/new` 回應的 `configOptions` 找出 model 選項，並驗證 `wanted` 合法。
/// 回傳 `(configId, value)`；agent 沒有回報 model 選項或值不在清單內時回傳明確錯誤。
fn pick_model_option(new: &Value, wanted: &str) -> Result<(String, String)> {
    let opts = new["configOptions"].as_array().cloned().unwrap_or_default();
    let opt = opts
        .iter()
        .find(|o| o["category"] == "model" || o["id"] == "model")
        .ok_or_else(|| anyhow!("此 agent 沒有回報可選的 model（configOptions 無 model 項）"))?;
    let id = opt["id"].as_str().unwrap_or("model").to_string();
    // options 可能是平的 [{value,name}]，也可能分組 [{options:[...]}]。
    let mut values: Vec<String> = Vec::new();
    for o in opt["options"].as_array().into_iter().flatten() {
        if let Some(v) = o["value"].as_str() {
            values.push(v.to_string());
        }
        for g in o["options"].as_array().into_iter().flatten() {
            if let Some(v) = g["value"].as_str() {
                values.push(v.to_string());
            }
        }
    }
    if !values.is_empty() && !values.iter().any(|v| v == wanted) {
        bail!("未知的 model `{wanted}`；可用：{}", values.join(", "));
    }
    Ok((id, wanted.to_string()))
}

/// 從 `session/new` 的 `configOptions` 找出推理強度選項（id 或 category 含 effort／reasoning／thought）。
fn pick_effort_option(new: &Value, wanted: &str) -> Result<(String, String)> {
    let opts = new["configOptions"].as_array().cloned().unwrap_or_default();
    let is_effort = |s: &str| {
        let s = s.to_ascii_lowercase();
        s.contains("effort") || s.contains("reasoning") || s.contains("thought")
    };
    let opt = opts
        .iter()
        .find(|o| {
            is_effort(o["id"].as_str().unwrap_or(""))
                || is_effort(o["category"].as_str().unwrap_or(""))
        })
        .ok_or_else(|| anyhow!("此 agent 沒有回報可設定的 effort（configOptions 無相關項）"))?;
    let id = opt["id"].as_str().unwrap_or("effort").to_string();
    let values: Vec<String> = opt["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["value"].as_str().map(str::to_string))
        .collect();
    if !values.is_empty() && !values.iter().any(|v| v == wanted) {
        bail!("effort `{wanted}` 不在可用清單：{}", values.join(", "));
    }
    Ok((id, wanted.to_string()))
}

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let (default_prog, args) = spec
        .kind
        .acp_command()
        .context("此 agent 不支援原生結構化協定")?;
    let prog = match spec.bin.as_deref() {
        Some(b) => crate::which(b).context(format!("找不到指定的執行檔 `{b}`"))?,
        None => crate::which(default_prog).context(format!("找不到 `{default_prog}`"))?,
    };

    let mut cmd = Command::new(&prog);
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
        .with_context(|| format!("無法啟動 `{} acp`", prog.display()))?;

    let stdin = child.stdin.take().context("取得 stdin 失敗")?;
    let stdout = child.stdout.take().context("取得 stdout 失敗")?;

    let conn = Arc::new(Conn {
        session_id: spec.id.clone(),
        acp_session: Mutex::new(String::new()),
        stdin: Mutex::new(stdin),
        next_id: AtomicI64::new(1),
        pending: Mutex::new(HashMap::new()),
        perms: Mutex::new(HashMap::new()),
        stream: Mutex::new(StreamBuf::default()),
        prepend_once: Mutex::new(spec.system_append.clone().filter(|s| !s.trim().is_empty())),
        image_ok: std::sync::atomic::AtomicBool::new(false),
        loading: std::sync::atomic::AtomicBool::new(false),
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
    conn.image_ok.store(
        init["agentCapabilities"]["promptCapabilities"]["image"]
            .as_bool()
            .unwrap_or(false),
        Ordering::SeqCst,
    );

    let mcp_servers = match &spec.mcp_exe {
        Some(exe) => json!([{
            "name": "termade",
            "command": exe.to_string_lossy(),
            "args": ["mcp", "--session", &spec.id],
            "env": []
        }]),
        None => json!([]),
    };
    // 只有宣告 `loadSession` 的 agent 之後才接得回來。
    let can_load = init["agentCapabilities"]["loadSession"]
        .as_bool()
        .unwrap_or(false);
    let (sid, new) = match &spec.resume {
        Some(id) => {
            if !can_load {
                bail!("{} 沒有宣告 loadSession，無法接續對話", spec.kind);
            }
            conn.loading.store(true, Ordering::SeqCst);
            let loaded = conn.call(
                "session/load",
                json!({ "sessionId": id, "cwd": spec.cwd.to_string_lossy(), "mcpServers": mcp_servers }),
                LOAD_TIMEOUT,
            );
            conn.loading.store(false, Ordering::SeqCst);
            // 重播的片段可能有一部分已進了行緩衝，丟掉。
            let _ = conn.stream.lock().unwrap().flush();
            (id.clone(), loaded?)
        }
        None => {
            let new = conn.call(
                "session/new",
                json!({ "cwd": spec.cwd.to_string_lossy(), "mcpServers": mcp_servers }),
                CALL_TIMEOUT,
            )?;
            let sid = new["sessionId"]
                .as_str()
                .context("`session/new` 未回傳 sessionId")?
                .to_string();
            (sid, new)
        }
    };
    *conn.acp_session.lock().unwrap() = sid.clone();
    if can_load {
        let _ = tx.send(AgentEvent::NativeId(sid.clone()));
    }
    // 接續時 model／effort 已存在 agent 的對話裡；回應沒帶 configOptions 就不重設。
    let has_options = spec.resume.is_none() || new["configOptions"].is_array();
    if let Some(wanted) = spec.model.as_ref().filter(|_| has_options) {
        let (config_id, value) = pick_model_option(&new, wanted)?;
        conn.call(
            "session/set_config_option",
            json!({ "sessionId": sid, "configId": config_id, "value": value }),
            CALL_TIMEOUT,
        )
        .with_context(|| format!("設定 model `{value}` 失敗"))?;
        let _ = tx.send(AgentEvent::Model(value));
    }
    if let Some(e) = spec.effort.as_ref().filter(|_| has_options) {
        let (config_id, value) = pick_effort_option(&new, e)?;
        conn.call(
            "session/set_config_option",
            json!({ "sessionId": sid, "configId": config_id, "value": value }),
            CALL_TIMEOUT,
        )
        .with_context(|| format!("設定 effort `{value}` 失敗"))?;
    }
    let _ = tx.send(AgentEvent::Status(SessionStatus::Idle));

    let control = AcpControl { conn, child };

    if let Some(p) = &spec.initial_prompt {
        control.send_input_with(p, &spec.initial_attachments)?;
    }

    Ok(Box::new(control))
}

#[cfg(test)]
mod model_tests {
    use super::*;

    #[test]
    fn picks_model_option_and_validates() {
        let new = json!({"sessionId":"x","configOptions":[
            {"id":"mode","category":"mode","options":[{"value":"a"}]},
            {"id":"model","category":"model","type":"select","options":[
                {"value":"p/m1","name":"M1"},{"value":"p/m2","name":"M2"}]}]});
        assert_eq!(pick_model_option(&new, "p/m2").unwrap().1, "p/m2");
        let e = pick_model_option(&new, "zzz").unwrap_err().to_string();
        assert!(e.contains("p/m1") && e.contains("zzz"));
        assert!(pick_model_option(&json!({"sessionId":"x"}), "a").is_err());
    }

    #[test]
    fn prepend_once_applies_only_to_first_prompt() {
        let p = Mutex::new(Some("[termade 協作說明]\n規則\n".to_string()));
        let first = take_effective_prompt(&p, "做任務 A");
        assert!(first.starts_with("[termade 協作說明]\n規則\n\n做任務 A"));
        let second = take_effective_prompt(&p, "做任務 B");
        assert_eq!(second, "做任務 B");
    }
}
