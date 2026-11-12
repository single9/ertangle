//! `ade-daemon` — 常駐的狀態中心。
//!
//! 負責：
//! * 持有 agent 子程序（透過 adapter）與其控制把手；
//! * 把 adapter 事件正規化後寫入 append-only event log 並廣播給所有 client；
//! * 提供 Unix socket 控制介面（[`ade_protocol`]）；
//! * 提供 agent 之間的訊息匯流排、收件匣/阻塞等待、防呆斷路器與任務看板；
//! * 提供內建的 stdio MCP server（[`mcp`]）。
//!
//! TUI 只是其中一個 client；daemon 可在 TUI 關閉後繼續運作。

pub mod mcp;

use ade_adapter::{spawn as spawn_adapter, AgentControl, AgentEvent, SessionSpec};
use ade_core::{
    new_id, now_millis, Artifact, BusMessage, Event, MsgId, MsgKind, Record, Seq, Session,
    SessionId, SessionStatus, State, Target, Task, TaskStatus,
};
use ade_protocol::{decode_line, encode_line, Frame, Request, SpawnRequest};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// 記憶體內保留的事件上限（供新 client 補齊；超過則靠快照）。
const BACKLOG_CAP: usize = 20_000;
/// `BusWait` 最長等待上限（5 分鐘，避免永久阻塞）。
const MAX_WAIT_MS: u64 = 300_000;
/// 同一對 session 連續傳送完全相同內容的上限（超過即觸發迴圈斷路）。
const LOOP_REPEAT_CAP: usize = 3;
/// 同一對 session 每分鐘最多可傳送的訊息數。
const PAIR_RATE_CAP_PER_MIN: usize = 30;

// ---------------------------------------------------------------------------
// 路徑
// ---------------------------------------------------------------------------

fn user_tag() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

/// 執行期目錄（放 socket）。
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("ade"),
        None => std::env::temp_dir().join(format!("ade-{}", user_tag())),
    }
}

/// 狀態目錄（放 event log）。
pub fn state_dir() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(d) => PathBuf::from(d).join("ade"),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".local/state/ade"),
            None => std::env::temp_dir().join("ade-state"),
        },
    }
}

pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("ADE_SOCKET") {
        return PathBuf::from(s);
    }
    runtime_dir().join("daemon.sock")
}

pub fn log_path() -> PathBuf {
    state_dir().join("events.jsonl")
}

// ---------------------------------------------------------------------------
// Client（給 TUI / CLI / MCP 用）
// ---------------------------------------------------------------------------

pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    pub fn connect() -> Result<Client> {
        let stream = UnixStream::connect(socket_path())
            .with_context(|| format!("無法連線 daemon（{:?}）", socket_path()))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Client { stream, reader })
    }

    pub fn send(&mut self, req: &Request) -> Result<()> {
        let line = encode_line(req)?;
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }

    /// 取得一個可獨立寫入的 stream（給需要「同時讀與寫」的 client，例如 TUI）。
    pub fn try_clone_stream(&self) -> Result<UnixStream> {
        Ok(self.stream.try_clone()?)
    }

    pub fn recv(&mut self) -> Result<Frame> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("daemon 已關閉連線");
        }
        Ok(decode_line(line.trim())?)
    }
}

/// daemon 是否在跑。
pub fn is_running() -> bool {
    Client::connect().is_ok()
}

