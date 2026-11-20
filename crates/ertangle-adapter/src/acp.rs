//! Native Agent Client Protocol adapter.
//!
//! M0 used for OpenCode (`opencode acp`), which is the only one among four agents that provides a structured
//! protocol natively with zero extra dependencies, and was therefore implemented as the first structured adapter.
//!
//! ACP subset covered: `initialize`, `session/new`, `session/load` (resumption), `session/prompt`,
//! `session/cancel`,
//! as well as server→client `session/update` (streaming) and `session/request_permission` (approvals).

use crate::stream::StreamBuf;
use crate::{AgentControl, AgentEvent, SessionSpec};
use anyhow::{anyhow, bail, Context, Result};
use ertangle_core::{new_id, Decision, PermissionRequest, SessionStatus};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tr::tr;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3600);
/// `session/load` must wait for the agent to replay the entire history, which takes longer than a normal call for long conversations.
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// Collected answers for a multi-field elicitation: field key -> (multi-select, answers).
type ElicitationAnswers = Arc<Mutex<HashMap<String, (bool, Vec<String>)>>>;

/// Pending question state for ACP elicitation or tool-call fallback.
enum PendingAcpQuestion {
    Elicitation {
        rpc_id: Value,
        key: String,
        multi_select: bool,
        total: usize,
        answers: ElicitationAnswers,
    },
    ToolCall {
        total: usize,
        answers: Arc<Mutex<Vec<String>>>,
    },
}

/// Builds form content that conforms to the ACP elicitation schema.
///
/// Single-select fields carry a scalar string; only multi-select fields carry an array.
fn elicitation_accept_content(entries: &[(String, bool, Vec<String>)]) -> Value {
    let mut content = serde_json::Map::new();
    for (key, multi_select, answers) in entries {
        let value = if !multi_select && answers.len() == 1 {
            json!(answers[0])
        } else {
            json!(answers)
        };
        content.insert(key.clone(), value);
    }
    Value::Object(content)
}

struct Conn {
    /// Agent ID represented by this adapter (`agent` field in quota events).
    agent: String,
    /// Quota marked as rate limited with `reached=true`, not yet cleared by a successful turn.
    limited: std::sync::atomic::AtomicBool,
    /// Antigravity-specific: executable and environment variables for `agy --output-format json -p /quota`.
    quota_prog: Option<(std::path::PathBuf, Vec<(String, String)>)>,
    /// Prevents running multiple quota query background processes concurrently.
    quota_refreshing: std::sync::atomic::AtomicBool,
    /// Our own session ID (for event correlation).
    session_id: String,
    /// Agent's ACP session ID (for prompt / cancel).
    acp_session: Mutex<String>,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, Sender<Value>>>,
    /// Approvals: internal req id -> (JSON-RPC id, options)
    perms: Mutex<HashMap<String, (Value, Vec<Value>)>>,
    /// Interactive questions: internal qst id -> pending question state
    questions: Mutex<HashMap<String, PendingAcpQuestion>>,
    /// Line buffer for streaming output (combines token-level chunks into complete lines).
    stream: Mutex<StreamBuf>,
    /// Collaboration instructions prepended to the first prompt (cleared once taken).
    prepend_once: Mutex<Option<String>>,
    /// Agent declared image block support during initialize (`promptCapabilities.image`).
    image_ok: std::sync::atomic::AtomicBool,
    /// `session/load` in progress: the agent replays history via `session/update`; updates during this period are discarded,
    /// otherwise the transcript would duplicate. Cleared by the reader upon receiving the load response (replays always precede the response).
    loading: std::sync::atomic::AtomicBool,
    /// Permission policy: directly auto-approved by adapter when `Auto` / `Bypass`.
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

