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
use std::time::{Duration, Instant};
use tr::tr;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3600);
/// `session/load` must wait for the agent to replay the entire history, which takes longer than a normal call for long conversations.
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Some ACP agents (e.g. agy) keep streaming `session/load` history replay after the load response.
/// The replay is considered finished once no `session/update` has arrived for this long.
const REPLAY_QUIET: Duration = Duration::from_millis(800);
/// Upper bound on waiting for the post-load replay to go quiet.
const REPLAY_MAX_WAIT: Duration = Duration::from_secs(20);
/// A turn with no `session/update` for this long is reported as possibly stalled.
/// (agy has been observed accepting `session/prompt` and then never calling the model.)
const STALL_AFTER: Duration = Duration::from_secs(180);
/// How often the stall watchdog checks in-flight turns.
const STALL_POLL: Duration = Duration::from_secs(15);

/// Tracks in-flight `session/prompt` turns.
///
/// ACP agents such as agy accept a second `session/prompt` while one is still running and may
/// answer it immediately; status must only return to Idle once *every* turn has finished,
/// otherwise a hung first turn is hidden behind a false Idle.
#[derive(Debug, Default)]
struct TurnTracker {
    inflight: usize,
    /// When the oldest still-running turn started (reset when all turns finish).
    started: Option<Instant>,
    /// When the stall warning was last emitted for the current quiet period.
    warned_at: Option<Instant>,
}

impl TurnTracker {
    fn begin(&mut self, now: Instant) {
        if self.inflight == 0 {
            self.started = Some(now);
        }
        self.inflight = self.inflight.saturating_add(1);
    }

    /// Ends one turn. Returns `None` while other turns are still running, otherwise
    /// `Some(warned)` where `warned` tells whether a stall warning should be cleared.
    fn end(&mut self) -> Option<bool> {
        self.inflight = self.inflight.saturating_sub(1);
        if self.inflight > 0 {
            return None;
        }
        self.started = None;
        Some(self.warned_at.take().is_some())
    }

    /// Returns how long the agent has been silent if a turn is running, has been quiet for at
    /// least `threshold`, and no warning was issued yet for this quiet period.
    fn stalled(
        &mut self,
        now: Instant,
        last_update: Instant,
        threshold: Duration,
    ) -> Option<Duration> {
        let started = self.started?;
        let quiet_since = started.max(last_update);
        let quiet = now.saturating_duration_since(quiet_since);
        if quiet < threshold || self.warned_at.is_some_and(|w| w >= quiet_since) {
            return None;
        }
        self.warned_at = Some(now);
        Some(quiet)
    }
}

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
        /// Maps a displayed option label back to the value required by the agent schema.
        option_values: HashMap<String, String>,
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
            answers
                .first()
                .map(|a| json!(a))
                .unwrap_or_else(|| json!(""))
        } else {
            json!(answers)
        };
        content.insert(key.clone(), value);
    }
    Value::Object(content)
}

/// Builds the response for a completed elicitation. Skipped fields are omitted so a
/// companion "Other" field cannot discard a real selection; a fully skipped form is declined.
fn elicitation_response(entries: &[(String, bool, Vec<String>)]) -> Value {
    let answered: Vec<_> = entries
        .iter()
        .filter(|(_, _, answers)| !answers.is_empty())
        .cloned()
        .collect();
    if answered.is_empty() {
        return json!({ "action": "decline" });
    }
    json!({
        "action": "accept",
        "content": elicitation_accept_content(&answered)
    })
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
    /// Notifies the idle-flush thread whenever a streaming chunk is buffered.
    stream_poke: Sender<()>,
    /// Collaboration instructions prepended to the first prompt (cleared once taken).
    prepend_once: Mutex<Option<String>>,
    /// Agent declared image block support during initialize (`promptCapabilities.image`).
    image_ok: std::sync::atomic::AtomicBool,
    /// `session/load` in progress: the agent replays history via `session/update`; updates during this period are discarded,
    /// otherwise the transcript would duplicate. Cleared once the replay has gone quiet after the load response
    /// (see [`wait_until_quiet`]), or at the latest when the next `session/prompt` is sent.
    loading: std::sync::atomic::AtomicBool,
    /// When the last `session/update` arrived (including discarded replay updates).
    last_update: Mutex<std::time::Instant>,
    /// In-flight prompt turns (see [`TurnTracker`]).
    turns: Mutex<TurnTracker>,
    /// Permission policy: directly auto-approved by adapter when `Auto` / `Bypass`.
    tx: Sender<AgentEvent>,
}

