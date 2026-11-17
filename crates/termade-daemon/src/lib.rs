//! `termade-daemon` — 常駐的狀態中心。
//!
//! 負責：
//! * 持有 agent 子處理程序 (透過 adapter)與其控制把手；
//! * 把 adapter 事件正規化後寫入 append-only event log (含秘密遮罩)並廣播給所有 client；
//! * 提供 Unix socket 與選配 TCP (M5 遠端)控制介面 ([`termade_protocol`])；
//! * 提供 agent 之間的訊息匯流排、收件匣/阻塞等待、防呆斷路器與任務看板；
//! * 提供 git worktree 管理、diff、行內審查註解 (review 迴圈)、merge 與 PR (M3)；
//! * 提供權限規則引擎 (`policy.toml`)與閒置休眠/喚醒 (M4)；
//! * 提供內建的 stdio MCP server ([`mcp`])。

mod bus;
pub use bus::BusGuard;
use bus::*;
mod client;
pub mod collab;
pub mod config;
mod git;
mod handlers;
mod handoff;
mod logwriter;
mod quota_watch;
mod server;
use logwriter::*;
pub use logwriter::{rotated_log_path, LogConfig};
use server::*;
pub use server::{
    detect_tailscale_ip, get_client_token, get_or_create_default_token, is_tailscale_ip,
    read_token_file, resolve_listen_addr, resolve_web_addr, run, run_remote, run_with_options,
    RemoteOptions,
};
mod paths;
pub(crate) mod procs;
pub use client::{ensure_daemon, is_running, send_request, Client, ConnStream};
pub use git::compute_worktree_diff;
pub use paths::{config_dir, default_token_path, log_path, runtime_dir, socket_path, state_dir};
pub mod mcp;
pub mod web;

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
use termade_adapter::{AgentControl, AgentEvent};
pub(crate) use termade_core::{new_id, now_millis};
use termade_core::{
    redact_secrets, BusMessage, Decision, Event, MsgId, MsgKind, PermissionPolicy, PolicyEngine,
    Record, Seq, Session, SessionId, SessionStatus, State, Target,
};
use termade_protocol::{decode_line, encode_line, Frame, Request};
use tr::tr;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
/// 記憶體內保留的事件上限 (供新 client 補齊；超過則靠快照)。
const BACKLOG_CAP: usize = 20_000;
/// `BusWait` 最長等待上限 (5 分鐘，避免永久阻塞)。
const MAX_WAIT_MS: u64 = 300_000;
/// 同一對 session 連續傳送完全相同內容的上限 (超過即觸發迴圈斷路)。
const LOOP_REPEAT_CAP: usize = 3;
/// 同一對 session 每分鐘最多可傳送的訊息數。
const PAIR_RATE_CAP_PER_MIN: usize = 30;

/// 每個 client 佇列中可累積的未送出 frame 上限；超過即視為卡住並斷線。
const CLIENT_QUEUE_CAP: usize = 4096;
/// 單一 session 的待送 prompt 佇列上限。
pub(crate) const MAX_QUEUE_LEN: usize = 20;

pub(crate) struct QueuedPrompt {
    pub(crate) text: String,
    pub(crate) attachments: Vec<termade_core::Attachment>,
    pub(crate) raw_text: String,
}

/// 取鎖；鎖中毒時取回內部資料繼續，避免一個 panic 拖垮整個 daemon。
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock()
        .unwrap_or_else(|e: PoisonError<MutexGuard<'_, T>>| {
            tracing::error!("鎖已中毒 (某執行緒 panic)，取回內部資料繼續");
            e.into_inner()
        })
}

/// 載入權限規則引擎：優先讀工作目錄 `.termade/policy.toml`，其次 `~/.config/termade/policy.toml`。
pub fn load_policy(cwd: &Path) -> PolicyEngine {
    let local = cwd.join(".termade").join("policy.toml");
    if let Ok(src) = fs::read_to_string(&local) {
        return PolicyEngine::parse_toml(&src);
    }
    let global = config_dir().join("policy.toml");
    if let Ok(src) = fs::read_to_string(&global) {
        return PolicyEngine::parse_toml(&src);
    }
    PolicyEngine::default()
}

