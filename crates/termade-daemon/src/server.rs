//! 伺服器：連線處理、遠端模式驗證與 daemon 主迴圈。

use super::*;

// ---------------------------------------------------------------------------
// 伺服器
// ---------------------------------------------------------------------------

pub(crate) fn handle_conn(
    shared: Arc<Shared>,
    stream: ConnStream,
    require_auth: bool,
) -> Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    let stream_closer = stream.try_clone().ok();
    let mut writer = stream;
    let (tx, rx) = client_channel(stream_closer);

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
                            let _ =
                                tx.send(Frame::error("遠端認證失敗：請設定正確的 TERMADE_TOKEN"));
                            break;
                        }
                    } else {
                        std::thread::sleep(AUTH_FAIL_DELAY);
                        let _ = tx.send(Frame::error("未認證的遠端連線：請先送 Hello 帶 token"));
                        break;
                    }
                }
                // 單一請求 panic 只斷這條連線，不影響 daemon 與其他 session。
                let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    shared.handle(&tx, req)
                }));
                if handled.is_err() {
                    tracing::error!("處理請求時 panic，中斷此連線");
                    let _ = tx.send(Frame::error("daemon 處理請求時發生內部錯誤"));
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(Frame::error(format!("無法解析請求：{e}")));
            }
        }
    }
    Ok(())
}

/// 認證失敗後、回覆並關閉連線前的延遲。
pub(crate) const AUTH_FAIL_DELAY: Duration = Duration::from_millis(500);

/// 常數時間比對 token（逐位元組 XOR 累加；長度不同仍走完整比較再判定）。
pub(crate) fn token_matches(given: Option<&str>, expected: &str) -> bool {
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
pub(crate) fn read_token_file(path: &Path) -> Result<String> {
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

/// 決定 token 來源：`--token-file` > `--token`（棄用）> `TERMADE_TOKEN`。
pub(crate) fn resolve_token(opts: &RemoteOptions) -> Result<Option<String>> {
    if let Some(p) = &opts.token_file {
        return read_token_file(p).map(Some);
    }
    if let Some(t) = opts.token.clone().filter(|t| !t.is_empty()) {
        tracing::warn!("--token 已棄用（會出現在 ps 輸出）；請改用 TERMADE_TOKEN 或 --token-file");
        eprintln!("警告：--token 已棄用（會出現在 ps 輸出）；請改用 TERMADE_TOKEN 或 --token-file");
        return Ok(Some(t));
    }
    Ok(std::env::var("TERMADE_TOKEN")
        .ok()
        .filter(|t| !t.is_empty()))
}

/// 檢查 `--listen` 設定：必須有 token；非 loopback 需 `insecure_listen_public`。
pub(crate) fn validate_listen(addr: &str, has_token: bool, insecure_public: bool) -> Result<()> {
    use std::net::ToSocketAddrs;
    if !has_token {
        bail!("--listen 需要認證 token：請設定 TERMADE_TOKEN 或 --token-file <path>");
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
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<()> {
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
                // 目錄是自己的（例如舊版以 755 建立）：直接收緊，不要求使用者手動處理。
                tracing::warn!("{dir:?} 權限過寬，已改為 0700");
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("無法將 {dir:?} 改為 0700，請手動 chmod 700"))?;
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
    pub(crate) fn libc_geteuid() -> u32;
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
            bail!("已有 daemon 在執行（{sock:?}）；請直接連線，或先 `termade shutdown`");
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
        tracing::info!("termade daemon 遠端監聽已啟動：tcp={addr}");
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

    tracing::info!(
        "termade daemon 已啟動：socket={sock:?} log={:?}",
        log_path()
    );

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