impl Conn {
    fn emit(&self, ev: AgentEvent) {
        // 寫入失敗代表接收端通道已關閉，無需處理
        let _ = self.tx.send(ev);
    }

    fn write(&self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        let mut w = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(line.as_bytes())?;
        w.flush()?;
        Ok(())
    }

    fn respond(&self, id: &Value, result: Value) {
        // 若 agent stdin 已關閉則忽略回應寫入錯誤
        let _ = self.write(&json!({"jsonrpc":"2.0","id":id,"result":result}));
    }

    fn respond_error(&self, id: &Value, code: i64, message: &str) {
        // 若 agent stdin 已關閉則忽略回應寫入錯誤
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
        // Any session/update arriving before the first session/prompt on a resumed session is
        // asynchronous history replay from session/load; stop discarding only once we prompt.
        self.loading.store(false, Ordering::SeqCst);
        self.turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .begin(Instant::now());
        self.emit(AgentEvent::Status(SessionStatus::Busy));
        let res = self.call(
            "session/prompt",
            json!({ "sessionId": sid, "prompt": prompt_blocks(&effective, atts, self.image_ok.load(Ordering::SeqCst)) }),
            PROMPT_TIMEOUT,
        );
        let ended = self.turns.lock().unwrap_or_else(|e| e.into_inner()).end();
        if let Some(warned) = ended {
            self.flush_stream();
            if warned {
                self.emit(AgentEvent::Note(String::new()));
            }
            self.emit(AgentEvent::Status(SessionStatus::Idle));
        }
        if res.is_ok() {
            if let Some(q) = recovered_quota(&self.agent, &self.limited) {
                self.emit(AgentEvent::Quota(q));
            }
        }
        res.map(|_| ())
    }

    /// Warns (once per quiet period) when a running turn has produced no `session/update` for
    /// [`STALL_AFTER`]. Skipped while waiting on the user for a permission or question.
    fn check_stall(&self, now: Instant) {
        let waiting_on_user = !self
            .perms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
            || !self
                .questions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty();
        if waiting_on_user {
            return;
        }
        let last = *self.last_update.lock().unwrap_or_else(|e| e.into_inner());
        let quiet = self
            .turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stalled(now, last, STALL_AFTER);
        let Some(quiet) = quiet else { return };
        self.flush_stream();
        let note = tr!(
            "agent 已 {mins} 分鐘沒有任何回應，可能卡住；可用 /cancel 中斷本回合後重送",
            mins = quiet.as_secs() / 60
        );
        self.emit(AgentEvent::Output(format!("⚠ {note}\n")));
        self.emit(AgentEvent::Note(note));
    }

