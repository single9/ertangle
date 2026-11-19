//! 原生 Agent Client Protocol adapter。
//!
//! M0 用於 OpenCode (`opencode acp`)，這是四家 agent 中唯一「原生、零額外相依性」
//! 就提供結構化協定的一條，因此當作第一個結構化 adapter。
//!
//! 涵蓋的 ACP 子集：`initialize`、`session/new`、`session/load` (接續)、`session/prompt`、
//! `session/cancel`，
//! 以及 server→client 的 `session/update` (串流)與 `session/request_permission` (核准)。

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
use tr::tr;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3600);
/// `session/load` 要等 agent 把整段歷史重播完，對話長時比一般呼叫久。
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);

struct Conn {
    /// 這個 adapter 代表的 agent id (額度事件的 `agent` 欄位)。
    agent: String,
    /// 已送出 `reached=true` 的限流額度、尚未被成功回合清除。
    limited: std::sync::atomic::AtomicBool,
    /// Antigravity 專用：`agy --output-format json -p /quota` 的執行檔與環境變數。
    quota_prog: Option<(std::path::PathBuf, Vec<(String, String)>)>,
    /// 避免同時啟動多個額度查詢背景程序。
    quota_refreshing: std::sync::atomic::AtomicBool,
    /// 我們自己的 session id (事件關聯用)。
    session_id: String,
    /// agent 回傳的 ACP session id (prompt / cancel 用)。
    acp_session: Mutex<String>,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Sender<Value>>>,
    /// 核准：內部 req id -> (JSON-RPC id, options)
    perms: Mutex<HashMap<String, (Value, Vec<Value>)>>,
    /// 串流輸出的行緩衝 (把 token 級 chunk 併成完整行)。
    stream: Mutex<StreamBuf>,
    /// 第一則 prompt 前置的協作說明 (取出一次即清空)。
    prepend_once: Mutex<Option<String>>,
    /// agent 在 initialize 宣告支援圖片區塊 (`promptCapabilities.image`)。
    image_ok: std::sync::atomic::AtomicBool,
    /// `session/load` 進行中：agent 會把歷史以 `session/update` 重播，這段期間的更新一律丟掉，
    /// 否則逐字稿會重複。由 reader 在收到 load 的回應時清除 (重播一定在回應之前)。
    loading: std::sync::atomic::AtomicBool,
    /// 權限政策：`Auto` / `Bypass` 時由 adapter 直接代為核准。
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

    /// 送出一段 prompt，等整個回合結束。
    fn prompt(&self, text: &str, atts: &[termade_core::Attachment]) -> Result<()> {
        let sid = self
            .acp_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if sid.is_empty() {
            bail!("{}", tr!("session 尚未就緒"));
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
        if res.is_ok() {
            if let Some(q) = recovered_quota(&self.agent, &self.limited) {
                let _ = self.tx.send(AgentEvent::Quota(q));
            }
        }
        res.map(|_| ())
    }

    /// 錯誤文字若是限流就送出 `reached=true` 並記下旗標 (之後成功的回合會清除)。
    fn report_rate_limit(&self, error_text: &str) {
        if let Some(q) = rate_limit_quota(&self.agent, error_text) {
            self.limited.store(true, Ordering::SeqCst);
            let _ = self.tx.send(AgentEvent::Quota(q));
        }
    }

    /// 非同步查詢 Antigravity 額度 (不阻塞回合)，同時間最多只跑一個查詢。
    fn refresh_quota_async(self: &Arc<Self>) {
        let Some((prog, env)) = self.quota_prog.clone() else {
            return;
        };
        if self
            .quota_refreshing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let conn = self.clone();
        std::thread::spawn(move || {
            struct Guard<'a>(&'a std::sync::atomic::AtomicBool);
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _guard = Guard(&conn.quota_refreshing);
            if let Some(raw) = query_agy_quota(&prog, &env) {
                let reached = conn.limited.load(Ordering::SeqCst);
                if let Some(q) = parse_agy_quota_json(&raw, reached) {
                    let _ = conn.tx.send(AgentEvent::Quota(q));
                }
            }
        });
    }