    /// Sends a prompt and waits for the entire turn to complete.
    fn prompt(&self, text: &str, atts: &[ertangle_core::Attachment]) -> Result<()> {
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

    /// If the error text indicates rate limiting, emits `reached=true` and sets flag (subsequent successful turns will clear it).
    fn report_rate_limit(&self, error_text: &str) {
        if let Some(q) = rate_limit_quota(&self.agent, error_text) {
            self.limited.store(true, Ordering::SeqCst);
            let _ = self.tx.send(AgentEvent::Quota(q));
        }
    }

    /// Asynchronously queries Antigravity quota (non-blocking), running at most one query at a time.
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
                // ACP: `used` (current context tokens) and `size` (window size).
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
                if crate::question::is_question_tool(&name) {
                    let questions =
                        crate::question::parse_tool_call_questions(&self.session_id, &name, &input);
                    if !questions.is_empty() {
                        let total = questions.len();
                        let shared_answers = Arc::new(Mutex::new(Vec::new()));
                        let mut q_map = self.questions.lock().unwrap_or_else(|e| e.into_inner());
                        for q in questions {
                            q_map.insert(
                                q.id.clone(),
                                PendingAcpQuestion::ToolCall {
                                    total,
                                    answers: Arc::clone(&shared_answers),
                                },
                            );
                            let _ = self.tx.send(AgentEvent::Question(q));
                        }
                    }
                }
                let _ = self.tx.send(AgentEvent::ToolCall {
                    name,
                    input,
                    ok: None,
                    output: None,
                });
            }
            "tool_call_update" => {
                // ACP reports the same tool incrementally with pending/in_progress; only completed/failed
                // is the final result. Previously "not completed" was treated as failure, causing successful
                // shell calls to emit an extra `✗ failed` line.
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

    /// Flushes remaining buffered lines (called before switching event types or tool calls).
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

        // Always forward to daemon: rule engine and session policy (auto/bypass auto-approval) are evaluated there.
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

    fn on_elicitation(&self, msg: &Value) {
        let jid = extract_request_id(msg);
        let params = &msg["params"];
        let list = crate::question::parse_acp_elicitations(&self.session_id, params);
        if list.is_empty() {
            self.respond_error(&jid, -32602, "invalid elicitation params");
            return;
        }
        let total = list.len();
        let shared_answers = Arc::new(Mutex::new(HashMap::new()));
        let mut q_map = self.questions.lock().unwrap_or_else(|e| e.into_inner());
        for (q, key) in list {
            let mut answers = shared_answers.lock().unwrap_or_else(|e| e.into_inner());
            answers.insert(key.clone(), (q.multi_select, Vec::new()));
            drop(answers);
            q_map.insert(
                q.id.clone(),
                PendingAcpQuestion::Elicitation {
                    rpc_id: jid.clone(),
                    key,
                    multi_select: q.multi_select,
                    total,
                    answers: Arc::clone(&shared_answers),
                },
            );
            let _ = self.tx.send(AgentEvent::Question(q));
        }
    }

    fn resolve_question(&self, id: &str, answers: &[String]) -> Result<()> {
        let entry = self
            .questions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let Some(pending) = entry else {
            bail!("{}", tr!("未知的問題請求：{id}", id = id));
        };
        match pending {
            PendingAcpQuestion::Elicitation {
                rpc_id,
                key,
                multi_select,
                total,
                answers: shared_answers,
            } => {
                if answers.is_empty() {
                    let mut q_map = self.questions.lock().unwrap_or_else(|e| e.into_inner());
                    q_map.retain(|_, v| match v {
                        PendingAcpQuestion::Elicitation { rpc_id: r, .. } => r != &rpc_id,
                        _ => true,
                    });
                    drop(q_map);

                    self.respond(&rpc_id, json!({ "action": "decline" }));
                } else {
                    let mut map = shared_answers.lock().unwrap_or_else(|e| e.into_inner());
                    map.insert(key, (multi_select, answers.to_vec()));
                    if map.len() == total {
                        let mut q_map = self.questions.lock().unwrap_or_else(|e| e.into_inner());
                        q_map.retain(|_, v| match v {
                            PendingAcpQuestion::Elicitation { rpc_id: r, .. } => r != &rpc_id,
                            _ => true,
                        });
                        drop(q_map);

                        let entries: Vec<(String, bool, Vec<String>)> = map
                            .iter()
                            .map(|(k, (multi, ans))| (k.clone(), *multi, ans.clone()))
                            .collect();
                        drop(map);

                        self.respond(
                            &rpc_id,
                            json!({
                                "action": "accept",
                                "content": elicitation_accept_content(&entries)
                            }),
                        );
                    }
                }
            }
            PendingAcpQuestion::ToolCall {
                total,
                answers: shared_answers,
            } => {
                if answers.is_empty() {
                    return Ok(());
                }
                let mut list = shared_answers.lock().unwrap_or_else(|e| e.into_inner());
                list.extend(answers.iter().cloned());
                if list.len() >= total {
                    let ans_str = list.join("\n");
                    let _ = self.tx.send(AgentEvent::Output(format!("↳ {ans_str}\n")));
                }
            }
        }
        Ok(())
    }
}

/// Translates normalized [`Decision`] to ACP `outcome`.
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

/// Status of `tool_call_update`: `Some(true)` succeeded, `Some(false)` failed, `None` progress only.
fn tool_update_ok(update: &Value) -> Option<bool> {
    match update["status"].as_str().unwrap_or("") {
        "completed" => Some(true),
        "failed" => Some(false),
        _ => None,
    }
}

/// Extracts plain text from ACP `content` (`ToolCallContent::Content`) to avoid showing JSON envelopes in transcripts.
/// Falls back to `rawOutput` when no text content is available.
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

/// Truncates excessively long tool output to prevent transcripts and event logs from ballooning.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Whether error text indicates rate limiting / quota exhaustion (`RESOURCE_EXHAUSTED`, HTTP 429, `quota exceeded`, `rate limit`).
fn is_rate_limit_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("resource_exhausted")
        || lower.contains("quota exceeded")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
    {
        return true;
    }
    // Standalone `429` (surrounded by non-digits) to avoid matching `4290` etc.
    let b = lower.as_bytes();
    lower.match_indices("429").any(|(i, _)| {
        (i == 0 || !b[i - 1].is_ascii_digit()) && b.get(i + 3).is_none_or(|c| !c.is_ascii_digit())
    })
}