    /// If the error text indicates rate limiting, emits `reached=true` and sets flag (subsequent successful turns will clear it).
    fn report_rate_limit(&self, error_text: &str) {
        if let Some(q) = rate_limit_quota(&self.agent, error_text) {
            self.limited.store(true, Ordering::SeqCst);
            self.emit(AgentEvent::Quota(q));
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
            // Safe Rust: 原子操作 store 保證不失敗，確保 Drop 不會 panic。
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _guard = Guard(&conn.quota_refreshing);
            if let Some(raw) = query_agy_quota(&prog, &env) {
                let reached = conn.limited.load(Ordering::SeqCst);
                if let Some(q) = parse_agy_quota_json(&raw, reached) {
                    conn.emit(AgentEvent::Quota(q));
                }
            }
        });
    }

    fn on_update(&self, msg: &Value) {
        let update = msg
            .get("params")
            .and_then(|p| p.get("update"))
            .unwrap_or(&Value::Null);
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
                self.emit(AgentEvent::Commands(names));
            }
            "usage_update" => {
                // ACP: `used` (current context tokens) and `size` (window size).
                if let Some(used) = update["used"].as_u64() {
                    self.emit(AgentEvent::Context {
                        used,
                        window: update["size"].as_u64(),
                    });
                }
            }
            "error" => {
                self.report_rate_limit(&update.to_string());
            }
            "agent_message_chunk" => {
                if let Some(t) = update
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                {
                    let chunk = self
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_chunk("msg", t);
                    self.send_chunk(chunk);
                    // 串流提示喚醒失敗則忽略
                    let _ = self.stream_poke.send(());
                }
            }
            "agent_thought_chunk" => {
                if let Some(t) = update
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                {
                    let chunk = self
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_chunk("think", t);
                    self.send_chunk(chunk);
                    // 串流提示喚醒失敗則忽略
                    let _ = self.stream_poke.send(());
                }
            }
            "tool_call" => {
                self.flush_stream();
                let name = tool_name(update);
                let raw_input = &update["rawInput"];
                let input = raw_input.to_string();
                // ACP emits an empty rawInput placeholder before the real elicitation request.
                // Creating a question here would show "{}" instead of the actual prompt.
                let empty_placeholder = raw_input.as_object().is_some_and(|obj| obj.is_empty())
                    || raw_input.as_str().is_some_and(str::is_empty);
                if !empty_placeholder && crate::question::is_question_tool(&name) {
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
                            self.emit(AgentEvent::Question(q));
                        }
                    }
                }
                self.emit(AgentEvent::ToolCall {
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
                self.emit(AgentEvent::ToolCall {
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
        let chunk = self
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .flush_chunk();
        self.send_chunk(chunk);
    }

    /// Flushes buffered text after a stream pause *without* a trailing newline, so the
    /// next chunk continues the same transcript line instead of splitting it mid-token.
    fn idle_flush_stream(&self) {
        let chunk = self
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .idle_flush_chunk();
        self.send_chunk(chunk);
    }

    fn send_chunk(&self, chunk: crate::stream::Chunk) {
        if chunk.text.is_empty() {
            return;
        }
        let ev = if chunk.append {
            AgentEvent::OutputAppend(chunk.text)
        } else {
            AgentEvent::Output(chunk.text)
        };
        self.emit(ev);
    }

    fn on_permission(&self, msg: &Value) {
        let jid = extract_request_id(msg);
        let params = &msg["params"];
        let options = params["options"].as_array().cloned().unwrap_or_default();

        // Always forward to daemon: rule engine and session policy (auto/bypass auto-approval) are evaluated there.
        let req = new_id("perm");
        let title = params
            .get("toolCall")
            .and_then(|tc| tc.get("title"))
            .and_then(|t| t.as_str())
            .unwrap_or("permission")
            .to_string();
        let detail = params
            .get("toolCall")
            .and_then(|tc| tc.get("rawInput"))
            .map_or_else(String::new, |v| v.to_string());
        self.perms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(req.clone(), (jid, options));
        self.emit(AgentEvent::Permission(PermissionRequest {
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
            let option_values = q
                .options
                .iter()
                .filter_map(|opt| opt.value.clone().map(|value| (opt.label.clone(), value)))
                .collect();
            q_map.insert(
                q.id.clone(),
                PendingAcpQuestion::Elicitation {
                    rpc_id: jid.clone(),
                    key,
                    multi_select: q.multi_select,
                    total,
                    answers: Arc::clone(&shared_answers),
                    option_values,
                },
            );
            self.emit(AgentEvent::Question(q));
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
                option_values,
            } => {
                // An empty answer means the human skipped this field, which is normal for
                // companion fields such as "Other". Keep it as an omitted field instead of
                // declining the whole form; a real selection in another field still wins.
                let submitted: Vec<String> = answers
                    .iter()
                    .map(|answer| {
                        option_values
                            .get(answer)
                            .cloned()
                            .unwrap_or_else(|| answer.clone())
                    })
                    .collect();
                let mut map = shared_answers.lock().unwrap_or_else(|e| e.into_inner());
                map.insert(key, (multi_select, submitted));
                if map.len() < total {
                    return Ok(());
                }
                let entries: Vec<(String, bool, Vec<String>)> = map
                    .iter()
                    .map(|(k, (multi, ans))| (k.clone(), *multi, ans.clone()))
                    .collect();
                drop(map);

                let mut q_map = self.questions.lock().unwrap_or_else(|e| e.into_inner());
                q_map.retain(|_, v| match v {
                    PendingAcpQuestion::Elicitation { rpc_id: r, .. } => r != &rpc_id,
                    _ => true,
                });
                drop(q_map);

                self.respond(&rpc_id, elicitation_response(&entries));
            }
            PendingAcpQuestion::ToolCall {
                total,
                answers: shared_answers,
            } => {
                let mut list = shared_answers.lock().unwrap_or_else(|e| e.into_inner());
                if answers.is_empty() {
                    // Preserve one slot per skipped question so a grouped request still completes.
                    list.push(String::new());
                } else {
                    list.extend(answers.iter().cloned());
                }
                if list.len() >= total {
                    let ans_str = list.join("\n");
                    self.emit(AgentEvent::Output(format!("↳ {ans_str}\n")));
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
    for item in update
        .get("content")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(t) = item
            .get("content")
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .or_else(|| item.get("text").and_then(|t| t.as_str()))
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
        i.checked_sub(1)
            .and_then(|prev| b.get(prev))
            .is_none_or(|c| !c.is_ascii_digit())
            && b.get(i + 3).is_none_or(|c| !c.is_ascii_digit())
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
                None if method == "session/update" => {
                    *conn.last_update.lock().unwrap_or_else(|e| e.into_inner()) =
                        std::time::Instant::now();
                    if !conn.loading.load(Ordering::SeqCst) {
                        conn.on_update(&msg);
                    }
                }
                None => {}
            }
        } else if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            if let Some(s) = conn
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id)
            {
                // 等待回應者若已逾時放棄則忽略傳送結果
                let _ = s.send(msg);
            }
        }
    }
    conn.emit(AgentEvent::Exited(None));
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
                    conn.emit(AgentEvent::Limited(e.to_string()));
                } else {
                    // Otherwise the failure is only in the daemon log and the turn looks silently dropped.
                    let note = tr!("ACP prompt 失敗：{err}", err = e);
                    conn.emit(AgentEvent::Output(format!("⚠ {note}\n")));
                    conn.emit(AgentEvent::Note(note));
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
            if let Some(opts) = r.get("configOptions").filter(|co| co.is_array()) {
                *self.options.lock().unwrap_or_else(|e| e.into_inner()) =
                    json!({ "configOptions": opts });
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
    for k in crate::env_removals(&spec.kind) {
        cmd.env_remove(k);
    }
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

    let (stream_poke, stream_rx) = mpsc::channel();
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
        stream_poke,
        prepend_once: Mutex::new(spec.system_append.clone().filter(|s| !s.trim().is_empty())),
        image_ok: std::sync::atomic::AtomicBool::new(false),
        loading: std::sync::atomic::AtomicBool::new(false),
        last_update: Mutex::new(std::time::Instant::now()),
        turns: Mutex::new(TurnTracker::default()),
        tx: tx.clone(),
    });

    {
        let weak = Arc::downgrade(&conn);
        std::thread::spawn(move || {
            // Exits once the connection is dropped (session ended).
            loop {
                std::thread::sleep(STALL_POLL);
                let Some(c) = weak.upgrade() else { return };
                c.check_stall(Instant::now());
            }
        });
    }

    {
        let weak = Arc::downgrade(&conn);
        std::thread::spawn(move || {
            // Flush trailing partial lines after 250ms of stream silence so internal
            // background-task turns without `conn.prompt()` don't leak into the next prompt.
            while stream_rx.recv().is_ok() {
                loop {
                    match stream_rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(()) => continue,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Some(c) = weak.upgrade() {
                                c.idle_flush_stream();
                            }
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        });
    }

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
            // 接收端若已關閉則忽略退出事件
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
        init.get("agentCapabilities")
            .and_then(|ac| ac.get("promptCapabilities"))
            .and_then(|pc| pc.get("image"))
            .and_then(|v| v.as_bool())
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
    let can_load = init
        .get("agentCapabilities")
        .and_then(|ac| ac.get("loadSession"))
        .and_then(|v| v.as_bool())
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
            // Some ACP agents (e.g. agy) stream `session/load` replay updates after returning the response.
            // Keep discarding until the replay goes quiet; otherwise it lands after the initial prompt
            // and buries the user's message under the old conversation.
            *conn.last_update.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
            wait_until_quiet(
                || *conn.last_update.lock().unwrap_or_else(|e| e.into_inner()),
                REPLAY_QUIET,
                REPLAY_MAX_WAIT,
            );
            conn.loading.store(false, Ordering::SeqCst);
            // 等待重播結束後刷新緩衝區，若無資料則忽略
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
            let sid = new
                .get("sessionId")
                .and_then(|v| v.as_str())
                .with_context(|| tr!("`session/new` 未回傳 sessionId"))?
                .to_string();
            (sid, new)
        }
    };
    *conn.acp_session.lock().unwrap_or_else(|e| e.into_inner()) = sid.clone();
    if can_load {
        // 接收端若已關閉則忽略事件傳送
        let _ = tx.send(AgentEvent::NativeId(sid.clone()));
    }
    // When resuming, model/effort already exist in agent conversation; do not reset if response lacks configOptions.
    let has_options =
        spec.resume.is_none() || new.get("configOptions").is_some_and(|co| co.is_array());
    if let Some(wanted) = spec.model.as_ref().filter(|_| has_options) {
        let (config_id, value) = pick_model_option(&new, wanted)?;
        conn.call(
            "session/set_config_option",
            json!({ "sessionId": sid, "configId": config_id, "value": value }),
            CALL_TIMEOUT,
        )
        .with_context(|| tr!("設定 model `{value}` 失敗", value = value))?;
        // 接收端若已關閉則忽略事件傳送
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
    let has_initial_prompt = spec
        .initial_prompt
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());
    // 接收端若已關閉則忽略事件傳送
    let _ = tx.send(AgentEvent::Status(if has_initial_prompt {
        SessionStatus::Busy
    } else {
        SessionStatus::Idle
    }));
    conn.refresh_quota_async();

    let control = AcpControl {
        conn,
        child,
        options: Mutex::new(
            json!({ "configOptions": new.get("configOptions").cloned().unwrap_or(Value::Null) }),
        ),
    };

    if let Some(p) = &spec.initial_prompt {
        control.send_input_with(p, &spec.initial_attachments)?;
    }

    Ok(Box::new(control))
}

/// Blocks until `last_activity()` is at least `quiet` old, or `max` has elapsed in total.
fn wait_until_quiet(
    last_activity: impl Fn() -> std::time::Instant,
    quiet: Duration,
    max: Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let idle = last_activity().elapsed();
        let waited = start.elapsed();
        if idle >= quiet || waited >= max {
            return;
        }
        std::thread::sleep((quiet - idle).min(max - waited));
    }
}

