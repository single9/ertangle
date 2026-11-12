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

pub mod mcp;

use ade_adapter::{spawn as spawn_adapter, AgentControl, AgentEvent, SessionSpec};
use ade_core::{
    new_id, now_millis, redact_secrets, Artifact, BusMessage, Event, MsgId, MsgKind, PolicyEngine,
    Record, ReviewComment, Seq, Session, SessionId, SessionStatus, State, Target, Task, TaskStatus,
    WorktreeInfo,
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

/// 設定目錄（放 `policy.toml`）。
pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d).join("ade"),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".config/ade"),
            None => std::env::temp_dir().join("ade-config"),
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

/// 從 `events.jsonl` 重播歷史事件，回傳重建後的 `(State, Vec<Record>)`。
pub fn replay_log(path: &Path) -> Result<(State, Vec<Record>)> {
    let mut state = State::new();
    let mut records = Vec::new();
    if !path.is_file() {
        return Ok((state, records));
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
    Ok((state, records))
}

// ---------------------------------------------------------------------------
// 雙模連線傳輸（Unix Socket + M5 TCP Remote）
// ---------------------------------------------------------------------------

pub enum ConnStream {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl ConnStream {
    pub fn try_clone(&self) -> std::io::Result<ConnStream> {
        match self {
            ConnStream::Unix(u) => Ok(ConnStream::Unix(u.try_clone()?)),
            ConnStream::Tcp(t) => Ok(ConnStream::Tcp(t.try_clone()?)),
        }
    }
}

impl Read for ConnStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ConnStream::Unix(u) => u.read(buf),
            ConnStream::Tcp(t) => t.read(buf),
        }
    }
}

impl Write for ConnStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            ConnStream::Unix(u) => u.write(buf),
            ConnStream::Tcp(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            ConnStream::Unix(u) => u.flush(),
            ConnStream::Tcp(t) => t.flush(),
        }
    }
}

// ---------------------------------------------------------------------------
// Client（給 TUI / CLI / MCP 用）
// ---------------------------------------------------------------------------

pub struct Client {
    stream: ConnStream,
    reader: BufReader<ConnStream>,
}

impl Client {
    /// 連線至 daemon：若設定了 `ADE_REMOTE=<host:port>` 則走 TCP，否則走本地 Unix socket。
    pub fn connect() -> Result<Client> {
        let stream = if let Ok(remote) = std::env::var("ADE_REMOTE") {
            let tcp = TcpStream::connect(remote.trim())
                .with_context(|| format!("無法連線遠端 daemon（{remote}）"))?;
            ConnStream::Tcp(tcp)
        } else {
            let unix = UnixStream::connect(socket_path())
                .with_context(|| format!("無法連線 daemon（{:?}）", socket_path()))?;
            ConnStream::Unix(unix)
        };
        let reader = BufReader::new(stream.try_clone()?);
        let mut client = Client { stream, reader };
        // 若設定了 `ADE_TOKEN`，在第一個請求前自動送出帶 token 的 Hello 驗證。
        if let Ok(token) = std::env::var("ADE_TOKEN") {
            if !token.is_empty() {
                client.send(&Request::Hello {
                    epoch: None,
                    client: "auth".into(),
                    since: None,
                    token: Some(token),
                })?;
            }
        }
        Ok(client)
    }

    pub fn send(&mut self, req: &Request) -> Result<()> {
        let line = encode_line(req)?;
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }

