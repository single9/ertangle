//! `ade-daemon` — 常駐的狀態中心。
//!
//! 負責：
//! * 持有 agent 子程序（透過 adapter）與其控制把手；
//! * 把 adapter 事件正規化後寫入 append-only event log 並廣播給所有 client；
//! * 提供 Unix socket 控制介面（[`ade_protocol`]）；
//! * 提供 agent 之間的訊息匯流排與任務看板。
//!
//! TUI 只是其中一個 client；daemon 可在 TUI 關閉後繼續運作。

use ade_adapter::{spawn as spawn_adapter, AgentControl, AgentEvent, SessionSpec};
use ade_core::{
    new_id, now_millis, BusMessage, Event, Record, Seq, Session, SessionId, SessionStatus, State,
    Task, TaskStatus,
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
    runtime_dir().join("daemon.sock")
}

pub fn log_path() -> PathBuf {
    state_dir().join("events.jsonl")
}

// ---------------------------------------------------------------------------
// Client（給 TUI / CLI 用）
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
// Daemon 內部狀態
// ---------------------------------------------------------------------------

struct SessionEntry {
    ctl: Arc<dyn AgentControl>,
}

struct Inner {
    state: State,
    seq: Seq,
    backlog: VecDeque<Record>,
    sessions: HashMap<SessionId, SessionEntry>,
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

    fn control(&self, id: &str) -> Option<Arc<dyn AgentControl>> {
        self.inner
            .lock()
            .unwrap()
            .sessions
            .get(id)
            .map(|e| e.ctl.clone())
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
                // M0：一律回完整快照（`since` 增量同步留待後續）。
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
                Some(ctl) => match ctl.send_input(&text) {
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
                Some(ctl) => {
                    let _ = ctl.cancel();
                    let _ = reply.send(Frame::ok());
                }
                None => {
                    let _ = reply.send(Frame::error(format!("找不到 session {session}")));
                }
            },

            Request::Kill { session } => match self.control(&session) {
                Some(ctl) => {
                    let _ = ctl.kill();
                    self.emit(Event::SessionExited {
                        session,
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
                Some(ctl) => match ctl.resolve_permission(&req, decision) {
                    Ok(()) => {
                        self.emit(Event::PermissionResolved {
                            session,
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
            } => {
                let message = BusMessage {
                    id: new_id("msg"),
                    from: from.unwrap_or_else(|| "human".into()),
                    to,
                    kind,
                    body,
                    reply_to: None,
                    ts: now_millis(),
                };
                self.emit(Event::BusMessage { message });
                let _ = reply.send(Frame::ok());
            }

            Request::TaskCreate {
                title,
                detail,
                assignee,
            } => {
                let task = Task {
                    id: new_id("task"),
                    title,
                    detail,
                    status: TaskStatus::Todo,
                    assignee,
                    created_by: None,
                    updated_ms: now_millis(),
                };
                self.emit(Event::TaskChanged { task });
                let _ = reply.send(Frame::ok());
            }

            Request::TaskUpdate { id, status } => {
                let existing = self.inner.lock().unwrap().state.tasks.get(&id).cloned();
                match existing {
                    Some(mut task) => {
                        task.status = status;
                        task.updated_ms = now_millis();
                        self.emit(Event::TaskChanged { task });
                        let _ = reply.send(Frame::ok());
                    }
                    None => {
                        let _ = reply.send(Frame::error(format!("找不到任務 {id}")));
                    }
                }
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
            parent: spawn.parent.clone(),
            created_ms: now_millis(),
            transport: String::new(),
        };
        self.emit(Event::SessionStarted {
            session: session.clone(),
        });

        let (tx, rx) = mpsc::channel();
        let spec = SessionSpec {
            id: id.clone(),
            kind: spawn.kind.clone(),
            cwd: cwd.clone(),
            transport: spawn.transport,
            args: vec![],
            env: vec![],
            permission: spawn.permission,
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
    let dir = runtime_dir();
    fs::create_dir_all(&dir).with_context(|| format!("建立 {dir:?} 失敗"))?;
    let sock = socket_path();
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
}