    fn on_update(&self, msg: &Value) {
        let update = &msg["params"]["update"];
        let kind = update["sessionUpdate"].as_str().unwrap_or("");
        match kind {
            "available_commands_update" => {
                let names: Vec<String> = update["availableCommands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["name"].as_str())
                    .map(|n| n.trim_start_matches('/').to_string())
                    .collect();
                let _ = self.tx.send(AgentEvent::Commands(names));
            }
            "usage_update" => {
                // ACP: `used` (目前 context 佔用)與 `size` (視窗大小)。
                if let Some(used) = update["used"].as_u64() {
                    let _ = self.tx.send(AgentEvent::Context {
                        used,
                        window: update["size"].as_u64(),
                    });
                }
            }
            "error" => {
                self.report_rate_limit(&update.to_string());
            }
            "agent_message_chunk" => {
                if let Some(t) = update["content"]["text"].as_str() {
                    let text = self
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push("msg", t);
                    if !text.is_empty() {
                        let _ = self.tx.send(AgentEvent::Output(text));
                    }
                }
            }
            "agent_thought_chunk" => {
                if let Some(t) = update["content"]["text"].as_str() {
                    let text = self
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push("think", t);
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
                // ACP 會用 pending／in_progress 逐步回報同一個工具；只有 completed／failed
                // 才是結果。以前把「不是 completed」一律當失敗，導致每個成功的 shell 呼叫
                // 都多印一行 `✗ 失敗`。
                let Some(ok) = tool_update_ok(update) else {
                    return;
                };
                let name = tool_name(update);
                let output = tool_result_text(update);
                let _ = self.tx.send(AgentEvent::ToolCall {
                    name,
                    input: String::new(),
                    ok: Some(ok),
                    output,
                });
            }
            _ => {}
        }
    }

    /// 把行緩衝的殘留送出 (換種類或工具呼叫前呼叫)。
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

    fn on_permission(&self, msg: &Value) {
        let jid = extract_request_id(msg);
        let params = &msg["params"];
        let options = params["options"].as_array().cloned().unwrap_or_default();

        // 一律轉給 daemon：規則引擎與 session 政策 (auto／bypass 自動核准)都在那裡決定。
        let req = new_id("perm");
        let title = params["toolCall"]["title"]
            .as_str()
            .unwrap_or("permission")
            .to_string();
        let detail = params["toolCall"]["rawInput"].to_string();
        self.perms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(req.clone(), (jid, options));
        let _ = self.tx.send(AgentEvent::Permission(PermissionRequest {
            req,
            session: self.session_id.clone(),
            title,
            detail,
        }));
    }

    fn resolve(&self, req: &str, decision: Decision) -> Result<()> {
        let entry = self
            .perms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(req);
        let Some((jid, options)) = entry else {
            bail!("{}", tr!("未知的核准請求：{req}", req = req));
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

/// `tool_call_update` 的狀態：`Some(true)` 成功、`Some(false)` 失敗、`None` 只是進度。
fn tool_update_ok(update: &Value) -> Option<bool> {
    match update["status"].as_str().unwrap_or("") {
        "completed" => Some(true),
        "failed" => Some(false),
        _ => None,
    }
}

/// 取出 ACP `content` 裡的純文字 (`ToolCallContent::Content`)，逐字稿才不會顯示 JSON 外殼。
/// 沒有文字內容時退回 `rawOutput`。
fn tool_result_text(update: &Value) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for item in update["content"].as_array().into_iter().flatten() {
        if let Some(t) = item["content"]["text"]
            .as_str()
            .or_else(|| item["text"].as_str())
            .filter(|t| !t.trim().is_empty())
        {
            parts.push(t);
        }
    }
    let text = if parts.is_empty() {
        match update.get("rawOutput") {
            Some(Value::Null) | None => return None,
            Some(v) => v.as_str().map_or_else(|| v.to_string(), str::to_string),
        }
    } else {
        parts.join("\n")
    };
    Some(truncate_chars(&text, 4000))
}

/// 截斷過長的工具輸出，避免逐字稿與 event log 被整份 stdout 撐爆。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// 錯誤文字是否為限流／額度用完 (`RESOURCE_EXHAUSTED`、HTTP 429、`quota exceeded`、`rate limit`)。
fn is_rate_limit_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("resource_exhausted")
        || lower.contains("quota exceeded")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
    {
        return true;
    }
    // 獨立的 `429` (前後不是數字)，避免誤判 `4290` 之類。
    let b = lower.as_bytes();
    lower.match_indices("429").any(|(i, _)| {
        (i == 0 || !b[i - 1].is_ascii_digit()) && b.get(i + 3).is_none_or(|c| !c.is_ascii_digit())
    })
}

/// 限流錯誤 → 「額度已用完」的 `Quota` (沒有百分比，只標記 `reached`)；其他錯誤回 `None`。
fn rate_limit_quota(agent: &str, error_text: &str) -> Option<termade_core::Quota> {
    is_rate_limit_error(error_text).then(|| termade_core::Quota {
        agent: agent.to_string(),
        windows: Vec::new(),
        reached: true,
        warning: false,
        updated_ms: termade_core::now_millis(),
    })
}

/// 回合成功後：若先前標記過限流，回傳解除 (`reached=false`) 的額度並清旗標；否則 `None`。
fn recovered_quota(
    agent: &str,
    limited: &std::sync::atomic::AtomicBool,
) -> Option<termade_core::Quota> {
    limited
        .swap(false, Ordering::SeqCst)
        .then(|| termade_core::Quota {
            agent: agent.to_string(),
            windows: Vec::new(),
            reached: false,
            warning: false,
            updated_ms: termade_core::now_millis(),
        })
}

/// 從 JSON-RPC 訊息中安全擷取 id；若不存在或為 null 則回傳安全後備值 (`Value::Null`)。
pub(crate) fn extract_request_id(msg: &Value) -> Value {
    match msg.get("id") {
        Some(id) if !id.is_null() => id.clone(),
        _ => Value::Null,
    }
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
            match msg.get("id") {
                Some(_) => match method {
                    "session/request_permission" => conn.on_permission(&msg),
                    _ => {
                        let id = extract_request_id(&msg);
                        conn.respond_error(&id, -32601, "not supported");
                    }
                },
                None => {
                    if method == "session/update" && !conn.loading.load(Ordering::SeqCst) {
                        conn.on_update(&msg);
                    }
                }
            }
        } else if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            // 載入期間唯一在等的請求就是 `session/load`；在同一條讀取執行緒上清旗標，
            // 回應之後的更新才不會被誤丟。
            conn.loading.store(false, Ordering::SeqCst);
            if let Some(s) = conn
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id)
            {
                let _ = s.send(msg);
            }
        }
    }
    let _ = conn.tx.send(AgentEvent::Exited(None));
}