/// 送一個請求到一個 stream（給持有寫入端的 client 用）。
pub fn send_request(stream: &mut UnixStream, req: &Request) -> Result<()> {
    let line = encode_line(req)?;
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// 確保 daemon 在跑；不在就背景啟動它並等待 socket 就緒。
pub fn ensure_daemon() -> Result<()> {
    if is_running() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("取得自身執行檔路徑失敗")?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("無法啟動 daemon")?;
    for _ in 0..120 {
        std::thread::sleep(Duration::from_millis(50));
        if is_running() {
            return Ok(());
        }
    }
    bail!("daemon 啟動逾時（{:?}）", socket_path())
}

// ---------------------------------------------------------------------------
// 防呆：速率限制與迴圈偵測
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PairStats {
    last_body: String,
    repeat_count: usize,
    timestamps_ms: VecDeque<u64>,
}

/// 針對每對 `(from, to)` 檢查是否發生無限迴圈或訊息風暴。
#[derive(Default)]
pub struct BusGuard {
    pairs: HashMap<(String, String), PairStats>,
}

impl BusGuard {
    pub fn check(&mut self, from: &str, to: &Target, body: &str, now_ms: u64) -> Result<()> {
        let to_key = match to {
            Target::Direct(s) => format!("@{s}"),
            Target::Channel(c) => format!("#{c}"),
            Target::Broadcast => "*".to_string(),
        };
        let entry = self
            .pairs
            .entry((from.to_string(), to_key.clone()))
            .or_default();

        // 1) 速率限制：滑動 60 秒視窗。
        while let Some(&front) = entry.timestamps_ms.front() {
            if now_ms.saturating_sub(front) > 60_000 {
                entry.timestamps_ms.pop_front();
            } else {
                break;
            }
        }
        if entry.timestamps_ms.len() >= PAIR_RATE_CAP_PER_MIN {
            bail!(
                "速率限制：{from} → {to_key} 在 1 分鐘內已傳送超過 {PAIR_RATE_CAP_PER_MIN} 則訊息"
            );
        }

        // 2) 迴圈偵測：連續相同內容達 LOOP_REPEAT_CAP 次即斷路。
        if !body.is_empty() && entry.last_body == body {
            if entry.repeat_count >= LOOP_REPEAT_CAP {
                bail!(
                    "迴圈偵測：{from} → {to_key} 連續傳送相同內容超過 {LOOP_REPEAT_CAP} 次，已斷路"
                );
            }
            entry.repeat_count += 1;
        } else {
            entry.last_body = body.to_string();
            entry.repeat_count = 1;
        }

        entry.timestamps_ms.push_back(now_ms);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Daemon 內部狀態
// ---------------------------------------------------------------------------

struct SessionEntry {
    ctl: Arc<dyn AgentControl>,
}

/// 阻塞等待訊息的註冊項（`BusWait` / `ade_wait` / `ade_ask`）。
struct Waiter {
    session: SessionId,
    from: Option<SessionId>,
    reply_to: Option<MsgId>,
    tx: Sender<BusMessage>,
}

impl Waiter {
    fn matches(&self, state: &State, msg: &BusMessage) -> bool {
        let label = state.sessions.get(&self.session).map(|s| s.label.as_str());
        if !msg.matches_recipient(&self.session, label) {
            return false;
        }
        if let Some(expected_from) = &self.from {
            let resolved_expected = state
                .resolve_session_id(expected_from)
                .unwrap_or_else(|| expected_from.clone());
            let resolved_actual = state
                .resolve_session_id(&msg.from)
                .unwrap_or_else(|| msg.from.clone());
            if resolved_expected != resolved_actual {
                return false;
            }
        }
        if let Some(expected_reply) = &self.reply_to {
            // 若對方明確帶了 `reply_to`，就比對；若對方直接用 `ade_send(to=asker)` 回覆而沒帶 `reply_to`，
            // 只要 `from` 已鎖定該 peer，也視為對此問題的回覆。
            if let Some(actual_reply) = &msg.reply_to {
                if actual_reply != expected_reply {
                    return false;
                }
            } else if self.from.is_none() {
                return false;
            }
        }
        true
    }
}

struct Inner {
    state: State,
    seq: Seq,
    backlog: VecDeque<Record>,
    sessions: HashMap<SessionId, SessionEntry>,
    /// 每個 session 已讀的最後一則訊息 id。
    inbox_cursors: HashMap<SessionId, MsgId>,
    /// 正在阻塞等待訊息的 client。
    waiters: Vec<Waiter>,
    bus_guard: BusGuard,
    default_cwd: PathBuf,
    clients: Vec<Sender<Frame>>,
}

struct Shared {
    inner: Mutex<Inner>,
    log: Mutex<File>,
}

impl Shared {
    /// 廣播一個事件：更新狀態、寫 log、推給所有 client。
    fn emit(&self, event: Event) {
        let mut inner = self.inner.lock().unwrap();
        inner.seq += 1;
        let rec = Record {
            seq: inner.seq,
            ts: now_millis(),
            event,
        };
        inner.state.apply(&rec);

        if let Ok(line) = serde_json::to_string(&rec) {
            let mut log = self.log.lock().unwrap();
            let _ = log.write_all(line.as_bytes());
            let _ = log.write_all(b"\n");
        }

        inner.backlog.push_back(rec.clone());
        while inner.backlog.len() > BACKLOG_CAP {
            inner.backlog.pop_front();
        }

        let frame = Frame::Record {
            record: Box::new(rec),
        };
        inner.clients.retain(|c| c.send(frame.clone()).is_ok());
    }

    /// 把 adapter 事件翻譯成核心事件。
    fn ingest(&self, sid: &SessionId, ev: AgentEvent) {
        let event = match ev {
            AgentEvent::Status(s) => Event::SessionStatus {
                session: sid.clone(),
                status: s,
            },
            AgentEvent::Output(t) => Event::Output {
                session: sid.clone(),
                text: t,
            },
            AgentEvent::ToolCall {
                name,
                input,
                ok,
                output,
            } => Event::ToolCall {
                session: sid.clone(),
                name,
                input,
                ok,
                output,
            },
            AgentEvent::Permission(p) => Event::PermissionRequested { request: p },
            AgentEvent::Cost(c) => Event::Cost {
                session: sid.clone(),
                cost: c,
            },
            AgentEvent::Model(_) => return,
            AgentEvent::Exited(code) => Event::SessionExited {
                session: sid.clone(),
                code,
            },
        };
        let exited = matches!(event, Event::SessionExited { .. });
        self.emit(event);
        if exited {
            self.inner.lock().unwrap().sessions.remove(sid);
        }
    }

    fn snapshot(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }

    fn default_cwd(&self) -> PathBuf {
        self.inner.lock().unwrap().default_cwd.clone()
    }

    /// 關閉：先結束所有 agent 子程序，再移除 socket，最後離開。
    ///
    /// 不這樣做的話，agent 程序會被 reparent 到 init 變成孤兒，daemon 也再也管不到它們。
    fn shutdown(self: &Arc<Self>) {
        let ctls: Vec<Arc<dyn AgentControl>> = {
            let mut inner = self.inner.lock().unwrap();
            inner.sessions.drain().map(|(_, e)| e.ctl).collect()
        };
        for ctl in ctls {
            let _ = ctl.kill();
        }
        let _ = fs::remove_file(socket_path());
        std::process::exit(0);
    }

    fn resolve_session(&self, id_or_label: &str) -> SessionId {
        self.inner
            .lock()
            .unwrap()
            .state
            .resolve_session_id(id_or_label)
            .unwrap_or_else(|| id_or_label.to_string())
    }

    fn control(&self, id_or_label: &str) -> Option<(SessionId, Arc<dyn AgentControl>)> {
        let inner = self.inner.lock().unwrap();
        let sid = inner
            .state
            .resolve_session_id(id_or_label)
            .unwrap_or_else(|| id_or_label.to_string());
        inner
            .sessions
            .get(&sid)
            .map(|e| (sid, e.ctl.clone()))
    }

    // -----------------------------------------------------------------------
    // 請求處理
    // -----------------------------------------------------------------------

    fn handle(self: &Arc<Self>, reply: &Sender<Frame>, req: Request) {
        match req {
            Request::Hello { since, .. } => {
                let _ = reply.send(Frame::Ready {
                    pid: std::process::id(),
                    version: VERSION.to_string(),
                });
                let _ = since;
                let snap = self.snapshot();
                let _ = reply.send(Frame::Snapshot {
                    state: Box::new(snap),
                });
                self.inner.lock().unwrap().clients.push(reply.clone());
            }

            Request::Snapshot => {
                let snap = self.snapshot();
                let _ = reply.send(Frame::Snapshot {
                    state: Box::new(snap),
                });
            }

            Request::Spawn(spawn) => self.handle_spawn(reply, spawn),

            Request::Prompt { session, text } => match self.control(&session) {
                Some((_, ctl)) => match ctl.send_input(&text) {
                    Ok(()) => {
                        let _ = reply.send(Frame::ok());
                    }
                    Err(e) => {
                        let _ = reply.send(Frame::error(format!("送 prompt 失敗：{e}")));
                    }
                },
                None => {
                    let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                }
            },

            Request::Cancel { session } => match self.control(&session) {
                Some((_, ctl)) => {
                    let _ = ctl.cancel();
                    let _ = reply.send(Frame::ok());
                }
                None => {
                    let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                }
            },

            Request::Kill { session } => match self.control(&session) {
                Some((sid, ctl)) => {
                    let _ = ctl.kill();
                    self.emit(Event::SessionExited {
                        session: sid,
                        code: None,
                    });
                    let _ = reply.send(Frame::ok());
                }
                None => {
                    let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                }
            },

            Request::Permission {
                session,
                req,
                decision,
            } => match self.control(&session) {
                Some((sid, ctl)) => match ctl.resolve_permission(&req, decision) {
                    Ok(()) => {
                        self.emit(Event::PermissionResolved {
                            session: sid,
                            req,
                            decision,
                        });
                        let _ = reply.send(Frame::ok());
                    }
                    Err(e) => {
                        let _ = reply.send(Frame::error(format!("審批回覆失敗：{e}")));
                    }
                },
                None => {
                    let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                }
            },

            Request::BusSend {
                from,
                to,
                kind,
                body,
                reply_to,
            } => self.handle_bus_send(reply, from, to, kind, body, reply_to),

            Request::BusInbox {
                session,
                unread_only,
            } => {
                let sid = self.resolve_session(&session);
                let mut inner = self.inner.lock().unwrap();
                let after = if unread_only {
                    inner.inbox_cursors.get(&sid).cloned()
                } else {
                    None
                };
                let messages = inner.state.inbox_for(&sid, after.as_deref());
                if unread_only {
                    if let Some(last) = messages.last() {
                        inner.inbox_cursors.insert(sid, last.id.clone());
                    }
                }
                let _ = reply.send(Frame::Messages {
                    messages,
                    timed_out: false,
                });
            }

            Request::BusWait {
                session,
                from,
                reply_to,
                timeout_ms,
            } => self.handle_bus_wait(reply, session, from, reply_to, timeout_ms),

            Request::TaskCreate {
                title,
                detail,
                assignee,
                created_by,
                depends_on,
            } => {
                let assignee = assignee.map(|a| self.resolve_session(&a));
                let created_by = created_by.map(|c| self.resolve_session(&c));
                let id = new_id("task");
                let task = Task {
                    id: id.clone(),
                    title,
                    detail,
                    status: TaskStatus::Todo,
                    assignee,
                    created_by,
                    depends_on,
                    updated_ms: now_millis(),
                };
                self.emit(Event::TaskChanged { task });
                let _ = reply.send(Frame::ok_note(format!("已建立任務 {id}")));
            }

            Request::TaskUpdate {
                id,
                status,
                assignee,
                detail,
            } => {
                let resolved_assignee = assignee.map(|a| self.resolve_session(&a));
                let existing = self.inner.lock().unwrap().state.tasks.get(&id).cloned();
                match existing {
                    Some(mut task) => {
                        if let Some(st) = status {
                            task.status = st;
                        }
                        if let Some(a) = resolved_assignee {
                            task.assignee = if a.is_empty() { None } else { Some(a) };
                        }
                        if let Some(d) = detail {
                            task.detail = d;
                        }
                        task.updated_ms = now_millis();
                        self.emit(Event::TaskChanged { task });
                        let _ = reply.send(Frame::ok_note(format!("已更新任務 {id}")));
                    }
                    None => {
                        let _ = reply.send(Frame::error(format!("找不到任務 {id}")));
                    }
                }
            }

            Request::SessionNote { session, note } => {
                let sid = self.resolve_session(&session);
                self.emit(Event::SessionNote { session: sid, note });
                let _ = reply.send(Frame::ok());
            }

            Request::ArtifactShare {
                session,
                kind,
                path,
                note,
            } => {
                let sid = self.resolve_session(&session);
                let id = new_id("art");
                let ts = now_millis();
                let artifact = Artifact {
                    id: id.clone(),
                    session: sid.clone(),
                    kind,
                    path: path.clone(),
                    note: note.clone(),
                    ts,
                };
                self.emit(Event::ArtifactShared { artifact });
                let summary = if note.is_empty() {
                    format!("[{}] {}", kind.id(), path)
                } else {
                    format!("[{}] {} — {}", kind.id(), path, note)
                };
                let message = BusMessage {
                    id: new_id("msg"),
                    from: sid,
                    to: Target::Broadcast,
                    kind: MsgKind::Artifact,
                    body: summary,
                    reply_to: None,
                    ts,
                };
                self.emit(Event::BusMessage { message });
                let _ = reply.send(Frame::ok_note(format!("已分享產出 {id}")));
            }

            Request::Shutdown => {
                let _ = reply.send(Frame::ok_note("daemon 關閉中"));
                let shared = self.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(150));
                    shared.shutdown();
                });
            }
        }
    }

    fn handle_bus_send(
        self: &Arc<Self>,
        reply: &Sender<Frame>,
        from: Option<SessionId>,
        to: Target,
        kind: MsgKind,
        body: String,
        reply_to: Option<MsgId>,
    ) {
        let now = now_millis();
        let (from_sid, resolved_to) = {
            let mut inner = self.inner.lock().unwrap();
            let from_sid = from
                .map(|f| {
                    inner
                        .state
                        .resolve_session_id(&f)
                        .unwrap_or(f)
                })
                .unwrap_or_else(|| "human".into());
            let resolved_to = match to {
                Target::Direct(t) => Target::Direct(
                    inner
                        .state
                        .resolve_session_id(&t)
                        .unwrap_or(t),
                ),
                other => other,
            };
            if let Err(e) = inner.bus_guard.check(&from_sid, &resolved_to, &body, now) {
                let _ = reply.send(Frame::error(e.to_string()));
                return;
            }
            (from_sid, resolved_to)
        };

        let msg_id = new_id("msg");
        let message = BusMessage {
            id: msg_id.clone(),
            from: from_sid.clone(),
            to: resolved_to.clone(),
            kind,
            body: body.clone(),
            reply_to,
            ts: now,
        };
        self.emit(Event::BusMessage {
            message: message.clone(),
        });

        // 喚醒正在 `BusWait` 的等待者，並同步寫入檔案信箱（機制 C 後備）。
        let mut woke_waiter = false;
        let mut idle_target_ctl: Option<Arc<dyn AgentControl>> = None;
        let mut mailbox_dirs: Vec<(PathBuf, String)> = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap();
            let state_clone = inner.state.clone();
            let waiters = std::mem::take(&mut inner.waiters);
            let mut remaining = Vec::with_capacity(waiters.len());
            for w in waiters {
                if w.matches(&state_clone, &message) && w.tx.send(message.clone()).is_ok() {
                    inner.inbox_cursors.insert(w.session.clone(), message.id.clone());
                    if matches!(&resolved_to, Target::Direct(t) if t == &w.session) {
                        woke_waiter = true;
                    }
                } else {
                    remaining.push(w);
                }
            }
            inner.waiters = remaining;

            for s in state_clone.sessions.values() {
                if message.matches_recipient(&s.id, Some(&s.label)) {
                    mailbox_dirs.push((PathBuf::from(&s.cwd), s.id.clone()));
                }
            }

            // 若是直接發給某個 session 的 Question / Handoff，且對方目前 Idle 又沒有在 BusWait，
            // 自動把訊息轉成一個 prompt 喚醒對方處理。
            if !woke_waiter && matches!(kind, MsgKind::Question | MsgKind::Handoff) {
                if let Target::Direct(target_sid) = &resolved_to {
                    let is_idle = inner
                        .state
                        .sessions
                        .get(target_sid)
                        .is_some_and(|s| s.status == SessionStatus::Idle);
                    if is_idle {
                        idle_target_ctl =
                            inner.sessions.get(target_sid).map(|e| e.ctl.clone());
                    }
                }
            }
        }

        for (cwd, sid) in mailbox_dirs {
            let _ = append_mailbox(&cwd, &sid, &message);
        }

        if let Some(ctl) = idle_target_ctl {
            let nudge = format!(
                "[ade 匯流排訊息 id={} 來自 {} ({})]: {}\n請處理並使用 ade_send（或 `ade send --to {}`）回覆。",
                message.id,
                message.from,
                message.kind.id(),
                message.body,
                message.from
            );
            let _ = ctl.send_input(&nudge);
        }

        let _ = reply.send(Frame::ok_note(format!("已送出訊息 {msg_id}")));
    }

    fn handle_bus_wait(
        self: &Arc<Self>,
        reply: &Sender<Frame>,
        session: SessionId,
        from: Option<SessionId>,
        reply_to: Option<MsgId>,
        timeout_ms: u64,
    ) {
        let sid = self.resolve_session(&session);
        let (tx, rx) = mpsc::channel();
        let waiter = Waiter {
            session: sid.clone(),
            from,
            reply_to,
            tx,
        };

        // 先檢查收件匣是否已經有符合條件的未讀訊息。
        {
            let mut inner = self.inner.lock().unwrap();
            let after = inner.inbox_cursors.get(&sid).cloned();
            let unread = inner.state.inbox_for(&sid, after.as_deref());
            if let Some(found) = unread.into_iter().find(|m| waiter.matches(&inner.state, m)) {
                inner.inbox_cursors.insert(sid, found.id.clone());
                let _ = reply.send(Frame::Messages {
                    messages: vec![found],
                    timed_out: false,
                });
                return;
            }
            inner.waiters.push(waiter);
        }

        let reply = reply.clone();
        let clamped_ms = timeout_ms.clamp(1, MAX_WAIT_MS);
        std::thread::spawn(move || {
            match rx.recv_timeout(Duration::from_millis(clamped_ms)) {
                Ok(msg) => {
                    let _ = reply.send(Frame::Messages {
                        messages: vec![msg],
                        timed_out: false,
                    });
                }
                Err(_) => {
                    let _ = reply.send(Frame::Messages {
                        messages: vec![],
                        timed_out: true,
                    });
                }
            }
        });
    }

    fn handle_spawn(self: &Arc<Self>, reply: &Sender<Frame>, spawn: SpawnRequest) {
        let id = new_id("ses");
        let mut cwd = spawn
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_cwd());
        let mut branch = None;

        if spawn.worktree {
            match make_worktree(&cwd, &id) {
                Ok((dir, br)) => {
                    cwd = dir;
                    branch = Some(br);
                }
                Err(e) => {
                    let _ = reply.send(Frame::error(format!("建立 worktree 失敗：{e}")));
                    return;
                }
            }
        }

        let parent = spawn.parent.as_ref().map(|p| self.resolve_session(p));
        let role = spawn.role.clone();
        let label = spawn
            .label
            .clone()
            .unwrap_or_else(|| format!("{}/{}", role.id(), &id[4..8]));

        let session = Session {
            id: id.clone(),
            kind: spawn.kind.clone(),
            label,
            cwd: cwd.to_string_lossy().to_string(),
            branch,
            role,
            status: SessionStatus::Starting,
            model: None,
            cost: Default::default(),
            parent,
            created_ms: now_millis(),
            transport: String::new(),
            status_note: None,
        };
        self.emit(Event::SessionStarted {
            session: session.clone(),
        });

        let mcp_exe = std::env::current_exe().ok();
        let mut env = vec![
            ("ADE_SESSION".to_string(), id.clone()),
            (
                "ADE_SOCKET".to_string(),
                socket_path().to_string_lossy().to_string(),
            ),
        ];
        if let Some(exe) = &mcp_exe {
            env.push(("ADE_BIN".to_string(), exe.to_string_lossy().to_string()));
            if let Some(bin_dir) = exe.parent() {
                let current_path = std::env::var("PATH").unwrap_or_default();
                env.push((
                    "PATH".to_string(),
                    format!("{}:{current_path}", bin_dir.to_string_lossy()),
                ));
            }
        }

        let (tx, rx) = mpsc::channel();
        let spec = SessionSpec {
            id: id.clone(),
            kind: spawn.kind.clone(),
            cwd: cwd.clone(),
            transport: spawn.transport,
            args: vec![],
            env,
            permission: spawn.permission,
            mcp_exe,
            initial_prompt: spawn.prompt.clone(),
            cols: 120,
            rows: 40,
        };

        match spawn_adapter(spec, tx) {
            Ok(ctl) => {
                let transport = ctl.transport().to_string();
                self.emit(Event::SessionStatus {
                    session: id.clone(),
                    status: SessionStatus::Idle,
                });
                let mut inner = self.inner.lock().unwrap();
                if let Some(s) = inner.state.sessions.get_mut(&id) {
                    s.transport = transport;
                }
                inner.sessions.insert(
                    id.clone(),
                    SessionEntry {
                        ctl: Arc::from(ctl),
                    },
                );
                drop(inner);

                let shared = self.clone();
                let sid = id.clone();
                std::thread::spawn(move || {
                    for ev in rx {
                        shared.ingest(&sid, ev);
                    }
                });

                let _ = reply.send(Frame::ok_note(format!("已建立 session {id}")));
            }
            Err(e) => {
                self.emit(Event::SessionExited {
                    session: id.clone(),
                    code: None,
                });
                let _ = reply.send(Frame::error(format!("啟動 agent 失敗：{e}")));
            }
        }
    }
}