    /// 取得一個可獨立寫入的 stream（給需要「同時讀與寫」的 client，例如 TUI）。
    pub fn try_clone_stream(&self) -> Result<ConnStream> {
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
pub fn send_request(stream: &mut ConnStream, req: &Request) -> Result<()> {
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
    if std::env::var_os("ADE_REMOTE").is_some() {
        bail!("無法連線至 ADE_REMOTE 指定的遠端 daemon");
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

/// 哪些訊息種類值得主動叫醒閒置的收件者。
///
/// `Status` 與 `Artifact` 屬於被動資訊（更新狀態、分享產出），留在收件匣即可；
/// 其餘（chat / handoff / question / answer）都代表對方可能需要動作。
fn kind_wakes_recipient(kind: MsgKind) -> bool {
    !matches!(kind, MsgKind::Status | MsgKind::Artifact)
}

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
    inbox_cursors: HashMap<SessionId, MsgId>,
    waiters: Vec<Waiter>,
    bus_guard: BusGuard,
    policy: PolicyEngine,
    default_cwd: PathBuf,
    clients: Vec<Sender<Frame>>,
}

struct Shared {
    inner: Mutex<Inner>,
    log: Mutex<File>,
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
        let (state, records) = replay_log(log_file)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)?;
        let seq = records.last().map_or(0, |r| r.seq);
        let skip = records.len().saturating_sub(BACKLOG_CAP);
        let backlog: VecDeque<Record> = records.into_iter().skip(skip).collect();
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
            log: Mutex::new(log),
            auth_token,
            epoch: new_id("ep"),
        });
        // 重啟後子程序已不受管：把仍 live 的 session 收斂成 Failed，狀態與 log 才一致。
        let orphans: Vec<SessionId> = {
            let inner = shared.inner.lock().unwrap();
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
        let mut inner = self.inner.lock().unwrap();
        inner.seq += 1;
        let rec = Record {
            seq: inner.seq,
            ts: now_millis(),
            event,
        };
        inner.state.apply(&rec);

        if let Ok(line) = serde_json::to_string(&rec) {
            let safe_line = redact_secrets(&line);
            let mut log = self.log.lock().unwrap();
            let _ = log.write_all(safe_line.as_bytes());
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

    /// 把 adapter 事件翻譯成核心事件（並在審批事件時先經過 PolicyEngine 規則過濾）。
    fn ingest(&self, sid: &SessionId, ev: AgentEvent) {
        if let AgentEvent::Permission(ref p) = ev {
            let decision_and_ctl = {
                let inner = self.inner.lock().unwrap();
                let d = inner.policy.evaluate(&p.title, &p.detail);
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
        inner.sessions.get(&sid).map(|e| (sid, e.ctl.clone()))
    }

    /// 若該 session 處於 `Sleeping`，先自動喚醒再送輸入。
    fn wake_if_sleeping(&self, sid: &SessionId, ctl: &Arc<dyn AgentControl>) {
        let is_sleeping = self
            .inner
            .lock()
            .unwrap()
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

    fn handle(self: &Arc<Self>, reply: &Sender<Frame>, req: Request) {
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
                let mut inner = self.inner.lock().unwrap();
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
                    let is_pty = self
                        .inner
                        .lock()
                        .unwrap()
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
                let existing = self.inner.lock().unwrap().state.tasks.get(&id).cloned();
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
                let sess = self.inner.lock().unwrap().state.sessions.get(&sid).cloned();
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
                let sess = self.inner.lock().unwrap().state.sessions.get(&sid).cloned();
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
                let sess = self.inner.lock().unwrap().state.sessions.get(&sid).cloned();
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
                    let inner = self.inner.lock().unwrap();
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
        let sessions: Vec<Session> = self
            .inner
            .lock()
            .unwrap()
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

    fn handle_bus_send(
        self: &Arc<Self>,
        reply: &Sender<Frame>,
        from: Option<SessionId>,
        to: Target,
        kind: MsgKind,
        body: String,
        reply_to: Option<MsgId>,
    ) {
        match self.deliver_message(from, to, kind, body, reply_to) {
            Ok(msg_id) => {
                let _ = reply.send(Frame::ok_note(format!("已送出訊息 {msg_id}")));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(e));
            }
        }
    }

    /// 把訊息送進匯流排：防呆檢查、喚醒阻塞中的 waiter、寫入各 session 的
    /// mailbox，並主動叫醒閒置（`Idle`）或休眠（`Sleeping`）的收件者。
    ///
    /// 喚醒規則：
    /// * 直接訊息（`Direct`）與廣播／頻道（`Broadcast` / `Channel`）都會喚醒收件者。
    /// * 只喚醒狀態為 `Idle` / `Sleeping` 的 session；`Busy` 代表正在工作，
    ///   訊息留在收件匣即可。
    /// * 已經透過 `ade_wait` / `ade_ask` 取得訊息的 session 不重複打擾。
    /// * `Status` / `Artifact` 屬於被動資訊，不主動喚醒任何人。
    fn deliver_message(
        self: &Arc<Self>,
        from: Option<SessionId>,
        to: Target,
        kind: MsgKind,
        body: String,
        reply_to: Option<MsgId>,
    ) -> std::result::Result<MsgId, String> {
        let now = now_millis();
        let (from_sid, resolved_to) = {
            let mut inner = self.inner.lock().unwrap();
            let from_sid = from
                .map(|f| inner.state.resolve_session_id(&f).unwrap_or(f))
                .unwrap_or_else(|| "human".into());
            let resolved_to = match to {
                Target::Direct(t) => {
                    Target::Direct(inner.state.resolve_session_id(&t).unwrap_or(t))
                }
                other => other,
            };
            inner
                .bus_guard
                .check(&from_sid, &resolved_to, &body, now)
                .map_err(|e| e.to_string())?;
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

        let mut woken_by_waiter: HashSet<SessionId> = HashSet::new();
        let mut idle_targets: Vec<(SessionId, Arc<dyn AgentControl>)> = Vec::new();
        let mut mailbox_dirs: Vec<(PathBuf, String)> = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap();
            let state_clone = inner.state.clone();
            let waiters = std::mem::take(&mut inner.waiters);
            let mut remaining = Vec::with_capacity(waiters.len());
            for w in waiters {
                if w.matches(&state_clone, &message) && w.tx.send(message.clone()).is_ok() {
                    inner
                        .inbox_cursors
                        .insert(w.session.clone(), message.id.clone());
                    woken_by_waiter.insert(w.session.clone());
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

            if kind_wakes_recipient(kind) {
                for s in state_clone.sessions.values() {
                    if s.id == from_sid
                        || woken_by_waiter.contains(&s.id)
                        || !matches!(s.status, SessionStatus::Idle | SessionStatus::Sleeping)
                        || !message.matches_recipient(&s.id, Some(&s.label))
                    {
                        continue;
                    }
                    if let Some(entry) = inner.sessions.get(&s.id) {
                        idle_targets.push((s.id.clone(), entry.ctl.clone()));
                    }
                }
            }
        }

        for (cwd, sid) in mailbox_dirs {
            let _ = append_mailbox(&cwd, &sid, &message);
        }

        for (target_sid, ctl) in idle_targets {
            self.wake_if_sleeping(&target_sid, &ctl);
            let reply_hint = match &message.to {
                Target::Direct(_) => format!(
                    "請處理並使用 ade_send（或 `ade send --to {}`）回覆。",
                    message.from
                ),
                _ => "請處理；可先呼叫 ade_inbox 查看完整收件匣，再用 ade_send 回覆。".to_string(),
            };
            let nudge = format!(
                "[ade 匯流排訊息 id={} 來自 {} ({})]: {}\n{reply_hint}",
                message.id,
                message.from,
                message.kind.id(),
                message.body,
            );
            let _ = ctl.send_input(&nudge);
        }

        Ok(msg_id)
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
        std::thread::spawn(
            move || match rx.recv_timeout(Duration::from_millis(clamped_ms)) {
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
            },
        );
    }

    fn handle_spawn(self: &Arc<Self>, reply: &Sender<Frame>, spawn: SpawnRequest) {
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
            repo_root,
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
                if transport != "pty" {
                    if let Some(prompt) = spawn.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
                        self.emit(Event::UserPrompt {
                            session: id.clone(),
                            text: prompt.to_string(),
                        });
                    }
                }
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

// ---------------------------------------------------------------------------
// Git / Worktree 輔助函式（M3）
// ---------------------------------------------------------------------------

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

/// 查詢某個工作目錄的變更檔案數與簡短摘要。
fn git_status_summary(cwd: &Path) -> (usize, String) {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .output()
    else {
        return (0, String::new());
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| !is_ade_status_line(l)).collect();
    let count = lines.len();
    let files: Vec<&str> = lines
        .iter()
        .take(3)
        .map(|l| l.get(3..).unwrap_or(l).trim())
        .collect();
    let summary = if count == 0 {
        "clean".to_string()
    } else {
        format!("{count} changed ({})", files.join(", "))
    };
    (count, summary)
}

/// 取得工作目錄的統一格式 diff（含已追蹤變更與新增未追蹤檔案的內容預覽）。
pub fn compute_worktree_diff(cwd: &Path) -> Result<String> {
    if !cwd.is_dir() {
        bail!("工作目錄不存在：{cwd:?}");
    }
    let mut out = String::new();

    // 1) 已追蹤檔案相對於 HEAD 的 diff。
    let diff = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff", "HEAD"])
        .output()
        .context("執行 git diff 失敗")?;
    let diff_str = String::from_utf8_lossy(&diff.stdout);
    if !diff_str.trim().is_empty() {
        out.push_str(&diff_str);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    // 2) 未追蹤的新檔案（排除 `.ade/`）：也產生 unified diff 樣式，方便在 TUI Diff 視圖審查與留註解。
    let untracked = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
        .context("執行 git ls-files 失敗")?;
    for rel in String::from_utf8_lossy(&untracked.stdout).lines() {
        let rel = rel.trim();
        if rel.is_empty() || rel.starts_with(".ade/") {
            continue;
        }
        let full = cwd.join(rel);
        if let Ok(content) = fs::read_to_string(&full) {
            out.push_str(&format!("diff --git a/{rel} b/{rel}\n"));
            out.push_str("new file mode 100644\n");
            out.push_str("--- /dev/null\n");
            out.push_str(&format!("+++ b/{rel}\n"));
            let lines: Vec<&str> = content.lines().collect();
            out.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
            for (idx, l) in lines.iter().take(200).enumerate() {
                out.push_str(&format!("+{l}\n"));
                if idx == 199 && lines.len() > 200 {
                    out.push_str("+... (truncated)\n");
                }
            }
        }
    }

    if out.trim().is_empty() {
        Ok("(目前工作區沒有尚未合併的變更)\n".to_string())
    } else {
        Ok(out)
    }
}

/// `git status --porcelain` 的一行是否屬於 `.ade/` 目錄下的路徑。
fn is_ade_status_line(line: &str) -> bool {
    let path = line.get(3..).unwrap_or("").trim();
    path == ".ade" || path.starts_with(".ade/")
}

fn git_output(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("執行 git {} 失敗", args.join(" ")))
}

/// 取得 `dir` 所在 repo 的根目錄。
fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git_output(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// session 所屬的主 repo；舊事件沒有記錄時退回預設目錄。
fn session_repo(default_cwd: &Path, s: &Session) -> PathBuf {
    s.repo_root
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cwd.to_path_buf())
}

/// 目錄內是否有未 commit 的變更（忽略 `.ade/`）。
fn has_uncommitted(dir: &Path) -> Result<bool> {
    let out = git_output(dir, &["status", "--porcelain"])?;
    if !out.status.success() {
        bail!(
            "git status 失敗：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| !l.trim().is_empty() && !is_ade_status_line(l)))
}

/// 若 worktree 內有尚未 commit 的變更，先自動 commit 到該 worktree 分支上。
fn auto_commit_worktree(wt_dir: &Path, label: &str) -> Result<()> {
    if !has_uncommitted(wt_dir)? {
        return Ok(());
    }
    let add = git_output(wt_dir, &["add", "-A", "--", ".", ":(exclude).ade"])?;
    if !add.status.success() {
        bail!(
            "git add 失敗：{}",
            String::from_utf8_lossy(&add.stderr).trim()
        );
    }
    let commit = git_output(
        wt_dir,
        &["commit", "-m", &format!("ade: changes from {label}")],
    )?;
    if !commit.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&commit.stdout),
            String::from_utf8_lossy(&commit.stderr)
        );
        if !text.contains("nothing to commit") && !text.contains("nothing added to commit") {
            bail!("commit worktree 失敗：{}", text.trim());
        }
    }
    Ok(())
}

/// 把 session 的 worktree 分支合併回主工作區。
fn merge_worktree(main_repo: &Path, session: &Session) -> Result<String> {
    let branch = session
        .branch
        .as_deref()
        .context("此 session 沒有獨立的 worktree 分支")?;
    // 前置檢查：任一不成立就不動 repo。
    let git_dir_out = git_output(main_repo, &["rev-parse", "--git-dir"])?;
    if !git_dir_out.status.success() {
        bail!("{main_repo:?} 不是 git repo");
    }
    let git_dir = {
        let p = String::from_utf8_lossy(&git_dir_out.stdout)
            .trim()
            .to_string();
        let p = PathBuf::from(p);
        if p.is_absolute() {
            p
        } else {
            main_repo.join(p)
        }
    };
    for marker in ["MERGE_HEAD", "rebase-merge", "rebase-apply"] {
        if git_dir.join(marker).exists() {
            bail!("主工作區正在合併／rebase 中（{marker}），請先處理完畢");
        }
    }
    if has_uncommitted(main_repo)? {
        bail!("主工作區有未 commit 的變更，請先 commit 或 stash 後再合併");
    }
    let target = git_output(main_repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let target = String::from_utf8_lossy(&target.stdout).trim().to_string();

    let wt_dir = Path::new(&session.cwd);
    if wt_dir.is_dir() {
        auto_commit_worktree(wt_dir, &session.label)?;
    }
    let out = git_output(main_repo, &["merge", "--no-edit", branch])?;
    if !out.status.success() {
        let conflicts = git_output(main_repo, &["diff", "--name-only", "--diff-filter=U"])
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let _ = git_output(main_repo, &["merge", "--abort"]);
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if conflicts.is_empty() {
            bail!("合併 {branch} 到 {target} 失敗，已還原：{detail}");
        }
        bail!(
            "合併 {branch} 到 {target} 發生衝突，已還原。衝突檔案：{}",
            conflicts.join(", ")
        );
    }
    Ok(format!("已將 {branch}（{}）合併進 {target}", session.label))
}

/// 透過 `gh pr create` 建立 Pull Request。
fn create_worktree_pr(session: &Session, title: &str, body: &str) -> Result<String> {
    let branch = session
        .branch
        .as_deref()
        .context("此 session 沒有獨立分支，無法建立 PR")?;
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.is_dir() {
        auto_commit_worktree(wt_dir, &session.label)?;
    }
    let out = Command::new("gh")
        .current_dir(wt_dir)
        .args([
            "pr", "create", "--title", title, "--body", body, "--head", branch,
        ])
        .output()
        .context("找不到或無法執行 `gh` CLI")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// gc 預設保護：未合併或有未 commit 變更時回傳原因。
fn gc_block_reason(main_repo: &Path, session: &Session) -> Option<String> {
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.is_dir() {
        match has_uncommitted(wt_dir) {
            Ok(true) => return Some("有未 commit 變更".into()),
            Ok(false) => {}
            Err(e) => return Some(format!("無法檢查狀態：{e}")),
        }
    }
    if let Some(branch) = &session.branch {
        match git_output(main_repo, &["merge-base", "--is-ancestor", branch, "HEAD"]) {
            Ok(o) if o.status.success() => {}
            Ok(_) => return Some("尚未合併".into()),
            Err(e) => return Some(format!("無法檢查合併狀態：{e}")),
        }
    }
    None
}

/// 移除某個 session 的 worktree 與對應分支；`force` 時連未合併／未 commit 的內容一併丟棄。
fn remove_worktree(main_repo: &Path, session: &Session, force: bool) -> Result<()> {
    let wt_dir = Path::new(&session.cwd);
    if wt_dir.exists() {
        let wt = wt_dir.to_string_lossy();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&wt);
        let out = git_output(main_repo, &args)?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
    }
    if let Some(branch) = &session.branch {
        let out = git_output(
            main_repo,
            &["branch", if force { "-D" } else { "-d" }, branch],
        )?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 伺服器
// ---------------------------------------------------------------------------

fn handle_conn(shared: Arc<Shared>, stream: ConnStream, require_auth: bool) -> Result<()> {
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

    // 遠端連線一律需要認證；沒有設定 token 時 fail closed（不再靜默放行）。
    let mut authenticated = !require_auth;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match decode_line::<Request>(line.trim()) {
            Ok(req) => {
                if !authenticated {
                    if let Request::Hello { ref token, .. } = req {
                        let ok = shared
                            .auth_token
                            .as_deref()
                            .is_some_and(|expected| token_matches(token.as_deref(), expected));
                        if ok {
                            authenticated = true;
                        } else {
                            // 失敗後延遲回覆再關閉連線，拖慢暴力嘗試。
                            std::thread::sleep(AUTH_FAIL_DELAY);
                            let _ = tx.send(Frame::error("遠端認證失敗：請設定正確的 ADE_TOKEN"));
                            break;
                        }
                    } else {
                        std::thread::sleep(AUTH_FAIL_DELAY);
                        let _ = tx.send(Frame::error("未認證的遠端連線：請先送 Hello 帶 token"));
                        break;
                    }
                }
                shared.handle(&tx, req);
            }
            Err(e) => {
                let _ = tx.send(Frame::error(format!("無法解析請求：{e}")));
            }
        }
    }
    Ok(())
}

/// 認證失敗後、回覆並關閉連線前的延遲。
const AUTH_FAIL_DELAY: Duration = Duration::from_millis(500);

/// 常數時間比對 token（逐位元組 XOR 累加；長度不同仍走完整比較再判定）。
fn token_matches(given: Option<&str>, expected: &str) -> bool {
    let Some(given) = given else { return false };
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

/// 遠端監聽的安全選項。
#[derive(Debug, Default, Clone)]
pub struct RemoteOptions {
    /// `--listen host:port`
    pub listen: Option<String>,
    /// 已棄用的 `--token`
    pub token: Option<String>,
    /// `--token-file <path>`（權限須為 0600）
    pub token_file: Option<PathBuf>,
    /// 允許綁定非 loopback 位址
    pub insecure_listen_public: bool,
}

/// 讀取 token 檔；要求不對 group/other 開放權限。
fn read_token_file(path: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path).with_context(|| format!("讀取 token 檔 {path:?} 失敗"))?;
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("token 檔 {path:?} 權限過寬，請執行 chmod 600");
    }
    let t = fs::read_to_string(path)?.trim().to_string();
    if t.is_empty() {
        bail!("token 檔 {path:?} 是空的");
    }
    Ok(t)
}

/// 決定 token 來源：`--token-file` > `--token`（棄用）> `ADE_TOKEN`。
fn resolve_token(opts: &RemoteOptions) -> Result<Option<String>> {
    if let Some(p) = &opts.token_file {
        return read_token_file(p).map(Some);
    }
    if let Some(t) = opts.token.clone().filter(|t| !t.is_empty()) {
        tracing::warn!("--token 已棄用（會出現在 ps 輸出）；請改用 ADE_TOKEN 或 --token-file");
        eprintln!("警告：--token 已棄用（會出現在 ps 輸出）；請改用 ADE_TOKEN 或 --token-file");
        return Ok(Some(t));
    }
    Ok(std::env::var("ADE_TOKEN").ok().filter(|t| !t.is_empty()))
}

/// 檢查 `--listen` 設定：必須有 token；非 loopback 需 `insecure_listen_public`。
fn validate_listen(addr: &str, has_token: bool, insecure_public: bool) -> Result<()> {
    use std::net::ToSocketAddrs;
    if !has_token {
        bail!("--listen 需要認證 token：請設定 ADE_TOKEN 或 --token-file <path>");
    }
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .with_context(|| format!("無法解析監聽位址 {addr}"))?
        .collect();
    let all_loopback = !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback());
    if !all_loopback {
        if !insecure_public {
            bail!(
                "--listen {addr} 不是 loopback 位址；TCP 為明文傳輸。\
                 建議綁 127.0.0.1 並用 SSH 轉發；若確定要公開，加上 --insecure-listen-public"
            );
        }
        eprintln!("警告：daemon 以明文 TCP 對非 loopback 位址 {addr} 開放，token 與內容可被側錄");
        tracing::warn!("明文 TCP 監聽於非 loopback 位址：{addr}");
    }
    Ok(())
}

/// 確保 socket 目錄存在、為自己所有且不對其他人開放（新建為 0700）。
fn ensure_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                bail!("{dir:?} 不是目錄");
            }
            // SAFETY: geteuid 無前置條件、無副作用。
            let me = unsafe { libc_geteuid() };
            if meta.uid() != me {
                bail!("{dir:?} 不屬於目前使用者（uid {}），拒絕啟動", meta.uid());
            }
            if meta.permissions().mode() & 0o077 != 0 {
                bail!("{dir:?} 權限過寬（需 0700），請 chmod 700 後重試");
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("建立 {dir:?} 失敗"))
        }
        Err(e) => Err(e.into()),
    }
}

extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

/// 以前景方式跑 daemon（預設只綁本地 Unix socket）。
pub fn run() -> Result<()> {
    run_with_options(None, None)
}

/// 相容入口：等同 [`run_remote`]，不允許公開監聽。
pub fn run_with_options(listen_tcp: Option<String>, token: Option<String>) -> Result<()> {
    run_remote(RemoteOptions {
        listen: listen_tcp,
        token,
        ..Default::default()
    })
}

/// 以前景方式跑 daemon，可選同時開放 TCP 遠端監聽（M5，含安全檢查）。
pub fn run_remote(opts: RemoteOptions) -> Result<()> {
    let auth_token = resolve_token(&opts)?;
    if let Some(addr) = &opts.listen {
        validate_listen(addr, auth_token.is_some(), opts.insecure_listen_public)?;
    }
    let listen_tcp = opts.listen.clone();
    let sock = socket_path();
    if let Some(dir) = sock.parent() {
        if dir == runtime_dir() {
            ensure_private_dir(dir)?;
        } else {
            fs::create_dir_all(dir).with_context(|| format!("建立 {dir:?} 失敗"))?;
        }
    }
    if sock.exists() {
        if is_running() {
            bail!("已有 daemon 在執行（{sock:?}）；請直接連線，或先 `ade shutdown`");
        }
        let _ = fs::remove_file(&sock);
    }
    let listener = UnixListener::bind(&sock).with_context(|| format!("綁定 {sock:?} 失敗"))?;

    fs::create_dir_all(state_dir())?;

    let default_cwd = std::env::current_dir()?;
    let policy = load_policy(&default_cwd);
    let shared = Shared::restore(&log_path(), policy, default_cwd, auth_token)?;

    // 若指定了 `--listen <host:port>`，在背景執行緒啟動 TCP 監聽（M5 遠端模式）。
    if let Some(addr) = listen_tcp {
        let tcp_listener =
            TcpListener::bind(&addr).with_context(|| format!("綁定 TCP {addr} 失敗"))?;
        tracing::info!("ade daemon 遠端監聽已啟動：tcp={addr}");
        let shared_tcp = shared.clone();
        std::thread::spawn(move || {
            for stream in tcp_listener.incoming() {
                match stream {
                    Ok(s) => {
                        let shared = shared_tcp.clone();
                        std::thread::spawn(move || {
                            let _ = handle_conn(shared, ConnStream::Tcp(s), true);
                        });
                    }
                    Err(e) => tracing::warn!("TCP accept 失敗：{e}"),
                }
            }
        });
    }

    tracing::info!("ade daemon 已啟動：socket={sock:?} log={:?}", log_path());

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let shared = shared.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_conn(shared, ConnStream::Unix(s), false) {
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
    use ade_core::{AgentKind, Cost, Decision, Role};

    // -----------------------------------------------------------------------
    // T05：遠端與 socket 安全
    // -----------------------------------------------------------------------

    #[test]
    fn token_compare_is_exact() {
        assert!(token_matches(Some("abc"), "abc"));
        assert!(!token_matches(Some("abd"), "abc"));
        assert!(!token_matches(Some("abcd"), "abc"));
        assert!(!token_matches(Some(""), "abc"));
        assert!(!token_matches(None, "abc"));
    }

    #[test]
    fn listen_requires_token_and_loopback() {
        assert!(validate_listen("127.0.0.1:0", false, false).is_err());
        assert!(validate_listen("127.0.0.1:0", true, false).is_ok());
        assert!(validate_listen("0.0.0.0:7420", true, false).is_err());
        assert!(validate_listen("0.0.0.0:7420", true, true).is_ok());
    }

    #[test]
    fn run_remote_refuses_listen_without_token() {
        std::env::remove_var("ADE_TOKEN");
        let err = run_remote(RemoteOptions {
            listen: Some("127.0.0.1:0".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("token"));
    }

    #[test]
    fn token_file_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_cwd();
        let f = dir.join("tok");
        fs::write(&f, "secret\n").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token_file(&f).is_err());
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_file(&f).unwrap(), "secret");
    }

    #[test]
    fn socket_dir_is_0700_and_loose_dir_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let base = temp_cwd();
        let d = base.join("ade");
        ensure_private_dir(&d).unwrap();
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // 已存在且 0700：通過
        ensure_private_dir(&d).unwrap();
        let loose = base.join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ensure_private_dir(&loose).is_err());
    }

    #[test]
    fn wrong_token_tcp_connection_is_closed() {
        use std::io::{BufRead, Write};
        let shared = Shared::restore(
            Path::new("/dev/null"),
            PolicyEngine::default(),
            std::env::temp_dir(),
            Some("right".into()),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let _ = handle_conn(shared, ConnStream::Tcp(s), true);
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let hello = Request::Hello {
            client: "t".into(),
            since: None,
            token: Some("wrong".into()),
            epoch: None,
        };
        c.write_all(encode_line(&hello).unwrap().as_bytes())
            .unwrap();
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("認證失敗"));
        // 之後的請求不被處理：連線已關閉，讀到 EOF
        let _ = c.write_all(encode_line(&Request::Snapshot).unwrap().as_bytes());
        line.clear();
        assert_eq!(r.read_line(&mut line).unwrap_or(0), 0);
    }

    #[test]
    fn paths_are_under_expected_dirs() {
        assert!(socket_path().ends_with("daemon.sock"));
        assert!(log_path().ends_with("events.jsonl"));
    }

    // -----------------------------------------------------------------------
    // 喚醒行為測試（廣播 / 直接訊息 / 任務指派）
    // -----------------------------------------------------------------------

    #[derive(Clone, Default)]
    struct MockCtl {
        inputs: Arc<Mutex<Vec<String>>>,
        wakes: Arc<Mutex<usize>>,
    }

    impl MockCtl {
        fn as_ctl(&self) -> Arc<dyn AgentControl> {
            Arc::new(self.clone())
        }
        fn inputs(&self) -> Vec<String> {
            self.inputs.lock().unwrap().clone()
        }
        fn wake_count(&self) -> usize {
            *self.wakes.lock().unwrap()
        }
    }

    impl AgentControl for MockCtl {
        fn send_input(&self, text: &str) -> Result<()> {
            self.inputs.lock().unwrap().push(text.to_string());
            Ok(())
        }
        fn cancel(&self) -> Result<()> {
            Ok(())
        }
        fn kill(&self) -> Result<()> {
            Ok(())
        }
        fn wake(&self) -> Result<()> {
            *self.wakes.lock().unwrap() += 1;
            Ok(())
        }
        fn resolve_permission(&self, _req: &str, _decision: Decision) -> Result<()> {
            Ok(())
        }
        fn transport(&self) -> &'static str {
            "mock"
        }
    }