/// Rate limit error -> "quota reached" `Quota` (no percentage, only `reached` flag); returns `None` for other errors.
fn rate_limit_quota(agent: &str, error_text: &str) -> Option<ertangle_core::Quota> {
    is_rate_limit_error(error_text).then(|| ertangle_core::Quota {
        agent: agent.to_string(),
        windows: Vec::new(),
        reached: true,
        warning: false,
        updated_ms: ertangle_core::now_millis(),
    })
}

/// After a successful turn: if previously rate-limited, emits recovered (`reached=false`) quota and clears flag; otherwise `None`.
fn recovered_quota(
    agent: &str,
    limited: &std::sync::atomic::AtomicBool,
) -> Option<ertangle_core::Quota> {
    limited
        .swap(false, Ordering::SeqCst)
        .then(|| ertangle_core::Quota {
            agent: agent.to_string(),
            windows: Vec::new(),
            reached: false,
            warning: false,
            updated_ms: ertangle_core::now_millis(),
        })
}

/// Safely extracts ID from a JSON-RPC message; returns safe fallback (`Value::Null`) if missing or null.
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
                    "elicitation/create" => conn.on_elicitation(&msg),
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
            // The only request awaited during loading is `session/load`; clearing flag on the same reader thread
            // ensures subsequent updates are not mistakenly dropped.
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
    /// Most recently reported `configOptions` (wrapped as `{"configOptions": [...]}`), used to validate and find configId during runtime switches.
    options: Mutex<Value>,
}

impl AgentControl for AcpControl {
    fn send_input(&self, text: &str) -> Result<()> {
        self.send_input_with(text, &[])
    }

    fn send_input_with(&self, text: &str, atts: &[ertangle_core::Attachment]) -> Result<()> {
        let conn = self.conn.clone();
        let text = text.to_string();
        let atts = atts.to_vec();
        std::thread::spawn(move || {
            if let Err(e) = conn.prompt(&text, &atts) {
                tracing::warn!("ACP prompt 失敗：{e}");
                if ertangle_core::looks_like_limit(&e.to_string()) {
                    let _ = conn.tx.send(AgentEvent::Limited(e.to_string()));
                }
                conn.report_rate_limit(&e.to_string());
            }
            conn.refresh_quota_async();
        });
        Ok(())
    }

    /// `session/set_config_option`; response includes complete `configOptions`, also updating the cache.
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

