//! `ade-daemon` — 常駐的狀態中心。
//!
//! 負責：
//! * 持有 agent 子程序（透過 adapter）與其控制把手；
//! * 把 adapter 事件正規化後寫入 append-only event log（含秘密遮罩）並廣播給所有 client；
//! * 提供 Unix socket 與選配 TCP（M5 遠端）控制介面（[`ade_protocol`]）；
//! * 提供 agent 之間的訊息匯流排、收件匣/阻塞等待、防呆斷路器與任務看板；
//! * 提供 git worktree 管理、diff、行內審查註解（review 迴圈）、merge 與 PR（M3）；
//! * 提供權限規則引擎（`policy.toml`）與閒置休眠/喚醒（M4）；
//! * 提供內建的 stdio MCP server（[`mcp`]）。

mod bus;
pub use bus::BusGuard;
use bus::*;
mod client;
pub mod config;
mod git;
mod logwriter;
mod server;
use logwriter::*;
pub use logwriter::{rotated_log_path, LogConfig};
use server::*;
pub use server::{run, run_remote, run_with_options, RemoteOptions};
mod paths;
pub use client::{ensure_daemon, is_running, send_request, Client, ConnStream};
pub use git::compute_worktree_diff;
use git::*;
pub use paths::{config_dir, log_path, runtime_dir, socket_path, state_dir};
pub mod mcp;

use ade_adapter::{spawn as spawn_adapter, AgentControl, AgentEvent, SessionSpec};
use ade_core::{
    new_id, now_millis, redact_secrets, Artifact, BusMessage, Decision, Event, MsgId, MsgKind,
    PermissionPolicy, PolicyEngine, Record, ReviewComment, Seq, Session, SessionId, SessionStatus,
    State, Target, Task, TaskStatus, WorktreeInfo,
};
use ade_protocol::{decode_line, encode_line, Frame, Request, SpawnRequest};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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

/// 每個 client 佇列中可累積的未送出 frame 上限；超過即視為卡住並斷線。
const CLIENT_QUEUE_CAP: usize = 4096;

/// 取鎖；鎖中毒時取回內部資料繼續，避免一個 panic 拖垮整個 daemon。
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock()
        .unwrap_or_else(|e: PoisonError<MutexGuard<'_, T>>| {
            tracing::error!("鎖已中毒（某執行緒 panic），取回內部資料繼續");
            e.into_inner()
        })
}

/// 載入權限規則引擎：優先讀工作目錄 `.ade/policy.toml`，其次 `~/.config/ade/policy.toml`。
pub fn load_policy(cwd: &Path) -> PolicyEngine {
    let local = cwd.join(".ade").join("policy.toml");
    if let Ok(src) = fs::read_to_string(&local) {
        return PolicyEngine::parse_toml(&src);
    }
    let global = config_dir().join("policy.toml");
    if let Ok(src) = fs::read_to_string(&global) {
        return PolicyEngine::parse_toml(&src);
    }
    PolicyEngine::default()
}

/// repo 層級的 `.ade/policy.toml`；不存在回傳空規則。
fn load_local_policy(dir: &Path) -> PolicyEngine {
    match fs::read_to_string(dir.join(".ade").join("policy.toml")) {
        Ok(src) => PolicyEngine::parse_toml(&src),
        Err(_) => PolicyEngine {
            allow: vec![],
            deny: vec![],
        },
    }
}

/// 審批決策順序：deny → allow → session 政策（auto／bypass 自動允許）→ 交給人（`None`）。
fn decide_permission(
    policy: &PolicyEngine,
    session_policy: PermissionPolicy,
    title: &str,
    detail: &str,
) -> Option<Decision> {
    if let Some(d) = policy.evaluate(title, detail) {
        return Some(d);
    }
    match session_policy {
        PermissionPolicy::Bypass => Some(Decision::AllowAlways),
        PermissionPolicy::Auto => Some(Decision::Allow),
        PermissionPolicy::Ask => None,
    }
}

/// 從 `events.jsonl` 重播歷史事件，回傳重建後的 `(State, Vec<Record>)`。
/// 只讀這一份檔；輪替後的檔案第一行是 snapshot，狀態以它為起點。
pub fn replay_log(path: &Path) -> Result<(State, Vec<Record>)> {
    replay_files(&[path.to_path_buf()])
}