struct AcpControl {
    conn: Arc<Conn>,
    child: Arc<Mutex<Child>>,
    /// 最近一次回報的 `configOptions` (包成 `{"configOptions": [...]}`)，執行期切換時用來驗證與找 configId。
    options: Mutex<Value>,
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
                conn.report_rate_limit(&e.to_string());
            }
            conn.refresh_quota_async();
        });
        Ok(())
    }

    /// `session/set_config_option`；回應帶完整 `configOptions`，順便更新快取。
    fn set_config(&self, model: Option<&str>, effort: Option<&str>) -> Result<Option<String>> {
        let sid = self
            .conn
            .acp_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut applied = None;
        let mut jobs = Vec::new();
        {
            let opts = self.options.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(m) = model {
                jobs.push(("model", pick_model_option(&opts, m)?));
            }
            if let Some(e) = effort {
                jobs.push(("effort", pick_effort_option(&opts, e)?));
            }
        }
        for (what, (config_id, value)) in jobs {
            let r = self
                .conn
                .call(
                    "session/set_config_option",
                    json!({ "sessionId": sid, "configId": config_id, "value": value }),
                    CALL_TIMEOUT,
                )
                .with_context(|| tr!("設定 {what} `{value}` 失敗", what = what, value = value))?;
            if r["configOptions"].is_array() {
                *self.options.lock().unwrap_or_else(|e| e.into_inner()) =
                    json!({ "configOptions": r["configOptions"] });
            }
            if what == "model" {
                applied = Some(value);
            }
        }
        Ok(applied)
    }

    fn cancel(&self) -> Result<()> {
        let sid = self
            .conn
            .acp_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if sid.is_empty() {
            return Ok(());
        }
        self.conn.write(&json!({
            "jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}
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

    fn resolve_permission(&self, req: &str, decision: Decision) -> Result<()> {
        self.conn.resolve(req, decision)
    }

    fn transport(&self) -> &'static str {
        "acp"
    }
}

/// 取出只用一次的前置說明，接在第一則 prompt 前面。
fn take_effective_prompt(prepend_once: &Mutex<Option<String>>, text: &str) -> String {
    match prepend_once
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        Some(prefix) if !prefix.trim().is_empty() => {
            format!("{}\n\n{}", prefix.trim_end(), text)
        }
        _ => text.to_string(),
    }
}

/// ACP prompt 區塊：文字 (含內嵌文字檔)；agent 支援圖片時另加 image 區塊，否則圖片改成一行說明。
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
        .ok_or_else(|| {
            anyhow!(tr!(
                "此 agent 沒有回報可選的 model (configOptions 無 model 項)"
            ))
        })?;
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
        bail!(
            "{}",
            tr!(
                "未知的 model `{wanted}`；可用：{list}",
                wanted = wanted,
                list = values.join(", ")
            )
        );
    }
    Ok((id, wanted.to_string()))
}