    fn test_shared() -> Arc<Shared> {
        Shared::restore(
            Path::new("/dev/null"),
            PolicyEngine::default(),
            std::env::temp_dir(),
            None,
        )
        .expect("建立測試 Shared")
    }

    fn temp_cwd() -> PathBuf {
        let dir = std::env::temp_dir().join(new_id("ade-test"));
        fs::create_dir_all(&dir).expect("建立測試目錄");
        dir
    }

    fn add_session(
        shared: &Arc<Shared>,
        id: &str,
        label: &str,
        status: SessionStatus,
        ctl: Arc<dyn AgentControl>,
        cwd: &Path,
    ) {
        let mut inner = shared.inner.lock().unwrap();
        inner.state.sessions.insert(
            id.to_string(),
            Session {
                id: id.to_string(),
                kind: AgentKind::OpenCode,
                label: label.to_string(),
                cwd: cwd.to_string_lossy().to_string(),
                branch: None,
                role: Role::Worker,
                status,
                model: None,
                cost: Cost::default(),
                parent: None,
                created_ms: now_millis(),
                transport: "mock".to_string(),
                status_note: None,
                repo_root: None,
            },
        );
        inner.sessions.insert(id.to_string(), SessionEntry { ctl });
    }

    #[test]
    fn broadcast_wakes_idle_and_sleeping_recipients_only() {
        let shared = test_shared();
        let dir = temp_cwd();
        let sender = MockCtl::default();
        let idle = MockCtl::default();
        let sleeping = MockCtl::default();
        let busy = MockCtl::default();

        add_session(
            &shared,
            "ses_sender",
            "worker/sender",
            SessionStatus::Idle,
            sender.as_ctl(),
            &dir,
        );
        add_session(
            &shared,
            "ses_idle",
            "worker/idle",
            SessionStatus::Idle,
            idle.as_ctl(),
            &dir,
        );
        add_session(
            &shared,
            "ses_sleep",
            "worker/sleep",
            SessionStatus::Sleeping,
            sleeping.as_ctl(),
            &dir,
        );
        add_session(
            &shared,
            "ses_busy",
            "worker/busy",
            SessionStatus::Busy,
            busy.as_ctl(),
            &dir,
        );

        shared
            .deliver_message(
                Some("ses_sender".into()),
                Target::Broadcast,
                MsgKind::Chat,
                "請大家回報狀態".into(),
                None,
            )
            .expect("廣播應成功");

        assert!(sender.inputs().is_empty(), "發送者不該被自己喚醒");
        assert_eq!(idle.inputs().len(), 1, "idle 收件者應被喚醒");
        assert!(idle.inputs()[0].contains("請大家回報狀態"));
        assert_eq!(sleeping.inputs().len(), 1, "sleeping 收件者應被喚醒");
        assert_eq!(sleeping.wake_count(), 1, "sleeping 應先 SIGCONT 再送輸入");
        assert!(busy.inputs().is_empty(), "busy 收件者不該被打斷");
    }