/// 機制 C 後備：把收到的訊息附加到 `<cwd>/.ade/mailbox/<session>.jsonl`。
fn append_mailbox(cwd: &Path, session: &str, msg: &BusMessage) -> Result<()> {
    let dir = cwd.join(".ade").join("mailbox");
    fs::create_dir_all(&dir)?;
    let file_path = dir.join(format!("{session}.jsonl"));
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// 在 `repo` 建立一個 worktree，回傳 (路徑, 分支)。
fn make_worktree(repo: &Path, id: &str) -> Result<(PathBuf, String)> {
    let branch = format!("ade/{id}");
    let dir = repo.join(".ade").join("wt").join(id);
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "add", "-b", &branch])
        .arg(&dir)
        .output()
        .context("執行 git worktree 失敗")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok((dir, branch))
}

// ---------------------------------------------------------------------------
// 伺服器
// ---------------------------------------------------------------------------

fn handle_conn(shared: Arc<Shared>, stream: UnixStream) -> Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let (tx, rx) = mpsc::channel::<Frame>();

    std::thread::spawn(move || {
        for frame in rx {
            match encode_line(&frame) {
                Ok(line) => {
                    if writer.write_all(line.as_bytes()).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
                Err(_) => break,
            }
        }
    });

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match decode_line::<Request>(line.trim()) {
            Ok(req) => shared.handle(&tx, req),
            Err(e) => {
                let _ = tx.send(Frame::error(format!("無法解析請求：{e}")));
            }
        }
    }
    Ok(())
}