/// 從 `session/new` 的 `configOptions` 找出推理強度選項 (id 或 category 含 effort／reasoning／thought)。
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
        .ok_or_else(|| {
            anyhow!(tr!(
                "此 agent 沒有回報可設定的 effort (configOptions 無相關項)"
            ))
        })?;
    let id = opt["id"].as_str().unwrap_or("effort").to_string();
    let values: Vec<String> = opt["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["value"].as_str().map(str::to_string))
        .collect();
    if !values.is_empty() && !values.iter().any(|v| v == wanted) {
        bail!(
            "{}",
            tr!(
                "effort `{wanted}` 不在可用清單：{list}",
                wanted = wanted,
                list = values.join(", ")
            )
        );
    }
    Ok((id, wanted.to_string()))
}

pub fn start(spec: SessionSpec, tx: Sender<AgentEvent>) -> Result<Box<dyn AgentControl>> {
    let (default_prog, args) = spec
        .kind
        .acp_command()
        .with_context(|| tr!("此 agent 不支援原生結構化協定"))?;
    let prog = match spec.bin.as_deref() {
        Some(b) => crate::which(b).with_context(|| tr!("找不到指定的執行檔 `{b}`", b = b))?,
        None => crate::which(default_prog)
            .with_context(|| tr!("找不到 `{prog}`", prog = default_prog))?,
    };

    let mut cmd = Command::new(&prog);
    cmd.args(args)
        .args(&spec.args)
        .current_dir(&spec.cwd)
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
        .with_context(|| tr!("無法啟動 `{prog} acp`", prog = prog.display()))?;

    let stdin = child.stdin.take().with_context(|| tr!("取得 stdin 失敗"))?;
    let stderr_tail = child.stderr.take().map(crate::stderr::StderrTail::spawn);
    let stdout = child
        .stdout
        .take()
        .with_context(|| tr!("取得 stdout 失敗"))?;

    let quota_prog = if spec.kind == termade_core::AgentKind::Antigravity {
        Some((prog.clone(), spec.env.clone()))
    } else {
        None
    };

    let conn = Arc::new(Conn {
        agent: spec.kind.id().to_string(),
        limited: std::sync::atomic::AtomicBool::new(false),
        quota_prog,
        quota_refreshing: std::sync::atomic::AtomicBool::new(false),
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
            // 用 try_wait 輪詢 (而非阻塞 wait)，否則會一直握著鎖，讓 `kill()` 永遠拿不到鎖而死鎖。
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
        Some(exe) => {
            let mut args = vec!["mcp".to_string(), "--session".to_string(), spec.id.clone()];
            if let Some((_, sock)) = spec.env.iter().find(|(k, _)| k == "TERMADE_SOCKET") {
                args.push("--socket".to_string());
                args.push(sock.clone());
            }
            json!([{
                "name": "termade",
                "command": exe.to_string_lossy(),
                "args": args,
                "env": []
            }])
        }
        None => json!([]),
    };
    // 只有宣告 `loadSession` 的 agent 之後才接得回來。
    let can_load = init["agentCapabilities"]["loadSession"]
        .as_bool()
        .unwrap_or(false);
    let (sid, new) = match &spec.resume {
        Some(id) => {
            if !can_load {
                bail!(
                    "{}",
                    tr!(
                        "{kind} 沒有宣告 loadSession，無法接續對話",
                        kind = spec.kind
                    )
                );
            }
            conn.loading.store(true, Ordering::SeqCst);
            let loaded = conn.call(
                "session/load",
                json!({ "sessionId": id, "cwd": spec.cwd.to_string_lossy(), "mcpServers": mcp_servers }),
                LOAD_TIMEOUT,
            );
            conn.loading.store(false, Ordering::SeqCst);
            // 重播的片段可能有一部分已進了行緩衝，丟掉。
            let _ = conn
                .stream
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .flush();
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
                .with_context(|| tr!("`session/new` 未回傳 sessionId"))?
                .to_string();
            (sid, new)
        }
    };
    *conn.acp_session.lock().unwrap_or_else(|e| e.into_inner()) = sid.clone();
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
        .with_context(|| tr!("設定 model `{value}` 失敗", value = value))?;
        let _ = tx.send(AgentEvent::Model(value));
    }
    if let Some(e) = spec.effort.as_ref().filter(|_| has_options) {
        let (config_id, value) = pick_effort_option(&new, e)?;
        conn.call(
            "session/set_config_option",
            json!({ "sessionId": sid, "configId": config_id, "value": value }),
            CALL_TIMEOUT,
        )
        .with_context(|| tr!("設定 effort `{value}` 失敗", value = value))?;
    }
    let _ = tx.send(AgentEvent::Status(SessionStatus::Idle));
    conn.refresh_quota_async();

    let control = AcpControl {
        conn,
        child,
        options: Mutex::new(json!({ "configOptions": new["configOptions"] })),
    };

    if let Some(p) = &spec.initial_prompt {
        control.send_input_with(p, &spec.initial_attachments)?;
    }

    Ok(Box::new(control))
}