/// 輪替過的舊檔（由舊到新），不含目前這一份。
pub fn rotated_logs(path: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = (1..)
        .map(|n| rotated_log_path(path, n))
        .take_while(|p| p.is_file())
        .collect();
    out.reverse();
    out
}

/// 依序重播所有輪替檔與目前這一份（`ade replay --all`）。
pub fn replay_log_all(path: &Path) -> Result<(State, Vec<Record>)> {
    let mut files = rotated_logs(path);
    files.push(path.to_path_buf());
    replay_files(&files)
}

fn replay_files(paths: &[PathBuf]) -> Result<(State, Vec<Record>)> {
    let mut state = State::new();
    let mut records = Vec::new();
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let file = File::open(path).with_context(|| format!("開啟 {path:?} 失敗"))?;
        let mut bad = 0usize;
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                bad += 1;
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Record>(line.trim()) {
                Ok(rec) => {
                    state.apply(&rec);
                    records.push(rec);
                }
                Err(_) => bad += 1,
            }
        }
        if bad > 0 {
            tracing::warn!("replay_log：略過 {bad} 行無法解析的紀錄（{path:?}）");
        }
    }
    Ok((state, records))
}

// ---------------------------------------------------------------------------
// Daemon 內部狀態
// ---------------------------------------------------------------------------

struct SessionEntry {
    ctl: Arc<dyn AgentControl>,
}

impl ConnStream {
    /// 關閉連線兩個方向（用於踢掉卡住的 client）。
    fn shutdown(&self) {
        let _ = match self {
            ConnStream::Unix(u) => u.shutdown(std::net::Shutdown::Both),
            ConnStream::Tcp(t) => t.shutdown(std::net::Shutdown::Both),
        };
    }
}

/// 送往單一連線的有界佇列發送端：回覆照常送入，廣播則受 `CLIENT_QUEUE_CAP` 限制。
#[derive(Clone)]
struct ClientTx {
    tx: Sender<Frame>,
    pending: Arc<AtomicUsize>,
    /// 佇列滿時用來關閉底層連線。
    closer: Option<Arc<ConnStream>>,
}

struct ClientRx {
    rx: Receiver<Frame>,
    pending: Arc<AtomicUsize>,
}

fn client_channel(closer: Option<ConnStream>) -> (ClientTx, ClientRx) {
    let (tx, rx) = mpsc::channel();
    let pending = Arc::new(AtomicUsize::new(0));
    (
        ClientTx {
            tx,
            pending: pending.clone(),
            closer: closer.map(Arc::new),
        },
        ClientRx { rx, pending },
    )
}

impl ClientTx {
    fn send(&self, frame: Frame) -> Result<(), mpsc::SendError<Frame>> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.tx.send(frame).inspect_err(|_| {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        })
    }

    /// 廣播用：佇列已滿則回傳 false，並關閉連線讓 client 重連補齊。
    fn send_bounded(&self, frame: Frame) -> bool {
        if self.pending.load(Ordering::SeqCst) >= CLIENT_QUEUE_CAP {
            tracing::warn!("client 佇列已滿（{CLIENT_QUEUE_CAP}），中斷連線");
            if let Some(c) = &self.closer {
                c.shutdown();
            }
            return false;
        }
        self.send(frame).is_ok()
    }
}

impl ClientRx {
    fn note_taken(&self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn try_iter(&self) -> impl Iterator<Item = Frame> + '_ {
        self.rx.try_iter().inspect(|_| self.note_taken())
    }
}

impl Iterator for ClientRx {
    type Item = Frame;
    fn next(&mut self) -> Option<Frame> {
        let f = self.rx.recv().ok()?;
        self.note_taken();
        Some(f)
    }
}

struct Inner {
    state: State,
    seq: Seq,
    backlog: VecDeque<Record>,
    sessions: HashMap<SessionId, SessionEntry>,
    inbox_cursors: HashMap<SessionId, MsgId>,
    waiters: Vec<Waiter>,
    bus_guard: BusGuard,
    policy: PolicyEngine,
    default_cwd: PathBuf,
    clients: Vec<ClientTx>,
}

struct Shared {
    inner: Mutex<Inner>,
    log_tx: Mutex<Sender<LogMsg>>,
    /// log 目前是否能正常寫入（false 表示寫入失敗中）。
    log_ok: Arc<AtomicBool>,
    auth_token: Option<String>,
    /// 本次 daemon 啟動的世代 id。
    epoch: String,
}