/// repo 層級的 `.termade/policy.toml`；不存在回傳空規則。
fn load_local_policy(dir: &Path) -> PolicyEngine {
    match fs::read_to_string(dir.join(".termade").join("policy.toml")) {
        Ok(src) => PolicyEngine::parse_toml(&src),
        Err(_) => PolicyEngine {
            allow: vec![],
            deny: vec![],
        },
    }
}

/// 核准決策順序：deny → allow → session 政策 (auto／bypass 自動允許)→ 交給人 (`None`)。
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

/// 輪替過的舊檔 (由舊到新)，不含目前這一份。
pub fn rotated_logs(path: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = (1..)
        .map(|n| rotated_log_path(path, n))
        .take_while(|p| p.is_file())
        .collect();
    out.reverse();
    out
}

/// 依序重播所有輪替檔與目前這一份 (`termade replay --all`)。
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
        let file = File::open(path)
            .with_context(|| tr!("開啟 {path} 失敗", path = format!("{path:?}")))?;
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
            tracing::warn!("replay_log：略過 {bad} 行無法解析的紀錄 ({path:?})");
        }
    }
    Ok((state, records))
}

// ---------------------------------------------------------------------------
// Daemon 內部狀態
// ---------------------------------------------------------------------------

pub(crate) struct SessionEntry {
    pub(crate) ctl: Arc<dyn AgentControl>,
    /// 這次處理程序執行的 id：接續會換新，用來擋掉舊處理程序遲到的事件。
    pub(crate) run: String,
}