#[cfg(test)]
mod model_tests {
    use super::*;

    #[test]
    fn extracts_request_id_with_safe_fallback() {
        assert_eq!(extract_request_id(&json!({"id": 123})), json!(123));
        assert_eq!(extract_request_id(&json!({"id": "req-1"})), json!("req-1"));
        assert_eq!(extract_request_id(&json!({"id": null})), Value::Null);
        assert_eq!(
            extract_request_id(&json!({"method": "session/update"})),
            Value::Null
        );
        assert_eq!(extract_request_id(&json!(42)), Value::Null);
        assert_eq!(extract_request_id(&json!([])), Value::Null);
    }

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

    #[test]
    fn tool_updates_only_report_final_states() {
        assert_eq!(tool_update_ok(&json!({"status":"pending"})), None);
        assert_eq!(tool_update_ok(&json!({"status":"in_progress"})), None);
        assert_eq!(tool_update_ok(&json!({})), None);
        assert_eq!(tool_update_ok(&json!({"status":"completed"})), Some(true));
        assert_eq!(tool_update_ok(&json!({"status":"failed"})), Some(false));
    }

    #[test]
    fn tool_result_text_unwraps_acp_content() {
        let u = json!({"content":[{"type":"content","content":{"type":"text","text":"a\nb"}}]});
        assert_eq!(tool_result_text(&u).as_deref(), Some("a\nb"));

        let u = json!({"content":[
            {"type":"content","content":{"type":"text","text":"x"}},
            {"type":"content","content":{"type":"text","text":"y"}}]});
        assert_eq!(tool_result_text(&u).as_deref(), Some("x\ny"));

        assert_eq!(tool_result_text(&json!({})), None);
        // diff 內容沒有純文字：不要硬印 JSON。
        assert_eq!(
            tool_result_text(&json!({"content":[{"type":"diff","path":"a.rs"}]})),
            None
        );
        // 沒有 content 時退回 rawOutput。
        assert_eq!(
            tool_result_text(&json!({"rawOutput":{"exit":0}})).as_deref(),
            Some("{\"exit\":0}")
        );
    }

    #[test]
    fn long_tool_output_is_truncated() {
        let s = "x".repeat(10);
        assert_eq!(truncate_chars(&s, 4), "xxxx…");
        assert_eq!(truncate_chars("abc", 4), "abc");
    }
}

const QUOTA_QUERY_TIMEOUT: Duration = Duration::from_secs(15);