    fn resolve_question(&self, id: &str, answers: &[String]) -> Result<()> {
        self.conn.resolve_question(id, answers)
    }

    fn transport(&self) -> &'static str {
        "acp"
    }
}

/// Takes the one-time collaboration prefix to prepend before the first prompt.
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

/// ACP prompt blocks: text (including inlined text files); adds image blocks if agent supports images, otherwise describes images as a text line.
fn prompt_blocks(text: &str, atts: &[ertangle_core::Attachment], image_ok: bool) -> Value {
    let mut blocks =
        vec![json!({ "type": "text", "text": ertangle_core::compose_text(text, atts, !image_ok) })];
    if image_ok {
        for a in atts.iter().filter(|a| a.is_image()) {
            if let Some(data) = &a.data {
                blocks.push(json!({ "type": "image", "data": data, "mimeType": a.mime }));
            }
        }
    }
    Value::Array(blocks)
}

/// Finds model option from `session/new` response `configOptions` and validates `wanted`.
/// Returns `(configId, value)`; returns a clear error if the agent reported no model options or the value is not in the list.
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
    // Options may be flat [{value,name}] or grouped [{options:[...]}]
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

/// Finds reasoning effort option from `session/new` `configOptions` (id or category containing effort/reasoning/thought).
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

    let quota_prog = if spec.kind == ertangle_core::AgentKind::Antigravity {
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
        questions: Mutex::new(HashMap::new()),
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
            // Poll with try_wait (rather than blocking wait) to avoid holding the lock indefinitely, which would deadlock `kill()`.
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

    // Handshake.
    let init = conn.call(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "elicitation": { "form": {} }
            }
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
            if let Some((_, sock)) = spec.env.iter().find(|(k, _)| k == "ERTANGLE_SOCKET") {
                args.push("--socket".to_string());
                args.push(sock.clone());
            }
            json!([{
                "name": "ertangle",
                "command": exe.to_string_lossy(),
                "args": args,
                "env": []
            }])
        }
        None => json!([]),
    };
    // Only agents declaring `loadSession` can be reconnected later.
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
            // Part of the replayed chunks might have entered the stream buffer; discard.
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
    // When resuming, model/effort already exist in agent conversation; do not reset if response lacks configOptions.
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
    fn elicitation_content_uses_scalar_for_single_select() {
        let entries = vec![
            ("strategy".to_string(), false, vec!["balanced".to_string()]),
            (
                "tags".to_string(),
                true,
                vec!["api".to_string(), "ui".to_string()],
            ),
        ];
        assert_eq!(
            elicitation_accept_content(&entries),
            json!({ "strategy": "balanced", "tags": ["api", "ui"] })
        );
        assert_eq!(
            elicitation_accept_content(&[("custom".to_string(), false, Vec::new())]),
            json!({ "custom": [] })
        );
    }

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
        let p = Mutex::new(Some("[ertangle 協作說明]\n規則\n".to_string()));
        let first = take_effective_prompt(&p, "做任務 A");
        assert!(first.starts_with("[ertangle 協作說明]\n規則\n\n做任務 A"));
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
        // Diff content has no plain text: do not print raw JSON.
        assert_eq!(
            tool_result_text(&json!({"content":[{"type":"diff","path":"a.rs"}]})),
            None
        );
        // Fall back to rawOutput when content is absent.
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

/// Executes `agy --output-format json -p /quota` to obtain raw JSON; returns `None` on failure or timeout.
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

/// Parses output of `agy --output-format json -p /quota` into `Quota`.
pub(crate) fn parse_agy_quota_json(raw: &str, reached: bool) -> Option<ertangle_core::Quota> {
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
            windows.push(ertangle_core::QuotaWindow {
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
    Some(ertangle_core::Quota {
        agent: "antigravity".into(),
        windows,
        reached: reached || primary_exhausted,
        warning: false,
        updated_ms: ertangle_core::now_millis(),
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

/// Parses UTC RFC 3339 timestamp string (`YYYY-MM-DDTHH:MM:SSZ` or with fractional seconds) to epoch milliseconds.
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
    // Howard Hinnant's civil-from-days inverse: convert to days since Unix epoch.
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