impl Shared {
    /// 從既有 log 重播還原狀態，建立 `Shared`；log 內仍 live 的 session 視為遺失。
    fn restore(
        log_file: &Path,
        policy: PolicyEngine,
        default_cwd: PathBuf,
        auth_token: Option<String>,
    ) -> Result<Arc<Shared>> {
        Self::restore_with(
            log_file,
            policy,
            default_cwd,
            auth_token,
            LogConfig::from_env(),
        )
    }

    fn restore_with(
        log_file: &Path,
        policy: PolicyEngine,
        default_cwd: PathBuf,
        auth_token: Option<String>,
        log_cfg: LogConfig,
    ) -> Result<Arc<Shared>> {
        let (state, records) = replay_log(log_file)?;
        let (log_tx, log_ok) = spawn_log_writer(log_file.to_path_buf(), state.clone(), log_cfg);
        let seq = records.last().map_or(0, |r| r.seq);
        let backlog_all: Vec<Record> = records
            .into_iter()
            .filter(|r| !matches!(r.event, Event::Snapshot { .. }))
            .collect();
        let skip = backlog_all.len().saturating_sub(BACKLOG_CAP);
        let backlog: VecDeque<Record> = backlog_all.into_iter().skip(skip).collect();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                state,
                seq,
                backlog,
                sessions: HashMap::new(),
                inbox_cursors: HashMap::new(),
                waiters: Vec::new(),
                bus_guard: BusGuard::default(),
                policy,
                default_cwd,
                clients: Vec::new(),
            }),
            log_tx: Mutex::new(log_tx),
            log_ok,
            auth_token,
            epoch: new_id("ep"),
        });
        // 重啟後子程序已不受管：把仍 live 的 session 收斂成 Failed，狀態與 log 才一致。
        let orphans: Vec<SessionId> = {
            let inner = lock(&shared.inner);
            inner
                .state
                .sessions
                .values()
                .filter(|s| s.status.is_live())
                .map(|s| s.id.clone())
                .collect()
        };
        for sid in orphans {
            shared.emit(Event::SessionStatus {
                session: sid,
                status: SessionStatus::Failed,
            });
        }
        Ok(shared)
    }

    /// 廣播一個事件：更新狀態、寫 log（先遮罩憑證）、推給所有 client。
    fn emit(&self, event: Event) {
        let mut inner = lock(&self.inner);
        inner.seq += 1;
        let rec = Record {
            seq: inner.seq,
            ts: now_millis(),
            event,
        };
        inner.state.apply(&rec);

        // 序列化、遮罩與寫檔都在 log 寫入執行緒；這裡持鎖送出，順序與 seq 一致。
        let _ = lock(&self.log_tx).send(LogMsg::Rec(Box::new(rec.clone())));

        inner.backlog.push_back(rec.clone());
        while inner.backlog.len() > BACKLOG_CAP {
            inner.backlog.pop_front();
        }

        let frame = Frame::Record {
            record: Box::new(rec),
        };
        inner.clients.retain(|c| c.send_bounded(frame.clone()));
    }

    /// 把 adapter 事件翻譯成核心事件（並在審批事件時先經過 PolicyEngine 規則過濾）。
    fn ingest(&self, sid: &SessionId, ev: AgentEvent) {
        if let AgentEvent::Permission(ref p) = ev {
            let decision_and_ctl = {
                let inner = lock(&self.inner);
                let sess = inner.state.sessions.get(sid);
                let policy = match sess {
                    Some(s) => inner.policy.merged_with(&load_local_policy(Path::new(
                        s.repo_root.as_deref().unwrap_or(&s.cwd),
                    ))),
                    None => inner.policy.clone(),
                };
                let d = decide_permission(
                    &policy,
                    sess.map(|s| s.permission).unwrap_or_default(),
                    &p.title,
                    &p.detail,
                );
                let ctl = inner.sessions.get(sid).map(|e| e.ctl.clone());
                d.zip(ctl)
            };
            if let Some((decision, ctl)) = decision_and_ctl {
                if ctl.resolve_permission(&p.req, decision).is_ok() {
                    self.emit(Event::PermissionResolved {
                        session: sid.clone(),
                        req: p.req.clone(),
                        decision,
                    });
                    return;
                }
            }
        }

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
            AgentEvent::Model(m) => Event::SessionModel {
                session: sid.clone(),
                model: m,
            },
            AgentEvent::Exited(code) => Event::SessionExited {
                session: sid.clone(),
                code,
            },
        };
        let exited = matches!(event, Event::SessionExited { .. });
        self.emit(event);
        if exited {
            lock(&self.inner).sessions.remove(sid);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn log_healthy(&self) -> bool {
        self.log_ok.load(Ordering::SeqCst)
    }

    /// 等 log 寫入執行緒把目前佇列中的事件全部寫完。
    fn flush_log(&self) {
        let (tx, rx) = mpsc::channel();
        if lock(&self.log_tx).send(LogMsg::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
    }

    fn snapshot(&self) -> State {
        lock(&self.inner).state.clone()
    }

    fn default_cwd(&self) -> PathBuf {
        lock(&self.inner).default_cwd.clone()
    }

    /// 關閉：先結束所有 agent 子程序，再移除 socket，最後離開。
    fn shutdown(self: &Arc<Self>) {
        let ctls: Vec<Arc<dyn AgentControl>> = {
            let mut inner = lock(&self.inner);
            inner.sessions.drain().map(|(_, e)| e.ctl).collect()
        };
        for ctl in ctls {
            let _ = ctl.kill();
        }
        self.flush_log();
        let _ = fs::remove_file(socket_path());
        std::process::exit(0);
    }

    fn resolve_session(&self, id_or_label: &str) -> SessionId {
        lock(&self.inner)
            .state
            .resolve_session_id(id_or_label)
            .unwrap_or_else(|| id_or_label.to_string())
    }

    fn control(&self, id_or_label: &str) -> Option<(SessionId, Arc<dyn AgentControl>)> {
        let inner = lock(&self.inner);
        let sid = inner
            .state
            .resolve_session_id(id_or_label)
            .unwrap_or_else(|| id_or_label.to_string());
        inner.sessions.get(&sid).map(|e| (sid, e.ctl.clone()))
    }

    /// 若該 session 處於 `Sleeping`，先自動喚醒再送輸入。
    fn wake_if_sleeping(&self, sid: &SessionId, ctl: &Arc<dyn AgentControl>) {
        let is_sleeping = lock(&self.inner)
            .state
            .sessions
            .get(sid)
            .is_some_and(|s| s.status == SessionStatus::Sleeping);
        if is_sleeping && ctl.wake().is_ok() {
            self.emit(Event::SessionStatus {
                session: sid.clone(),
                status: SessionStatus::Idle,
            });
        }
    }

    // -----------------------------------------------------------------------
    // 請求處理
    // -----------------------------------------------------------------------

    fn handle(self: &Arc<Self>, reply: &ClientTx, req: Request) {
        match req {
            Request::Hello {
                client,
                since,
                token,
                epoch,
            } => {
                if let Some(expected) = &self.auth_token {
                    if !token_matches(token.as_deref(), expected) {
                        let _ = reply.send(Frame::error("遠端認證失敗：token 不符"));
                        return;
                    }
                }
                if client == "auth" {
                    return;
                }
                let _ = reply.send(Frame::Ready {
                    pid: std::process::id(),
                    version: VERSION.to_string(),
                    epoch: self.epoch.clone(),
                });
                // epoch 不符（或未帶）表示 client 的 seq 屬於別的 log，不能增量。
                let since = since.filter(|_| epoch.as_deref() == Some(self.epoch.as_str()));

                // M4 增量事件同步：若 `since` 在 backlog 範圍內，只補齊差異事件。
                let mut inner = lock(&self.inner);
                let can_incremental = since.is_some_and(|s| {
                    inner
                        .backlog
                        .front()
                        .is_some_and(|first| s + 1 >= first.seq)
                        || s == inner.seq
                });
                if let (Some(since_seq), true) = (since, can_incremental) {
                    for rec in inner.backlog.iter().filter(|r| r.seq > since_seq) {
                        let _ = reply.send(Frame::Record {
                            record: Box::new(rec.clone()),
                        });
                    }
                } else {
                    let snap = inner.state.clone();
                    let _ = reply.send(Frame::Snapshot {
                        state: Box::new(snap),
                    });
                }
                inner.clients.push(reply.clone());
            }

            Request::Snapshot => {
                let snap = self.snapshot();
                let _ = reply.send(Frame::Snapshot {
                    state: Box::new(snap),
                });
            }

            Request::Spawn(spawn) => self.handle_spawn(reply, spawn),

            Request::Prompt { session, text } => match self.control(&session) {
                Some((sid, ctl)) => {
                    self.wake_if_sleeping(&sid, &ctl);
                    let is_pty = lock(&self.inner)
                        .state
                        .sessions
                        .get(&sid)
                        .is_some_and(|s| s.transport == "pty");
                    if !is_pty {
                        self.emit(Event::UserPrompt {
                            session: sid.clone(),
                            text: text.clone(),
                        });
                    }
                    match ctl.send_input(&text) {
                        Ok(()) => {
                            let _ = reply.send(Frame::ok());
                        }
                        Err(e) => {
                            let _ = reply.send(Frame::error(format!("送 prompt 失敗：{e}")));
                        }
                    }
                }
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

            Request::Sleep { session } => match self.control(&session) {
                Some((sid, ctl)) => match ctl.sleep() {
                    Ok(()) => {
                        self.emit(Event::SessionStatus {
                            session: sid.clone(),
                            status: SessionStatus::Sleeping,
                        });
                        let _ = reply.send(Frame::ok_note(format!("已休眠 {sid}")));
                    }
                    Err(e) => {
                        let _ = reply.send(Frame::error(format!("休眠失敗：{e}")));
                    }
                },
                None => {
                    let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                }
            },

            Request::Wake { session } => match self.control(&session) {
                Some((sid, ctl)) => match ctl.wake() {
                    Ok(()) => {
                        self.emit(Event::SessionStatus {
                            session: sid.clone(),
                            status: SessionStatus::Idle,
                        });
                        let _ = reply.send(Frame::ok_note(format!("已喚醒 {sid}")));
                    }
                    Err(e) => {
                        let _ = reply.send(Frame::error(format!("喚醒失敗：{e}")));
                    }
                },
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
                let mut inner = lock(&self.inner);
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
                let body = format!(
                    "[ade 任務指派 id={id}]\n標題：{title}\n指派者：{}\n說明：{}\n請先用 ade_task_update 把狀態改為 doing，完成後改為 done 並用 ade_send 回報。",
                    created_by.as_deref().unwrap_or("human"),
                    if detail.is_empty() { "(無)" } else { &detail }
                );
                let task = Task {
                    id: id.clone(),
                    title,
                    detail,
                    status: TaskStatus::Todo,
                    assignee: assignee.clone(),
                    created_by: created_by.clone(),
                    depends_on,
                    updated_ms: now_millis(),
                };
                self.emit(Event::TaskChanged { task });

                // 指派任務時直接投遞一則 handoff 訊息，讓閒置中的負責人被喚醒。
                if let Some(target) = assignee.filter(|a| !a.is_empty()) {
                    if created_by.as_ref() != Some(&target) {
                        let _ = self.deliver_message(
                            created_by,
                            Target::Direct(target),
                            MsgKind::Handoff,
                            body,
                            None,
                        );
                    }
                }

                let _ = reply.send(Frame::ok_note(format!("已建立任務 {id}")));
            }

            Request::TaskUpdate {
                id,
                status,
                assignee,
                detail,
            } => {
                let resolved_assignee = assignee.map(|a| self.resolve_session(&a));
                let existing = lock(&self.inner).state.tasks.get(&id).cloned();
                match existing {
                    Some(mut task) => {
                        let previous_assignee = task.assignee.clone();
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

                        // 只有「換人做」時才發通知，避免每次狀態更新都吵醒負責人。
                        let newly_assigned = task
                            .assignee
                            .clone()
                            .filter(|a| Some(a) != previous_assignee.as_ref());
                        let creator = task.created_by.clone();
                        let task_id = task.id.clone();
                        let title = task.title.clone();
                        let task_detail = task.detail.clone();
                        let task_status = task.status;

                        self.emit(Event::TaskChanged { task });

                        if let Some(target) = newly_assigned {
                            if creator.as_ref() != Some(&target) {
                                let body = format!(
                                    "[ade 任務指派 id={task_id}]\n標題：{title}\n狀態：{}\n說明：{}\n請先用 ade_task_update 把狀態改為 doing，完成後改為 done 並用 ade_send 回報。",
                                    task_status.id(),
                                    if task_detail.is_empty() { "(無)" } else { &task_detail }
                                );
                                let _ = self.deliver_message(
                                    creator,
                                    Target::Direct(target),
                                    MsgKind::Handoff,
                                    body,
                                    None,
                                );
                            }
                        }

                        let _ = reply.send(Frame::ok_note(format!("已更新任務 {task_id}")));
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

            Request::WorktreeList => {
                let worktrees = self.collect_worktrees();
                let _ = reply.send(Frame::Worktrees { worktrees });
            }

            Request::WorktreeDiff { session } => {
                let sid = self.resolve_session(&session);
                let sess = lock(&self.inner).state.sessions.get(&sid).cloned();
                match sess {
                    Some(s) => match compute_worktree_diff(Path::new(&s.cwd)) {
                        Ok(diff) => {
                            let _ = reply.send(Frame::Diff {
                                session: s.id,
                                branch: s.branch,
                                cwd: s.cwd,
                                diff,
                            });
                        }
                        Err(e) => {
                            let _ = reply.send(Frame::error(format!("取得 diff 失敗：{e}")));
                        }
                    },
                    None => {
                        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                    }
                }
            }

            Request::WorktreeMerge { session } => {
                let sid = self.resolve_session(&session);
                let sess = lock(&self.inner).state.sessions.get(&sid).cloned();
                let default_cwd = self.default_cwd();
                match sess {
                    Some(s) => match merge_worktree(&session_repo(&default_cwd, &s), &s) {
                        Ok(msg) => {
                            let _ = reply.send(Frame::ok_note(msg));
                        }
                        Err(e) => {
                            let _ = reply.send(Frame::error(format!("合併失敗：{e}")));
                        }
                    },
                    None => {
                        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                    }
                }
            }

            Request::WorktreePr {
                session,
                title,
                body,
            } => {
                let sid = self.resolve_session(&session);
                let sess = lock(&self.inner).state.sessions.get(&sid).cloned();
                match sess {
                    Some(s) => match create_worktree_pr(&s, &title, &body) {
                        Ok(url) => {
                            let _ = reply.send(Frame::ok_note(url));
                        }
                        Err(e) => {
                            let _ = reply.send(Frame::error(format!("建立 PR 失敗：{e}")));
                        }
                    },
                    None => {
                        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                    }
                }
            }

            Request::WorktreeGc { session, force } => {
                let target_sid = session.map(|s| self.resolve_session(&s));
                let default_cwd = self.default_cwd();
                let candidates: Vec<Session> = {
                    let inner = lock(&self.inner);
                    inner
                        .state
                        .sessions
                        .values()
                        .filter(|s| {
                            s.branch.is_some()
                                && match &target_sid {
                                    Some(id) => &s.id == id,
                                    None => !s.status.is_live(),
                                }
                        })
                        .cloned()
                        .collect()
                };
                let mut removed = 0usize;
                let mut skipped: Vec<String> = Vec::new();
                for s in candidates {
                    let repo = session_repo(&default_cwd, &s);
                    if !force {
                        if let Some(reason) = gc_block_reason(&repo, &s) {
                            skipped.push(format!("{}（{reason}）", s.label));
                            continue;
                        }
                    }
                    match remove_worktree(&repo, &s, force) {
                        Ok(()) => removed += 1,
                        Err(e) => skipped.push(format!("{}（移除失敗：{e}）", s.label)),
                    }
                }
                let mut note = format!("已清理 {removed} 個 worktree");
                if !skipped.is_empty() {
                    note.push_str(&format!(
                        "；略過 {} 個：{}（可用 --force 強制移除）",
                        skipped.len(),
                        skipped.join("、")
                    ));
                }
                let _ = reply.send(Frame::ok_note(note));
            }

            Request::ReviewComment {
                session,
                location,
                comment,
            } => {
                let sid = self.resolve_session(&session);
                let review = ReviewComment {
                    id: new_id("rev"),
                    session: sid.clone(),
                    location: location.clone(),
                    comment: comment.clone(),
                    ts: now_millis(),
                };
                self.emit(Event::ReviewCommented { review });
                let message = BusMessage {
                    id: new_id("msg"),
                    from: "reviewer".into(),
                    to: Target::Direct(sid.clone()),
                    kind: MsgKind::Chat,
                    body: format!("[review @ {location}] {comment}"),
                    reply_to: None,
                    ts: now_millis(),
                };
                self.emit(Event::BusMessage { message });

                if let Some((target_id, ctl)) = self.control(&sid) {
                    self.wake_if_sleeping(&target_id, &ctl);
                    let prompt = format!(
                        "[ade 程式碼審查註解]\n位置：{location}\n意見：{comment}\n請針對上述位置與審查意見修正程式碼。"
                    );
                    let _ = ctl.send_input(&prompt);
                }
                let _ = reply.send(Frame::ok_note("已送出審查註解"));
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

    fn collect_worktrees(&self) -> Vec<WorktreeInfo> {
        let sessions: Vec<Session> = lock(&self.inner)
            .state
            .sessions_sorted()
            .into_iter()
            .filter(|s| s.branch.is_some())
            .cloned()
            .collect();
        let mut out = Vec::with_capacity(sessions.len());
        for s in sessions {
            let path = Path::new(&s.cwd);
            let (changed_files, summary) = if path.is_dir() {
                git_status_summary(path)
            } else {
                (0, "(已移除)".to_string())
            };
            out.push(WorktreeInfo {
                session: s.id,
                label: s.label,
                path: s.cwd,
                branch: s.branch.unwrap_or_default(),
                live: s.status.is_live(),
                changed_files,
                summary,
            });
        }
        out
    }
    fn handle_spawn(self: &Arc<Self>, reply: &ClientTx, spawn: SpawnRequest) {
        let id = new_id("ses");
        let mut cwd = spawn
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_cwd());
        let mut branch = None;
        let mut repo_root = None;

        if spawn.worktree {
            let root = git_toplevel(&cwd).unwrap_or_else(|| cwd.clone());
            match make_worktree(&root, &id) {
                Ok((dir, br)) => {
                    repo_root = Some(root.to_string_lossy().to_string());
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

        let global_dir = config_dir();
        let resolved = config::Config::load(&global_dir, &cwd).resolve(
            &spawn.kind,
            &role,
            spawn.model.as_deref(),
            &spawn.args,
            spawn.permission,
        );
        // 子 agent 的權限不得高於父 session 與設定的上限（防呆，不是安全邊界）。
        let mut permission = resolved.permission;
        let mut downgraded = None;
        if let Some(pid) = &parent {
            let parent_perm = lock(&self.inner)
                .state
                .sessions
                .get(pid)
                .map(|p| p.permission);
            let mut cap = parent_perm;
            if let Some(m) = resolved.max_agent_permission {
                cap = Some(cap.map_or(m, |c| c.min_policy(m)));
            }
            if let Some(cap) = cap {
                let clamped = permission.min_policy(cap);
                if clamped != permission {
                    downgraded = Some((permission, clamped));
                    permission = clamped;
                }
            }
        }

        let session = Session {
            id: id.clone(),
            kind: spawn.kind.clone(),
            label,
            cwd: cwd.to_string_lossy().to_string(),
            branch,
            role,
            status: SessionStatus::Starting,
            model: resolved.model.clone(),
            cost: Default::default(),
            parent,
            created_ms: now_millis(),
            transport: String::new(),
            status_note: None,
            repo_root,
            permission,
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

        env.extend(resolved.env.iter().cloned());
        let (tx, rx) = mpsc::channel();
        let spec = SessionSpec {
            id: id.clone(),
            kind: spawn.kind.clone(),
            cwd: cwd.clone(),
            transport: spawn.transport,
            args: resolved.args.clone(),
            model: resolved.model.clone(),
            env,
            permission,
            allowed_tools: resolved.allowed_tools.clone(),
            disallowed_tools: resolved.disallowed_tools.clone(),
            read_only: resolved.read_only,
            sandbox: resolved.sandbox.clone(),
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
                if transport != "pty" {
                    if let Some(prompt) = spawn.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
                        self.emit(Event::UserPrompt {
                            session: id.clone(),
                            text: prompt.to_string(),
                        });
                    }
                }
                let mut inner = lock(&self.inner);
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

                let mut note = format!("已建立 session {id}");
                if let Some((from, to)) = downgraded {
                    note.push_str(&format!(
                        "（權限由 {} 調降為 {}：不得高於父 session／設定上限）",
                        from.id(),
                        to.id()
                    ));
                }
                let _ = reply.send(Frame::ok_note(note));
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

#[cfg(test)]
mod tests;
