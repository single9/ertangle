//! 多主機連線管理與請求路由。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use ertangle_core::SessionId;
use ertangle_daemon::{Client, ConnStream};
use ertangle_protocol::{Frame, Request};
use tr::tr;

use crate::app::App;

/// 主機連線狀態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostStatus {
    Connected,
    Reconnecting(String),
    Disconnected,
}

/// 來自各主機的非同步事件。
pub(crate) enum HostEvent {
    Frame { host: String, frame: Frame },
    Disconnected { host: String },
    Reconnected { host: String, writer: ConnStream },
}

impl std::fmt::Debug for HostEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostEvent::Frame { host, frame } => f
                .debug_struct("HostEvent::Frame")
                .field("host", host)
                .field("frame", frame)
                .finish(),
            HostEvent::Disconnected { host } => f
                .debug_struct("HostEvent::Disconnected")
                .field("host", host)
                .finish(),
            HostEvent::Reconnected { host, .. } => f
                .debug_struct("HostEvent::Reconnected")
                .field("host", host)
                .finish(),
        }
    }
}

/// 主機連線規格。
#[derive(Clone)]
pub(crate) enum HostConnectSpec {
    Local,
    Tcp { addr: String, token: Option<String> },
}

impl HostConnectSpec {
    pub fn connect(&self) -> Result<Client> {
        match self {
            HostConnectSpec::Local => Client::connect(),
            HostConnectSpec::Tcp { addr, token } => Client::connect_tcp(addr, token.clone()),
        }
    }
}

/// 多主機請求路由器 (取代原先單一的 writer: ConnStream)。
pub(crate) struct HostRouter {
    writers: BTreeMap<String, ConnStream>,
    pub(crate) active_host: String,
    pub(crate) session_to_host: HashMap<SessionId, String>,
}

impl HostRouter {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            writers: BTreeMap::new(),
            active_host: "local".into(),
            session_to_host: HashMap::new(),
        }
    }

    pub fn single(stream: ConnStream) -> Self {
        let mut writers = BTreeMap::new();
        writers.insert("local".into(), stream);
        Self {
            writers,
            active_host: "local".into(),
            session_to_host: HashMap::new(),
        }
    }

    pub fn set_writer(&mut self, host: &str, stream: ConnStream) {
        self.writers.insert(host.to_string(), stream);
    }

    pub fn set_active(&mut self, host: &str) {
        self.active_host = host.to_string();
    }

    #[allow(dead_code)]
    pub fn map_session(&mut self, session: SessionId, host: String) {
        self.session_to_host.insert(session, host);
    }

    pub fn update_from_app(&mut self, app: &App) {
        self.active_host = app.active_host.clone();
        for (host_name, host_data) in &app.hosts {
            for sid in host_data
                .state
                .sessions
                .keys()
                .chain(host_data.state.archived_sessions.keys())
            {
                self.session_to_host.insert(sid.clone(), host_name.clone());
            }
        }
    }

    pub fn target_host(&self, req: &Request) -> &str {
        match req {
            Request::Prompt { session, .. }
            | Request::Cancel { session }
            | Request::Kill { session }
            | Request::KillBgProc { session, .. }
            | Request::Resume { session, .. }
            | Request::Clear { session, .. }
            | Request::Sleep { session }
            | Request::Wake { session }
            | Request::SessionRemove { session, .. }
            | Request::SessionConfig { session, .. }
            | Request::SessionBudget { session, .. }
            | Request::WorktreeDiff { session }
            | Request::WorktreeDiscard { session, .. }
            | Request::WorktreeMerge { session, .. }
            | Request::WorktreePr { session, .. }
            | Request::QueuePop { session }
            | Request::QueueClear { session }
            | Request::Handoff { session, .. }
            | Request::Permission { session, .. }
            | Request::BusInbox { session, .. }
            | Request::ReviewComment { session, .. } => self
                .session_to_host
                .get(session)
                .map(|s| s.as_str())
                .unwrap_or(&self.active_host),
            _ => &self.active_host,
        }
    }

    pub fn send(&mut self, req: &Request) -> Result<()> {
        let host = self.target_host(req).to_string();
        if let Some(stream) = self.writers.get_mut(&host) {
            ertangle_daemon::send_request(stream, req)
        } else if let Some(stream) = self.writers.get_mut(&self.active_host) {
            ertangle_daemon::send_request(stream, req)
        } else {
            bail!("{}", tr!("找不到主機 {host} 的連線", host = host));
        }
    }
}

/// 啟動單一主機的非同步連線／讀取／重連執行緒。
pub(crate) fn spawn_host_thread(
    host_name: String,
    initial_client: Option<Client>,
    spec: HostConnectSpec,
    tx: mpsc::Sender<HostEvent>,
    shared_seq: Arc<AtomicU64>,
    shared_epoch: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut current_client = initial_client;
        while running.load(Ordering::Relaxed) {
            let mut client = match current_client.take() {
                Some(c) => c,
                None => {
                    let mut backoff = Duration::from_millis(200);
                    let max_backoff = Duration::from_secs(5);
                    let mut connected_client = None;

                    while running.load(Ordering::Relaxed) {
                        let start = std::time::Instant::now();
                        while start.elapsed() < backoff {
                            if !running.load(Ordering::Relaxed) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        if !running.load(Ordering::Relaxed) {
                            break;
                        }

                        if let Ok(mut new_client) = spec.connect() {
                            let seq = shared_seq.load(Ordering::Relaxed);
                            let epoch = shared_epoch
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let token = match &spec {
                                HostConnectSpec::Local => ertangle_daemon::get_client_token(),
                                HostConnectSpec::Tcp { token, .. } => token.clone(),
                            };

                            let hello = Request::Hello {
                                client: "tui".into(),
                                since: if seq > 0 { Some(seq) } else { None },
                                token,
                                epoch,
                            };
                            if new_client.send(&hello).is_ok() {
                                if let Ok(new_writer) = new_client.try_clone_stream() {
                                    let _ = tx.send(HostEvent::Reconnected {
                                        host: host_name.clone(),
                                        writer: new_writer,
                                    });
                                    connected_client = Some(new_client);
                                    break;
                                }
                            }
                        }

                        backoff = match backoff.as_millis() {
                            200 => Duration::from_millis(500),
                            500 => Duration::from_millis(1000),
                            1000 => Duration::from_millis(2000),
                            _ => (backoff * 2).min(max_backoff),
                        };
                    }

                    match connected_client {
                        Some(c) => c,
                        None => break,
                    }
                }
            };

            loop {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                match client.recv() {
                    Ok(frame) => {
                        if tx
                            .send(HostEvent::Frame {
                                host: host_name.clone(),
                                frame,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(HostEvent::Disconnected {
                            host: host_name.clone(),
                        });
                        break;
                    }
                }
            }
        }
    })
}