impl ConnStream {
    /// 關閉連線兩個方向 (用於踢掉卡住的 client)。
    pub(crate) fn shutdown(&self) {
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

pub(crate) fn client_channel(closer: Option<ConnStream>) -> (ClientTx, ClientRx) {
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
            tracing::warn!("client 佇列已滿 ({CLIENT_QUEUE_CAP})，中斷連線");
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

pub(crate) struct Inner {
    pub(crate) state: State,
    pub(crate) seq: Seq,
    pub(crate) backlog: VecDeque<Record>,
    pub(crate) sessions: HashMap<SessionId, SessionEntry>,
    pub(crate) inbox_cursors: HashMap<SessionId, MsgId>,
    /// 送達時收件者正忙、因此沒有喚醒的訊息 (T25)；回到 `Idle` 時合併提醒一次。
    pub(crate) pending_nudge: HashMap<SessionId, Vec<MsgId>>,
    pub(crate) waiters: Vec<Waiter>,
    /// 等待人類回覆的 MCP 核准請求：req id → 決定通道。
    pub(crate) approval_waiters: HashMap<String, Sender<Decision>>,
    pub(crate) bus_guard: BusGuard,
    pub(crate) policy: PolicyEngine,
    pub(crate) default_cwd: PathBuf,
    pub(crate) clients: Vec<ClientTx>,
    /// 各 session 的待送 prompt 佇列 (含附件內容，只留記憶體)。
    pub(crate) queues: HashMap<SessionId, VecDeque<QueuedPrompt>>,
}

pub(crate) struct WebServerState {
    pub(crate) running: Arc<AtomicBool>,
    pub(crate) addr: String,
}

pub(crate) struct Shared {
    pub(crate) inner: Mutex<Inner>,
    pub(crate) log_tx: Mutex<Sender<LogMsg>>,
    /// log 目前是否能正常寫入 (false 表示寫入失敗中)。
    pub(crate) log_ok: Arc<AtomicBool>,
    pub(crate) auth_token: Mutex<Option<String>>,
    /// 本次 daemon 啟動的世代 id。
    pub(crate) epoch: String,
    /// 已經提醒過額度快滿的 agent (用量降回門檻以下才會重新提醒)。
    pub(crate) quota_warned: Mutex<std::collections::HashSet<String>>,
    /// 已經提醒過成本接近上限的 session (降回門檻以下或調高上限後才會重新提醒)。
    pub(crate) budget_warned: Mutex<std::collections::HashSet<SessionId>>,
    pub(crate) web_server: Mutex<Option<WebServerState>>,
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
        let (health_tx, health_rx) = mpsc::channel();
        let (log_tx, log_ok) = spawn_log_writer(
            log_file.to_path_buf(),
            state.clone(),
            log_cfg,
            Some(health_tx),
        );
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
                pending_nudge: HashMap::new(),
                waiters: Vec::new(),
                approval_waiters: HashMap::new(),
                bus_guard: BusGuard::default(),
                policy,
                default_cwd,
                clients: Vec::new(),
                queues: HashMap::new(),
            }),
            log_tx: Mutex::new(log_tx),
            log_ok,
            auth_token: Mutex::new(auth_token),
            epoch: new_id("ep"),
            quota_warned: Default::default(),
            budget_warned: Default::default(),
            web_server: Mutex::new(None),
        });

        let weak_shared = Arc::downgrade(&shared);
        std::thread::spawn(move || {
            while let Ok(ok) = health_rx.recv() {
                if let Some(sh) = weak_shared.upgrade() {
                    sh.set_log_health(ok);
                } else {
                    break;
                }
            }
        });

        let weak_for_procs = Arc::downgrade(&shared);
        std::thread::spawn(move || {
            while let Some(sh) = weak_for_procs.upgrade() {
                sh.scan_all_bg_procs();
                std::thread::sleep(Duration::from_secs(3));
            }
        });

        // 重啟後子處理程序已不受管：把仍 live 的 session 收斂成 Failed，狀態與 log 才一致。
        // 有原生對話 id 的可以接續，但不自動做 (會在使用者沒注意時動用額度)，只留一則說明。
        let orphans: Vec<(SessionId, bool)> = {
            let inner = lock(&shared.inner);
            inner
                .state
                .sessions
                .values()
                .filter(|s| s.status.is_live())
                .map(|s| (s.id.clone(), s.native_id.is_some()))
                .collect()
        };
        for (sid, resumable) in orphans {
            shared.emit(Event::SessionStatus {
                session: sid.clone(),
                status: SessionStatus::Failed,
            });
            if resumable {
                shared.emit(Event::SessionNote {
                    session: sid,
                    note: tr!("daemon 重啟，可用 :resume 接續"),
                });
            }
        }
        // daemon 重啟時記憶體內的 QueuedPrompt 已不在，清掉舊 log 殘留的排隊狀態。
        let stale_queues: Vec<SessionId> = {
            let inner = lock(&shared.inner);
            inner.state.queues.keys().cloned().collect()
        };
        for sid in stale_queues {
            shared.emit(Event::QueueChanged {
                session: sid,
                items: Vec::new(),
            });
        }
        shared.prune_ended_sessions(now_millis());
        Ok(shared)
    }

    /// 重啟還原後，清掉超過 1 小時且已結束、又沒有 worktree 分支的 session (log 仍完整保留)。
    /// 否則歷史上所有 session 都會一直堆在清單裡。
    pub(crate) fn prune_ended_sessions(&self, now_ms: u64) {
        const KEEP_ENDED_MS: u64 = 3600 * 1000;
        let mut inner = lock(&self.inner);
        let stale: Vec<SessionId> = inner
            .state
            .sessions
            .values()
            .filter(|s| {
                !s.status.is_live()
                    && s.branch.is_none()
                    && now_ms.saturating_sub(s.created_ms) > KEEP_ENDED_MS
            })
            .map(|s| s.id.clone())
            .collect();
        for id in stale {
            inner.state.sessions.remove(&id);
            inner.state.transcripts.remove(&id);
            inner.state.queues.remove(&id);
        }
    }

    /// 另一個仍在執行的 session 是否正在使用這個工作目錄 (交接後新舊 session 共用同一個 worktree)。
    /// 其他尚未移除、與 `s` 共用同一個 worktree 的 session (不論狀態)的 label。
    pub(crate) fn cwd_shared_with(&self, s: &Session) -> Vec<String> {
        lock(&self.inner)
            .state
            .sessions
            .values()
            .filter(|o| o.id != s.id && o.cwd == s.cwd)
            .map(|o| o.label.clone())
            .collect()
    }

    pub(crate) fn cwd_in_use_by_other(&self, s: &Session) -> Option<String> {
        lock(&self.inner)
            .state
            .sessions
            .values()
            .find(|o| o.id != s.id && o.cwd == s.cwd && o.status.is_live())
            .map(|o| o.label.clone())
    }

    pub(crate) fn set_log_health(&self, ok: bool) {
        let mut inner = lock(&self.inner);
        if inner.state.log_ok == ok {
            return;
        }
        inner.state.log_ok = ok;
        let snap = inner.state.clone();
        let frame = Frame::Snapshot {
            state: Box::new(snap),
        };
        inner.clients.retain(|c| c.send_bounded(frame.clone()));
    }

    /// 廣播一個事件：更新狀態、寫 log (先遮罩憑證)、推給所有 client。
    pub(crate) fn emit(&self, event: Event) {
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

    /// 把 adapter 事件翻譯成核心事件 (並在核准事件時先經過 PolicyEngine 規則過濾)。
    /// 依規則引擎與 session 政策自動決定核准；`None` 表示要問人。
    pub(crate) fn auto_decision(
        &self,
        sid: &SessionId,
        p: &termade_core::PermissionRequest,
    ) -> Option<Decision> {
        let inner = lock(&self.inner);
        let sess = inner.state.sessions.get(sid);
        let policy = match sess {
            Some(s) => inner.policy.merged_with(&load_local_policy(Path::new(
                s.repo_root.as_deref().unwrap_or(&s.cwd),
            ))),
            None => inner.policy.clone(),
        };
        decide_permission(
            &policy,
            sess.map(|s| s.permission).unwrap_or_default(),
            &p.title,
            &p.detail,
        )
    }

    pub(crate) fn ingest(self: &Arc<Self>, sid: &SessionId, ev: AgentEvent) {
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
            AgentEvent::Commands(commands) => Event::SessionCommands {
                session: sid.clone(),
                commands,
            },
            AgentEvent::NativeId(native_id) => Event::SessionNativeId {
                session: sid.clone(),
                native_id,
            },
            AgentEvent::Context { used, window } => Event::SessionContext {
                session: sid.clone(),
                used,
                window,
            },
            AgentEvent::Quota(quota) => Event::QuotaUpdated { quota },
            AgentEvent::Limited(detail) => Event::SessionLimited {
                session: sid.clone(),
                detail,
            },
            AgentEvent::Note(note) => Event::SessionNote {
                session: sid.clone(),
                note,
            },
            AgentEvent::Exited(code) => Event::SessionExited {
                session: sid.clone(),
                code,
            },
        };
        let exited = matches!(event, Event::SessionExited { .. });
        let is_cost = matches!(event, Event::Cost { .. });
        let became_idle = matches!(
            event,
            Event::SessionStatus {
                status: SessionStatus::Idle,
                ..
            }
        );
        let ended = matches!(
            event,
            Event::SessionExited { .. }
                | Event::SessionLimited { .. }
                | Event::SessionStatus {
                    status: SessionStatus::Exited | SessionStatus::Failed,
                    ..
                }
        );
        let watch = matches!(
            event,
            Event::QuotaUpdated { .. } | Event::SessionLimited { .. }
        )
        .then(|| event.clone());
        self.emit(event);
        if is_cost {
            self.check_budget(sid);
        }
        if let Some(ev) = watch {
            self.react_to_quota(&ev);
        }
        if exited {
            lock(&self.inner).sessions.remove(sid);
        }
        if ended {
            lock(&self.inner).pending_nudge.remove(sid);
            self.clear_queue_for_ended(sid, false);
        } else if became_idle {
            self.scan_bg_procs_for_session(sid);
            // 使用者排的 prompt 是明確指示，先送；佇列沒送出東西才補未讀訊息提醒 (T25)。
            if !self.flush_queue_if_idle(sid) {
                self.nudge_unread_if_idle(sid);
            }
        }
    }

    /// 當 session 回到 `Idle` 時，若佇列有待送 prompt，取第一則送出 (一次一則)。
    /// 回傳是否真的送出了 prompt。
    pub(crate) fn flush_queue_if_idle(self: &Arc<Self>, sid: &SessionId) -> bool {
        let popped = {
            let mut inner = lock(&self.inner);
            let is_idle = inner
                .state
                .sessions
                .get(sid)
                .is_some_and(|s| s.status == SessionStatus::Idle && !s.over_budget);
            // 超過成本上限 (T17)時佇列保留不送；調高上限後由 handle_budget 再觸發。
            if !is_idle {
                return false;
            }
            let item = inner.queues.get_mut(sid).and_then(|q| q.pop_front());
            if inner.queues.get(sid).is_some_and(|q| q.is_empty()) {
                inner.queues.remove(sid);
            }
            item
        };
        let Some(item) = popped else {
            return false;
        };
        match handlers::session::send_prompt(self, sid, &item.text, &item.attachments) {
            Ok(()) => {
                let items: Vec<String> = {
                    let inner = lock(&self.inner);
                    inner
                        .queues
                        .get(sid)
                        .map(|q| q.iter().map(|p| p.raw_text.clone()).collect())
                        .unwrap_or_default()
                };
                self.emit(Event::QueueChanged {
                    session: sid.clone(),
                    items,
                });
                true
            }
            Err(e) => {
                {
                    let mut inner = lock(&self.inner);
                    inner
                        .queues
                        .entry(sid.clone())
                        .or_default()
                        .push_front(item);
                }
                self.emit(Event::SessionNote {
                    session: sid.clone(),
                    note: tr!("排隊 prompt 送出失敗：{e}", e = e),
                });
                false
            }
        }
    }

    /// session 結束或移除時清空待送佇列，並廣播 `QueueChanged` (與結束提示)。
    pub(crate) fn clear_queue_for_ended(&self, sid: &SessionId, is_removed: bool) {
        let (dropped, had_state) = {
            let mut inner = lock(&self.inner);
            let dropped = inner.queues.remove(sid).map_or(0, |q| q.len());
            let had_state = inner.state.queues.contains_key(sid);
            (dropped, had_state)
        };
        if dropped == 0 && !had_state {
            return;
        }
        self.emit(Event::QueueChanged {
            session: sid.clone(),
            items: Vec::new(),
        });
        if !is_removed && dropped > 0 {
            self.emit(Event::SessionNote {
                session: sid.clone(),
                note: tr!(
                    "session 已結束，丟棄 {dropped} 則排隊 prompt",
                    dropped = dropped
                ),
            });
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

    pub(crate) fn snapshot(&self) -> State {
        let mut st = lock(&self.inner).state.clone();
        st.log_ok = self.log_ok.load(Ordering::SeqCst);
        st
    }

    pub(crate) fn default_cwd(&self) -> PathBuf {
        lock(&self.inner).default_cwd.clone()
    }

    pub(crate) fn auth_token(&self) -> Option<String> {
        lock(&self.auth_token).clone()
    }

    pub(crate) fn ensure_auth_token(&self) -> Result<String> {
        let mut tok_guard = lock(&self.auth_token);
        if let Some(t) = tok_guard.as_ref() {
            return Ok(t.clone());
        }
        let tok = crate::server::get_or_create_default_token()?;
        *tok_guard = Some(tok.clone());
        Ok(tok)
    }

    pub(crate) fn web_info(&self) -> String {
        let web_lock = lock(&self.web_server);
        if let Some(state) = web_lock.as_ref() {
            if state.running.load(Ordering::Relaxed) {
                let token = self.auth_token();
                let token_hint = token.as_deref().unwrap_or("");
                return format!("http://{}/?token={token_hint}", state.addr);
            }
        }
        "off".to_string()
    }

    pub(crate) fn stop_web(&self) -> bool {
        let mut web_lock = lock(&self.web_server);
        if let Some(state) = web_lock.take() {
            state.running.store(false, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub(crate) fn start_web(
        self: &Arc<Self>,
        addr_spec: &str,
        insecure_public: bool,
    ) -> Result<String> {
        let (resolved, _) = crate::server::resolve_web_addr(addr_spec)?;
        let token = self.ensure_auth_token().ok();
        crate::server::validate_listen(&resolved, token.is_some(), insecure_public)?;

        let mut web_lock = lock(&self.web_server);
        if let Some(state) = web_lock.as_ref() {
            if state.running.load(Ordering::Relaxed) && state.addr == resolved {
                let token_hint = token.as_deref().unwrap_or("");
                return Ok(format!("http://{resolved}/?token={token_hint}"));
            }
            state.running.store(false, Ordering::Relaxed);
        }

        let running = Arc::new(AtomicBool::new(true));
        crate::web::start_web_server(self.clone(), &resolved, running.clone())?;
        *web_lock = Some(WebServerState {
            running,
            addr: resolved.clone(),
        });
        tracing::info!("termade Web 版已啟動：http://{resolved}");
        let token_hint = token.as_deref().unwrap_or("");
        Ok(format!("http://{resolved}/?token={token_hint}"))
    }

    /// 關閉：先結束所有 agent 子處理程序，再移除 socket，最後離開。
    pub(crate) fn shutdown(self: &Arc<Self>) {
        self.stop_web();
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

    pub(crate) fn resolve_session(&self, id_or_label: &str) -> SessionId {
        lock(&self.inner)
            .state
            .resolve_session_id(id_or_label)
            .unwrap_or_else(|| id_or_label.to_string())
    }

    pub(crate) fn control(&self, id_or_label: &str) -> Option<(SessionId, Arc<dyn AgentControl>)> {
        let inner = lock(&self.inner);
        let sid = inner
            .state
            .resolve_session_id(id_or_label)
            .unwrap_or_else(|| id_or_label.to_string());
        inner.sessions.get(&sid).map(|e| (sid, e.ctl.clone()))
    }

    /// 若該 session 處於 `Sleeping`，先自動喚醒再送輸入。
    pub(crate) fn wake_if_sleeping(&self, sid: &SessionId, ctl: &Arc<dyn AgentControl>) {
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

    /// 探測單一 session 的後代背景處理程序，清單有變更時發出 Event::SessionBgProcs。
    pub(crate) fn scan_bg_procs_for_session(&self, sid: &SessionId) {
        let (root_pid, is_busy, old_procs) = {
            let inner = lock(&self.inner);
            let Some(s) = inner.state.sessions.get(sid) else {
                return;
            };
            if !s.status.is_live() {
                return;
            }
            let is_busy = s.status == SessionStatus::Busy;
            let old_procs = s.bg_procs.clone();
            let root_pid = inner.sessions.get(sid).and_then(|e| e.ctl.pid());
            (root_pid, is_busy, old_procs)
        };

        let Some(pid) = root_pid else {
            return;
        };

        let new_procs = procs::scan_session_procs(pid, is_busy);
        if new_procs != old_procs {
            self.emit(Event::SessionBgProcs {
                session: sid.clone(),
                procs: new_procs,
            });
        }
    }

    /// 探測所有活躍 session 的後代背景處理程序。
    pub(crate) fn scan_all_bg_procs(&self) {
        let live_sids: Vec<SessionId> = {
            let inner = lock(&self.inner);
            inner
                .state
                .sessions
                .iter()
                .filter(|(_, s)| s.status.is_live())
                .map(|(sid, _)| sid.clone())
                .collect()
        };
        for sid in live_sids {
            self.scan_bg_procs_for_session(&sid);
        }
    }

    // -----------------------------------------------------------------------
    // 請求處理
    // -----------------------------------------------------------------------

    fn handle(self: &Arc<Self>, reply: &ClientTx, req: Request) {
        handlers::dispatch(self, reply, req);
    }
}

#[cfg(test)]
mod tests;