#[cfg(test)]
mod turn_tracker_tests {
    use super::*;

    const T: Duration = Duration::from_secs(180);

    #[test]
    fn idle_only_after_last_concurrent_turn_ends() {
        let mut t = TurnTracker::default();
        let now = Instant::now();
        t.begin(now);
        t.begin(now);
        assert_eq!(
            t.end(),
            None,
            "second turn finishing early must not report Idle"
        );
        assert_eq!(t.end(), Some(false));
    }

    #[test]
    fn end_without_begin_does_not_underflow() {
        let mut t = TurnTracker::default();
        assert_eq!(t.end(), Some(false));
    }

    #[test]
    fn no_stall_when_no_turn_running() {
        let mut t = TurnTracker::default();
        let now = Instant::now();
        assert_eq!(t.stalled(now + T * 2, now, T), None);
    }

    #[test]
    fn warns_once_per_quiet_period_and_rearms_after_activity() {
        let mut t = TurnTracker::default();
        let start = Instant::now();
        t.begin(start);
        assert_eq!(t.stalled(start + T / 2, start, T), None);
        assert_eq!(t.stalled(start + T, start, T), Some(T));
        assert_eq!(t.stalled(start + T * 2, start, T), None, "already warned");
        let activity = start + T * 3;
        assert_eq!(t.stalled(activity + T, activity, T), Some(T));
        assert_eq!(t.end(), Some(true), "warning must be cleared on Idle");
    }