/// 執行 `agy --output-format json -p /quota` 取得原始 JSON；失敗或逾時回 `None`。
fn query_agy_quota(prog: &std::path::Path, env: &[(String, String)]) -> Option<String> {
    let mut cmd = Command::new(prog);
    cmd.args(["--output-format", "json", "-p", "/quota"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = std::time::Instant::now() + QUOTA_QUERY_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        use std::io::Read;
        so.read_to_string(&mut out).ok()?;
    }
    Some(out)
}

/// 解析 `agy --output-format json -p /quota` 的輸出為 `Quota`。
pub(crate) fn parse_agy_quota_json(raw: &str, reached: bool) -> Option<termade_core::Quota> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    let groups = v["command"]["data"]["groups"].as_array()?;
    let mut windows = Vec::new();
    for g in groups {
        let group_name = g["name"].as_str().unwrap_or("");
        let Some(buckets) = g["buckets"].as_array() else {
            continue;
        };
        for b in buckets {
            let Some(remaining) = b["remaining_fraction"].as_f64() else {
                continue;
            };
            let id = b["id"].as_str().unwrap_or("");
            let win_raw = b["window"].as_str().unwrap_or("");
            let name = agy_window_name(group_name, id, win_raw);
            let used_percent = ((1.0 - remaining) * 100.0).clamp(0.0, 100.0) as f32;
            let resets_at_ms = b["reset_time"].as_str().and_then(parse_rfc3339_utc_ms);
            windows.push(termade_core::QuotaWindow {
                name,
                used_percent,
                resets_at_ms,
            });
        }
    }
    if windows.is_empty() {
        return None;
    }
    windows.sort_by_key(|w| {
        let rank = match w.name.as_str() {
            "5h" => 0,
            "7d" => 1,
            n if n.starts_with("argon") => 2,
            "3p-5h" => 3,
            "3p-7d" => 4,
            _ => 5,
        };
        (rank, w.name.clone())
    });
    let primary_exhausted = windows
        .iter()
        .any(|w| matches!(w.name.as_str(), "5h" | "7d") && w.used_percent >= 100.0);
    Some(termade_core::Quota {
        agent: "antigravity".into(),
        windows,
        reached: reached || primary_exhausted,
        warning: false,
        updated_ms: termade_core::now_millis(),
    })
}

fn agy_window_name(group_name: &str, id: &str, window: &str) -> String {
    if id == "gemini-5h" || (group_name == "Gemini Models" && window == "5h") {
        return "5h".into();
    }
    if id == "gemini-weekly" || (group_name == "Gemini Models" && window == "weekly") {
        return "7d".into();
    }
    if id.contains("argon") || group_name.contains("Argon") {
        return match window {
            "5h" => "argon-5h".into(),
            "weekly" => "argon-7d".into(),
            other if !other.is_empty() => format!("argon-{other}"),
            _ => "argon".into(),
        };
    }
    if id.starts_with("3p-") || group_name.contains("Claude") {
        return match window {
            "5h" => "3p-5h".into(),
            "weekly" => "3p-7d".into(),
            _ => id.into(),
        };
    }
    match window {
        "weekly" => "7d".into(),
        other if !other.is_empty() => other.into(),
        _ => id.into(),
    }
}