    #[test]
    fn artifact_broadcast_does_not_wake_anyone() {
        let shared = test_shared();
        let dir = temp_cwd();
        let idle = MockCtl::default();
        add_session(
            &shared,
            "ses_idle",
            "worker/idle",
            SessionStatus::Idle,
            idle.as_ctl(),
            &dir,
        );

        shared
            .deliver_message(
                Some("human".into()),
                Target::Broadcast,
                MsgKind::Artifact,
                "分享了一個檔案".into(),
                None,
            )
            .expect("artifact 廣播應成功");

        assert!(idle.inputs().is_empty(), "artifact 只進收件匣，不主動喚醒");
    }

    #[test]
    fn direct_chat_wakes_idle_target() {
        let shared = test_shared();
        let dir = temp_cwd();
        let target = MockCtl::default();
        add_session(
            &shared,
            "ses_target",
            "worker/target",
            SessionStatus::Idle,
            target.as_ctl(),
            &dir,
        );

        shared
            .deliver_message(
                Some("human".into()),
                Target::Direct("worker/target".into()),
                MsgKind::Chat,
                "請看一下這則訊息".into(),
                None,
            )
            .expect("直接訊息應成功");

        assert_eq!(target.inputs().len(), 1, "直接訊息應喚醒 idle 對象");
    }

