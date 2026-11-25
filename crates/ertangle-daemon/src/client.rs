//! 連線傳輸 (Unix socket／TCP)與 client 端。

use super::*;
use tr::tr;

// ---------------------------------------------------------------------------
// 雙模連線傳輸 (Unix Socket + M5 TCP Remote)
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
// Client (給 TUI / CLI / MCP 用)
// ---------------------------------------------------------------------------

pub struct Client {
    pub(crate) stream: ConnStream,
    reader: BufReader<ConnStream>,
}

impl Client {
    /// 連線至 daemon：若設定了 `ERTANGLE_REMOTE=<host:port>` 則走 TCP，否則走本地 Unix socket。
    pub fn connect() -> Result<Client> {
        let stream = if let Some(remote) = ertangle_core::env_var("ERTANGLE_REMOTE") {
            let tcp = TcpStream::connect(remote.trim())
                .with_context(|| tr!("無法連線遠端 daemon ({remote})", remote = remote))?;
            ConnStream::Tcp(tcp)
        } else {
            let unix = UnixStream::connect(socket_path()).with_context(|| {
                tr!(
                    "無法連線 daemon ({path})",
                    path = format!("{:?}", socket_path())
                )
            })?;
            ConnStream::Unix(unix)
        };
        let reader = BufReader::new(stream.try_clone()?);
        let mut client = Client { stream, reader };
        // 若可取得遠端 token，在第一個請求前自動送出帶 token 的 Hello 驗證。
        if let Some(token) = get_client_token() {
            client.send(&Request::Hello {
                epoch: None,
                client: "auth".into(),
                since: None,
                token: Some(token),
            })?;
        }
        Ok(client)
    }

    /// 連線至指定 TCP 位址的遠端 daemon。
    pub fn connect_tcp(addr: &str, token: Option<String>) -> Result<Client> {
        let tcp = TcpStream::connect(addr.trim())
            .with_context(|| tr!("無法連線遠端 daemon ({addr})", addr = addr))?;
        let stream = ConnStream::Tcp(tcp);
        let reader = BufReader::new(stream.try_clone()?);
        let mut client = Client { stream, reader };
        if let Some(token) = token {
            client.send(&Request::Hello {
                epoch: None,
                client: "auth".into(),
                since: None,
                token: Some(token),
            })?;
        }
        Ok(client)
    }

    pub fn send(&mut self, req: &Request) -> Result<()> {
        let line = encode_line(req)?;
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }

    /// 取得一個可獨立寫入的 stream (給需要「同時讀與寫」的 client，例如 TUI)。
    pub fn try_clone_stream(&self) -> Result<ConnStream> {
        Ok(self.stream.try_clone()?)
    }

    pub fn recv(&mut self) -> Result<Frame> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("{}", tr!("daemon 已關閉連線"));
        }
        Ok(decode_line(line.trim())?)
    }
}

/// daemon 是否在跑。
pub fn is_running() -> bool {
    Client::connect().is_ok()
}

/// 送一個請求到一個 stream (給持有寫入端的 client 用)。
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
    if ertangle_core::env_var_os("ERTANGLE_REMOTE").is_some() {
        bail!("{}", tr!("無法連線至 ERTANGLE_REMOTE 指定的遠端 daemon"));
    }
    let sock = socket_path();
    if let Some(dir) = sock.parent() {
        if dir == runtime_dir() {
            crate::server::ensure_private_dir(dir)?;
        }
    }
    let exe = crate::current_exe_path().with_context(|| tr!("取得自身執行檔路徑失敗"))?;
    let log_dir = state_dir();
    // 建立日誌目錄，若已存在或建立失敗在開啟日誌檔時會一併處理
    let _ = fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("daemon.log");
    let err_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path);
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(Stdio::null());
    if let Ok(f) = err_file {
        cmd.stderr(f);
    } else {
        cmd.stderr(Stdio::null());
    }
    let mut child = cmd.spawn().with_context(|| tr!("無法啟動 daemon"))?;
    for _ in 0..120 {
        std::thread::sleep(Duration::from_millis(50));
        if is_running() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            let detail = fs::read_to_string(&log_path)
                .ok()
                .and_then(|s| {
                    s.lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            if detail.is_empty() {
                bail!("{}", tr!("daemon 異常退出 ({status})", status = status));
            } else {
                bail!("{}", tr!("daemon 啟動失敗：{detail}", detail = detail));
            }
        }
    }
    bail!(
        "{}",
        tr!(
            "daemon 啟動逾時 ({path})",
            path = format!("{:?}", socket_path())
        )
    )
}