/// 以前景方式跑 daemon，直到被關閉。
pub fn run() -> Result<()> {
    let sock = socket_path();
    if let Some(dir) = sock.parent() {
        fs::create_dir_all(dir).with_context(|| format!("建立 {dir:?} 失敗"))?;
    }
    if sock.exists() {
        // 已經有 daemon 在跑就不要搶它的 socket；只有「殘留的死 socket」才清掉。
        if is_running() {
            bail!("已有 daemon 在執行（{sock:?}）；請直接連線，或先 `ade shutdown`");
        }
        let _ = fs::remove_file(&sock);
    }
    let listener = UnixListener::bind(&sock).with_context(|| format!("綁定 {sock:?} 失敗"))?;

    fs::create_dir_all(state_dir())?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())?;

    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            state: State::new(),
            seq: 0,
            backlog: VecDeque::new(),
            sessions: HashMap::new(),
            inbox_cursors: HashMap::new(),
            waiters: Vec::new(),
            bus_guard: BusGuard::default(),
            default_cwd: std::env::current_dir()?,
            clients: Vec::new(),
        }),
        log: Mutex::new(log),
    });

    tracing::info!("ade daemon 已啟動：socket={sock:?} log={:?}", log_path());

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let shared = shared.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_conn(shared, s) {
                        tracing::debug!("client 連線結束：{e}");
                    }
                });
            }
            Err(e) => tracing::warn!("accept 失敗：{e}"),
        }
    }
    let _ = fs::remove_file(&sock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_under_expected_dirs() {
        assert!(socket_path().ends_with("daemon.sock"));
        assert!(log_path().ends_with("events.jsonl"));
    }

    #[test]
    fn bus_guard_detects_loop_and_rate_limit() {
        let mut guard = BusGuard::default();
        let target = Target::Direct("ses_b".into());
        for i in 0..LOOP_REPEAT_CAP {
            assert!(
                guard.check("ses_a", &target, "ping", 1000 + i as u64).is_ok(),
                "repeat {i} should pass"
            );
        }
        let err = guard
            .check("ses_a", &target, "ping", 2000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("迴圈偵測"));

        // 不同內容應重置連續計數，但超過每分鐘上限仍會被擋下。
        let mut guard2 = BusGuard::default();
        for i in 0..PAIR_RATE_CAP_PER_MIN {
            assert!(guard2
                .check("ses_a", &target, &format!("msg-{i}"), 1000 + i as u64)
                .is_ok());
        }
        let err2 = guard2
            .check("ses_a", &target, "overflow", 2000)
            .unwrap_err()
            .to_string();
        assert!(err2.contains("速率限制"));
    }
}