    #[test]
    fn task_assignment_wakes_idle_assignee() {
        let shared = test_shared();
        let dir = temp_cwd();
        let assignee = MockCtl::default();
        add_session(
            &shared,
            "ses_assignee",
            "worker/assignee",
            SessionStatus::Idle,
            assignee.as_ctl(),
            &dir,
        );

        let (tx, _rx) = mpsc::channel();
        shared.handle(
            &tx,
            Request::TaskCreate {
                title: "跑測試".into(),
                detail: "跑完整測試並回報".into(),
                assignee: Some("worker/assignee".into()),
                created_by: Some("human".into()),
                depends_on: vec![],
            },
        );

        let inputs = assignee.inputs();
        assert_eq!(inputs.len(), 1, "指派任務應喚醒負責人");
        assert!(inputs[0].contains("任務指派"));
        assert!(inputs[0].contains("跑測試"));

        let inbox = shared
            .inner
            .lock()
            .unwrap()
            .state
            .inbox_for("ses_assignee", None);
        assert_eq!(inbox.len(), 1, "任務通知應進負責人的收件匣");
        assert_eq!(inbox[0].kind, MsgKind::Handoff);
    }

    #[test]
    fn bus_guard_detects_loop_and_rate_limit() {
        let mut guard = BusGuard::default();
        let target = Target::Direct("ses_b".into());
        for i in 0..LOOP_REPEAT_CAP {
            assert!(
                guard
                    .check("ses_a", &target, "ping", 1000 + i as u64)
                    .is_ok(),
                "repeat {i} should pass"
            );
        }
        let err = guard
            .check("ses_a", &target, "ping", 2000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("迴圈偵測"));

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

    // ---- git 輔助函式（T02／T03）----

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn temp_repo(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ade-git-{tag}-{}", new_id("t")));
        fs::create_dir_all(&d).unwrap();
        git(&d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("a.txt"), "base\n").unwrap();
        git(&d, &["add", "."]);
        git(&d, &["commit", "-qm", "init"]);
        d
    }

    fn wt_session(repo: &Path, id: &str) -> Session {
        let (dir, branch) = make_worktree(repo, id).unwrap();
        Session {
            id: id.into(),
            kind: AgentKind::ClaudeCode,
            label: format!("w/{id}"),
            cwd: dir.to_string_lossy().to_string(),
            branch: Some(branch),
            role: Role::Worker,
            status: SessionStatus::Exited,
            model: None,
            cost: Default::default(),
            parent: None,
            created_ms: 0,
            transport: String::new(),
            status_note: None,
            repo_root: Some(repo.to_string_lossy().to_string()),
        }
    }

    #[test]
    fn merge_clean_succeeds_and_excludes_ade_dir() {
        let repo = temp_repo("m1");
        let s = wt_session(&repo, "s1");
        let wt = Path::new(&s.cwd);
        fs::write(wt.join("new.txt"), "x").unwrap();
        fs::create_dir_all(wt.join("sub.ade")).unwrap();
        fs::write(wt.join("sub.ade/f.txt"), "f").unwrap();
        fs::create_dir_all(wt.join(".ade/mailbox")).unwrap();
        fs::write(wt.join(".ade/mailbox/m"), "m").unwrap();
        merge_worktree(&repo, &s).unwrap();
        assert!(repo.join("new.txt").exists());
        assert!(repo.join("sub.ade/f.txt").exists());
        assert!(!repo.join(".ade/mailbox/m").exists());
        assert!(!has_uncommitted(&repo).unwrap());
    }

    #[test]
    fn merge_conflict_is_aborted() {
        let repo = temp_repo("m2");
        let s = wt_session(&repo, "s2");
        fs::write(Path::new(&s.cwd).join("a.txt"), "branch\n").unwrap();
        fs::write(repo.join("a.txt"), "main\n").unwrap();
        git(&repo, &["commit", "-qam", "main change"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let err = merge_worktree(&repo, &s).unwrap_err().to_string();
        assert!(err.contains("a.txt"), "{err}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert!(!repo.join(".git/MERGE_HEAD").exists());
        assert!(!has_uncommitted(&repo).unwrap());
    }

    #[test]
    fn merge_refuses_dirty_main() {
        let repo = temp_repo("m3");
        let s = wt_session(&repo, "s3");
        fs::write(Path::new(&s.cwd).join("n.txt"), "n").unwrap();
        fs::write(repo.join("a.txt"), "dirty\n").unwrap();
        assert!(merge_worktree(&repo, &s).is_err());
        assert_eq!(fs::read_to_string(repo.join("a.txt")).unwrap(), "dirty\n");
        assert!(!repo.join("n.txt").exists());
    }

    #[test]
    fn merge_targets_each_sessions_own_repo() {
        let r1 = temp_repo("m4a");
        let r2 = temp_repo("m4b");
        let s1 = wt_session(&r1, "s4a");
        let s2 = wt_session(&r2, "s4b");
        fs::write(Path::new(&s1.cwd).join("one.txt"), "1").unwrap();
        fs::write(Path::new(&s2.cwd).join("two.txt"), "2").unwrap();
        merge_worktree(&session_repo(&r1, &s1), &s1).unwrap();
        merge_worktree(&session_repo(&r1, &s2), &s2).unwrap();
        assert!(r1.join("one.txt").exists() && !r1.join("two.txt").exists());
        assert!(r2.join("two.txt").exists() && !r2.join("one.txt").exists());
    }

    #[test]
    fn gc_protects_unmerged_and_dirty() {
        let repo = temp_repo("g1");
        // 已合併且乾淨
        let merged = wt_session(&repo, "gm");
        assert!(gc_block_reason(&repo, &merged).is_none());
        remove_worktree(&repo, &merged, false).unwrap();
        assert!(!Path::new(&merged.cwd).exists());
        // 未合併 commit
        let unmerged = wt_session(&repo, "gu");
        let wt = Path::new(&unmerged.cwd);
        fs::write(wt.join("u.txt"), "u").unwrap();
        git(wt, &["add", "u.txt"]);
        git(wt, &["commit", "-qm", "u"]);
        assert!(gc_block_reason(&repo, &unmerged)
            .unwrap()
            .contains("未合併"));
        assert!(remove_worktree(&repo, &unmerged, false).is_err());
        // 未 commit
        let dirty = wt_session(&repo, "gd");
        fs::write(Path::new(&dirty.cwd).join("d.txt"), "d").unwrap();
        assert!(gc_block_reason(&repo, &dirty)
            .unwrap()
            .contains("未 commit"));
        // force 全部移除
        remove_worktree(&repo, &unmerged, true).unwrap();
        remove_worktree(&repo, &dirty, true).unwrap();
        assert!(!Path::new(&unmerged.cwd).exists() && !Path::new(&dirty.cwd).exists());
    }

    #[test]
    fn ade_status_line_matches_only_ade_dir() {
        assert!(is_ade_status_line("?? .ade/wt/x"));
        assert!(!is_ade_status_line("?? sub.ade/f"));
        assert!(!is_ade_status_line(" M src/.ade/f"));
    }

    // ---- 重啟還原（T01）----

    fn restart_session(id: &str, status: SessionStatus) -> Session {
        Session {
            id: id.into(),
            kind: AgentKind::ClaudeCode,
            label: id.into(),
            cwd: "/tmp".into(),
            branch: None,
            role: Role::Worker,
            status,
            model: None,
            cost: Default::default(),
            parent: None,
            created_ms: 0,
            transport: String::new(),
            status_note: None,
            repo_root: None,
        }
    }

    #[test]
    fn restart_restores_state_and_continues_seq() {
        let log = std::env::temp_dir().join(format!("{}.jsonl", new_id("ade-log")));
        let first =
            Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
        first.emit(Event::SessionStarted {
            session: restart_session("ses-live", SessionStatus::Idle),
        });
        first.emit(Event::SessionStarted {
            session: restart_session("ses-done", SessionStatus::Exited),
        });
        let n = first.inner.lock().unwrap().seq;
        drop(first);

        let second =
            Shared::restore(&log, PolicyEngine::default(), std::env::temp_dir(), None).unwrap();
        let inner = second.inner.lock().unwrap();
        assert!(inner.state.sessions.contains_key("ses-live"));
        assert!(inner.state.sessions.contains_key("ses-done"));
        // 原本 live 的 session 被收斂為已結束（多一筆事件）。
        assert!(!inner.state.sessions["ses-live"].status.is_live());
        assert_eq!(inner.seq, n + 1);
        drop(inner);
        second.emit(Event::SessionStatus {
            session: "ses-done".into(),
            status: SessionStatus::Exited,
        });
        drop(second);

        let (_, recs) = replay_log(&log).unwrap();
        assert!(recs.windows(2).all(|w| w[1].seq == w[0].seq + 1));
        assert_eq!(recs.len() as u64, n + 2);
    }

    #[test]
    fn hello_with_wrong_epoch_gets_snapshot() {
        let shared = test_shared();
        shared.emit(Event::SessionStarted {
            session: restart_session("ses-a", SessionStatus::Idle),
        });
        let (tx, rx) = mpsc::channel::<Frame>();
        shared.handle(
            &tx,
            Request::Hello {
                client: "t".into(),
                since: Some(1),
                token: None,
                epoch: Some("old".into()),
            },
        );
        let frames: Vec<Frame> = rx.try_iter().collect();
        assert!(frames.iter().any(|f| matches!(f, Frame::Snapshot { .. })));
        let (tx2, rx2) = mpsc::channel::<Frame>();
        shared.handle(
            &tx2,
            Request::Hello {
                client: "t".into(),
                since: Some(1),
                token: None,
                epoch: Some(shared.epoch.clone()),
            },
        );
        assert!(!rx2.try_iter().any(|f| matches!(f, Frame::Snapshot { .. })));
    }
}