    #[test]
    fn quiet_period_counts_from_turn_start_not_stale_update() {
        let mut t = TurnTracker::default();
        let old_update = Instant::now();
        let start = old_update + T * 5;
        t.begin(start);
        assert_eq!(t.stalled(start + T / 2, old_update, T), None);
    }
}

#[cfg(test)]
mod replay_wait_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn returns_once_activity_goes_quiet() {
        let start = Instant::now();
        // Activity keeps arriving for the first 150ms, then stops.
        let last = || {
            let now = Instant::now();
            if now.duration_since(start) < Duration::from_millis(150) {
                now
            } else {
                start + Duration::from_millis(150)
            }
        };
        wait_until_quiet(last, Duration::from_millis(100), Duration::from_secs(5));
        let took = start.elapsed();
        assert!(
            took >= Duration::from_millis(250),
            "returned too early: {took:?}"
        );
        assert!(took < Duration::from_secs(2), "waited too long: {took:?}");
    }

    #[test]
    fn gives_up_after_max_wait_when_activity_never_stops() {
        let start = Instant::now();
        wait_until_quiet(
            Instant::now,
            Duration::from_millis(100),
            Duration::from_millis(200),
        );
        let took = start.elapsed();
        assert!(
            took >= Duration::from_millis(200),
            "returned too early: {took:?}"
        );
        assert!(took < Duration::from_secs(2), "waited too long: {took:?}");
    }

    #[test]
    fn returns_immediately_when_already_quiet() {
        let past = Instant::now() - Duration::from_secs(1);
        let start = Instant::now();
        wait_until_quiet(|| past, Duration::from_millis(100), Duration::from_secs(5));
        assert!(start.elapsed() < Duration::from_millis(50));
    }
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
    fn elicitation_response_keeps_selection_when_other_field_is_skipped() {
        let entries = vec![
            ("A1".to_string(), false, vec!["Postgres".to_string()]),
            ("A2".to_string(), false, Vec::new()),
        ];
        assert_eq!(
            elicitation_response(&entries),
            json!({ "action": "accept", "content": { "A1": "Postgres" } })
        );
        assert_eq!(
            elicitation_response(&[("A2".to_string(), false, Vec::new())]),
            json!({ "action": "decline" })
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
    for k in crate::env_removals(&ertangle_core::AgentKind::Antigravity) {
        cmd.env_remove(k);
    }
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
                // 查詢額度逾時強制終止子處理程序並回收，忽略錯誤
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
    let groups = v
        .get("command")
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("groups"))
        .and_then(|g| g.as_array())?;
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
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hh.checked_mul(3_600)?)?
        .checked_add(mm.checked_mul(60)?)?
        .checked_add(ss)?;
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
                                {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.39, "reset_time": "2026-11-25T13:49:39Z"},
                                {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.93, "reset_time": "2026-11-19T15:43:56Z"}
                            ]
                        },
                        {
                            "name": "Claude and GPT models",
                            "buckets": [
                                {"id": "3p-weekly", "window": "weekly", "remaining_fraction": 0.10, "reset_time": "2026-11-26T10:58:54Z"},
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
        assert!((q.windows[1].used_percent - 61.0).abs() < 0.01);
        assert_eq!(q.windows[2].name, "argon-7d");
        assert_eq!(q.windows[3].name, "3p-5h");
        assert_eq!(q.windows[4].name, "3p-7d");
        assert_eq!(q.windows[0].resets_at_ms, Some(1_795_103_036_000));
        assert_eq!(q.max_used(), 61.0);
        assert_eq!(q.max_used_live(0), 61.0);
        assert!(!q.reached);
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