/// 解析 UTC RFC 3339 時間字串 (`YYYY-MM-DDTHH:MM:SSZ` 或帶小數秒) 為 epoch 毫秒。
pub(crate) fn parse_rfc3339_utc_ms(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    if dp.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let hms = time.split('.').next()?;
    let mut tp = hms.split(':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let ss: i64 = tp.next()?.parse().ok()?;
    if tp.next().is_some()
        || !(0..=23).contains(&hh)
        || !(0..=59).contains(&mm)
        || !(0..=60).contains(&ss)
    {
        return None;
    }
    // Howard Hinnant 的 civil-from-days 反推：轉成 Unix epoch 天數。
    let y_adj = y - if m <= 2 { 1 } else { 0 };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj.rem_euclid(400);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86_400 + hh * 3_600 + mm * 60 + ss;
    u64::try_from(secs).ok()?.checked_mul(1_000)
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    #[test]
    fn detects_rate_limit_errors() {
        let samples = [
            r#"`session/prompt` 失敗：{"code":-32000,"message":"RESOURCE_EXHAUSTED: Quota exceeded for metric"}"#,
            r#"{"code":429,"message":"Too Many Requests"}"#,
            r#"{"message":"You have hit the rate limit"}"#,
            r#"{"sessionUpdate":"error","error":{"message":"HTTP 429"}}"#,
        ];
        for s in samples {
            let q = rate_limit_quota("antigravity", s).expect(s);
            assert!(q.reached);
            assert_eq!(q.agent, "antigravity");
            assert!(q.windows.is_empty());
        }
    }

    #[test]
    fn recovery_clears_limit_once() {
        let flag = std::sync::atomic::AtomicBool::new(false);
        assert!(recovered_quota("antigravity", &flag).is_none());
        flag.store(true, Ordering::SeqCst);
        let q = recovered_quota("antigravity", &flag).expect("reset");
        assert!(!q.reached && !q.warning && q.windows.is_empty());
        assert_eq!(q.agent, "antigravity");
        assert!(recovered_quota("antigravity", &flag).is_none());
    }

    #[test]
    fn ignores_other_errors() {
        for s in [
            r#"{"code":-32603,"message":"internal error"}"#,
            r#"{"message":"tokens used: 14290"}"#,
            r#"{"message":"invalid params"}"#,
        ] {
            assert!(rate_limit_quota("antigravity", s).is_none(), "{s}");
        }
    }

    #[test]
    fn parses_rfc3339_utc_ms() {
        assert_eq!(parse_rfc3339_utc_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_utc_ms("2026-11-19T15:43:56Z"),
            Some(1_795_103_036_000)
        );
        assert_eq!(
            parse_rfc3339_utc_ms("2026-11-19T15:43:56.123Z"),
            Some(1_795_103_036_000)
        );
        assert!(parse_rfc3339_utc_ms("2026-11-19T15:43:56+08:00").is_none());
    }

    #[test]
    fn parses_agy_quota_json_sample() {
        let sample = r#"{
            "conversation_id": "",
            "status": "SUCCESS",
            "command": {
                "name": "usage",
                "data": {
                    "groups": [
                        {
                            "name": "Gemini Models",
                            "buckets": [
                                {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.75, "reset_time": "2026-11-25T13:49:39Z"},
                                {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.93, "reset_time": "2026-11-19T15:43:56Z"}
                            ]
                        },
                        {
                            "name": "Claude and GPT models",
                            "buckets": [
                                {"id": "3p-weekly", "window": "weekly", "remaining_fraction": 1, "reset_time": "2026-11-26T10:58:54Z"},
                                {"id": "3p-5h", "window": "5h", "remaining_fraction": 1, "reset_time": "2026-11-19T15:58:54Z"}
                            ]
                        },
                        {
                            "name": "Gemini 4 Argon",
                            "buckets": [
                                {"id": "gemini-argon-wtu-weekly", "window": "weekly", "remaining_fraction": 0.78, "reset_time": "2026-11-25T13:41:45Z"}
                            ]
                        }
                    ]
                }
            }
        }"#;
        let q = parse_agy_quota_json(sample, false).expect("valid agy quota");
        assert_eq!(q.agent, "antigravity");
        assert!(!q.reached);
        assert_eq!(q.windows.len(), 5);
        assert_eq!(q.windows[0].name, "5h");
        assert!((q.windows[0].used_percent - 7.0).abs() < 0.01);
        assert_eq!(q.windows[1].name, "7d");
        assert!((q.windows[1].used_percent - 25.0).abs() < 0.01);
        assert_eq!(q.windows[2].name, "argon-7d");
        assert_eq!(q.windows[3].name, "3p-5h");
        assert_eq!(q.windows[4].name, "3p-7d");
        assert_eq!(q.windows[0].resets_at_ms, Some(1_795_103_036_000));
    }

    #[test]
    fn marks_reached_when_primary_window_exhausted() {
        let sample = r#"{"command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-5h","window":"5h","remaining_fraction":0.0}]}]}}}"#;
        let q = parse_agy_quota_json(sample, false).expect("exhausted");
        assert!(q.reached);
        assert!((q.windows[0].used_percent - 100.0).abs() < 0.01);
        assert!(parse_agy_quota_json("not json", false).is_none());
    }
}
