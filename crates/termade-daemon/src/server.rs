//! 伺服器：連線處理、遠端模式驗證與 daemon 主迴圈。

use super::*;
use tr::tr;

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

    // 遠端連線一律需要認證；沒有設定 token 時 fail closed (不再靜默放行)。
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
                            let _ = tx.send(Frame::error(tr!(
                                "遠端認證失敗：請設定正確的 TERMADE_TOKEN"
                            )));
                            break;
                        }
                    } else {
                        std::thread::sleep(AUTH_FAIL_DELAY);
                        let _ =
                            tx.send(Frame::error(tr!("未認證的遠端連線：請先送 Hello 帶 token")));
                        break;
                    }
                }
                // 單一請求 panic 只斷這條連線，不影響 daemon 與其他 session。
                let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    shared.handle(&tx, req)
                }));
                if handled.is_err() {
                    tracing::error!("處理請求時 panic，中斷此連線");
                    let _ = tx.send(Frame::error(tr!("daemon 處理請求時發生內部錯誤")));
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(Frame::error(tr!("無法解析請求：{e}", e = e)));
            }
        }
    }
    Ok(())
}

/// 認證失敗後、回覆並關閉連線前的延遲。
pub(crate) const AUTH_FAIL_DELAY: Duration = Duration::from_millis(500);

/// 常數時間比對 token (逐位元組 XOR 累加；長度不同仍走完整比較再判定)。
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
    /// `--web host:port` (行動端隨身審核器 Web/WebSocket)
    pub web: Option<String>,
    /// 已棄用的 `--token`
    pub token: Option<String>,
    /// `--token-file <path>` (權限須為 0600)
    pub token_file: Option<PathBuf>,
    /// 允許綁定非 loopback 位址
    pub insecure_listen_public: bool,
}

/// 判斷指定 IP 是否屬於 Tailscale 專用網段 (IPv4 100.64.0.0/10 或 IPv6 fd7a:115c:a1e0::/48)。
pub fn is_tailscale_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let oct = v4.octets();
            oct[0] == 100 && (64..=127).contains(&oct[1])
        }
        std::net::IpAddr::V6(v6) => {
            let seg = v6.segments();
            seg[0] == 0xfd7a && seg[1] == 0x115c && seg[2] == 0xa1e0
        }
    }
}

/// 偵測本機 Tailscale IPv4 位址 (呼叫 `tailscale ip -4` 或讀取環境變數 `TERMADE_TEST_TAILSCALE_IP`)。
pub fn detect_tailscale_ip() -> Result<std::net::Ipv4Addr> {
    if let Ok(ip_env) = std::env::var("TERMADE_TEST_TAILSCALE_IP") {
        let trimmed = ip_env.trim();
        if !trimmed.is_empty() {
            return trimmed
                .parse()
                .with_context(|| tr!("TERMADE_TEST_TAILSCALE_IP 位址無效：{ip}", ip = trimmed));
        }
    }
    let output = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .with_context(|| tr!("無法執行 tailscale 指令，請確認已安裝並加入 PATH"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{}",
            tr!("取得 Tailscale IP 失敗：{stderr}", stderr = stderr.trim())
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let ip_str = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .ok_or_else(|| anyhow::anyhow!(tr!("tailscale ip -4 未輸出有效 IP")))?;
    let ip: std::net::Ipv4Addr = ip_str
        .parse()
        .with_context(|| tr!("Tailscale 輸出的 IP 無效：{ip_str}", ip_str = ip_str))?;
    Ok(ip)
}

/// 解析 `--listen` 規格：
/// 支援 `tailscale` (預設 port 7420)、`tailscale:<port>` 與一般 `host:port`。
/// 回傳 `(解析後的 host:port, 是否屬於 Tailscale 網段)`。
pub fn resolve_listen_addr(spec: &str) -> Result<(String, bool)> {
    if spec == "tailscale" || spec.starts_with("tailscale:") {
        let port: u16 = if spec == "tailscale" {
            7420
        } else {
            let port_str = &spec["tailscale:".len()..];
            port_str
                .parse()
                .with_context(|| tr!("無效的 port 設定：{port_str}", port_str = port_str))?
        };
        let ip = detect_tailscale_ip()?;
        return Ok((format!("{ip}:{port}"), true));
    }

    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = spec
        .to_socket_addrs()
        .with_context(|| tr!("無法解析監聽位址 {spec}", spec = spec))?
        .collect();
    if addrs.is_empty() {
        bail!("{}", tr!("無法解析監聽位址 {spec}", spec = spec));
    }
    let all_tailscale = addrs.iter().all(|a| is_tailscale_ip(a.ip()));
    Ok((spec.to_string(), all_tailscale))
}

/// 解析 `--web` 規格：
/// 支援 `tailscale` (預設 port 7421)、`tailscale:<port>` 與一般 `host:port`。
pub fn resolve_web_addr(spec: &str) -> Result<(String, bool)> {
    if spec == "tailscale" || spec.starts_with("tailscale:") {
        let port: u16 = if spec == "tailscale" {
            7421
        } else {
            let port_str = &spec["tailscale:".len()..];
            port_str
                .parse()
                .with_context(|| tr!("無效的 port 設定：{port_str}", port_str = port_str))?
        };
        let ip = detect_tailscale_ip()?;
        return Ok((format!("{ip}:{port}"), true));
    }

    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = spec
        .to_socket_addrs()
        .with_context(|| tr!("無法解析 Web 監聽位址 {spec}", spec = spec))?
        .collect();
    if addrs.is_empty() {
        bail!("{}", tr!("無法解析 Web 監聽位址 {spec}", spec = spec));
    }
    let all_tailscale = addrs.iter().all(|a| is_tailscale_ip(a.ip()));
    Ok((spec.to_string(), all_tailscale))
}

/// 讀取 token 檔；要求不對 group/other 開放權限。
pub fn read_token_file(path: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path)
        .with_context(|| tr!("讀取 token 檔 {path} 失敗", path = format!("{path:?}")))?;
    if meta.permissions().mode() & 0o077 != 0 {
        bail!(
            "{}",
            tr!(
                "token 檔 {path} 權限過寬，請執行 chmod 600",
                path = format!("{path:?}")
            )
        );
    }
    let t = fs::read_to_string(path)?.trim().to_string();
    if t.is_empty() {
        bail!(
            "{}",
            tr!("token 檔 {path} 是空的", path = format!("{path:?}"))
        );
    }
    Ok(t)
}

fn generate_secure_token() -> String {
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        let mut buf = [0u8; 32];
        if f.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// 取得或自動建立預設遠端 Token 檔 (`~/.local/state/termade/remote.token`，權限為 0600)。
pub fn get_or_create_default_token() -> Result<String> {
    let path = default_token_path();
    if path.exists() {
        return read_token_file(&path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        let _ = ensure_private_dir(parent);
    }
    let token = generate_secure_token();
    use std::os::unix::fs::OpenOptionsExt;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(token.as_bytes())?;
            file.write_all(b"\n")?;
            tracing::info!("已自動產生遠端 Token 並儲存至 {path:?}");
            eprintln!(
                "{}",
                tr!(
                    "已自動產生遠端 Token 並儲存至 {path}",
                    path = format!("{path:?}")
                )
            );
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read_token_file(&path),
        Err(e) => {
            Err(e).with_context(|| tr!("建立 token 檔 {path} 失敗", path = format!("{path:?}")))
        }
    }
}

/// 取得 client 認證用 token：優先讀取環境變數 `TERMADE_TOKEN`，若無則讀取預設 `remote.token`。
pub fn get_client_token() -> Option<String> {
    if let Ok(token) = std::env::var("TERMADE_TOKEN") {
        let t = token.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let default_path = default_token_path();
    if default_path.exists() {
        if let Ok(t) = read_token_file(&default_path) {
            return Some(t);
        }
    }
    None
}

/// 決定 token 來源：`--token-file` > `--token` (棄用)> `TERMADE_TOKEN`。
/// 若開啟遠端監聽 (`opts.listen.is_some()`) 且以上皆無，自動從 `remote.token` 讀取或建立。
pub(crate) fn resolve_token(opts: &RemoteOptions) -> Result<Option<String>> {
    if let Some(p) = &opts.token_file {
        return read_token_file(p).map(Some);
    }
    if let Some(t) = opts.token.clone().filter(|t| !t.is_empty()) {
        tracing::warn!("--token 已棄用 (會出現在 ps 輸出)；請改用 TERMADE_TOKEN 或 --token-file");
        eprintln!(
            "{}",
            tr!("警告：--token 已棄用 (會出現在 ps 輸出)；請改用 TERMADE_TOKEN 或 --token-file")
        );
        return Ok(Some(t));
    }
    if let Ok(t) = std::env::var("TERMADE_TOKEN").map(|s| s.trim().to_string()) {
        if !t.is_empty() {
            return Ok(Some(t));
        }
    }
    if opts.listen.is_some() || opts.web.is_some() {
        let token = get_or_create_default_token()?;
        return Ok(Some(token));
    }
    Ok(None)
}

/// 檢查 `--listen` 設定：必須有 token；非 loopback 且非 Tailscale 需 `insecure_listen_public`。
pub(crate) fn validate_listen(addr: &str, has_token: bool, insecure_public: bool) -> Result<()> {
    use std::net::ToSocketAddrs;
    if !has_token {
        bail!(
            "{}",
            tr!("--listen 需要認證 token：請設定 TERMADE_TOKEN 或 --token-file <path>")
        );
    }
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .with_context(|| tr!("無法解析監聽位址 {addr}", addr = addr))?
        .collect();
    let all_safe = !addrs.is_empty()
        && addrs
            .iter()
            .all(|a| a.ip().is_loopback() || is_tailscale_ip(a.ip()));
    if !all_safe {
        if !insecure_public {
            bail!(
                "{}",
                tr!(
                    "--listen {addr} 不是 loopback 或 Tailscale 位址；TCP 為明文傳輸。\
                     建議綁 127.0.0.1、Tailscale 或搭配 SSH 轉發；若確定要公開，加上 --insecure-listen-public",
                    addr = addr
                )
            );
        }
        eprintln!(
            "{}",
            tr!(
                "警告：daemon 以明文 TCP 對非 loopback/Tailscale 位址 {addr} 開放，token 與內容可被側錄",
                addr = addr
            )
        );
        tracing::warn!("明文 TCP 監聽於非 loopback/Tailscale 位址：{addr}");
    }
    Ok(())
}

/// 確保 socket 目錄存在、為自己所有且不對其他人開放 (新建為 0700)。
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                bail!("{}", tr!("{dir} 不是目錄", dir = format!("{dir:?}")));
            }
            // SAFETY: geteuid 無前置條件、無副作用。
            let me = unsafe { libc_geteuid() };
            if meta.uid() != me {
                bail!(
                    "{}",
                    tr!(
                        "{dir} 不屬於目前使用者 (uid {uid})，拒絕啟動",
                        dir = format!("{dir:?}"),
                        uid = meta.uid()
                    )
                );
            }
            if meta.permissions().mode() & 0o077 != 0 {
                // 目錄是自己的 (例如舊版以 755 建立)：直接收緊，不要求使用者手動處理。
                tracing::warn!("{dir:?} 權限過寬，已改為 0700");
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).with_context(|| {
                    tr!(
                        "無法將 {dir} 改為 0700，請手動 chmod 700",
                        dir = format!("{dir:?}")
                    )
                })?;
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
                .with_context(|| tr!("建立 {dir} 失敗", dir = format!("{dir:?}")))
        }
        Err(e) => Err(e.into()),
    }
}

extern "C" {
    #[link_name = "geteuid"]
    pub(crate) fn libc_geteuid() -> u32;
}

/// 以前景方式跑 daemon (預設只綁本地 Unix socket)。
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

/// 以前景方式跑 daemon，可選同時開放 TCP 遠端監聽 (M5，含安全檢查)。
pub fn run_remote(opts: RemoteOptions) -> Result<()> {
    let resolved_listen = if let Some(addr_spec) = &opts.listen {
        let (resolved, _) = resolve_listen_addr(addr_spec)?;
        Some(resolved)
    } else {
        None
    };

    let resolved_web = if let Some(addr_spec) = &opts.web {
        let (resolved, _) = resolve_web_addr(addr_spec)?;
        Some(resolved)
    } else {
        None
    };

    let auth_token = resolve_token(&opts)?;
    if let Some(addr) = &resolved_listen {
        validate_listen(addr, auth_token.is_some(), opts.insecure_listen_public)?;
    }
    if let Some(addr) = &resolved_web {
        validate_listen(addr, auth_token.is_some(), opts.insecure_listen_public)?;
    }
    let listen_tcp = resolved_listen;
    let sock = socket_path();
    if let Some(dir) = sock.parent() {
        if dir == runtime_dir() {
            ensure_private_dir(dir)?;
        } else {
            fs::create_dir_all(dir)
                .with_context(|| tr!("建立 {dir} 失敗", dir = format!("{dir:?}")))?;
        }
    }
    if sock.exists() {
        if is_running() {
            bail!(
                "{}",
                tr!(
                    "已有 daemon 在執行 ({sock})；請直接連線，或先 `termade shutdown`",
                    sock = format!("{sock:?}")
                )
            );
        }
        let _ = fs::remove_file(&sock);
    }
    let listener = UnixListener::bind(&sock)
        .with_context(|| tr!("綁定 {sock} 失敗", sock = format!("{sock:?}")))?;

    fs::create_dir_all(state_dir())?;

    let default_cwd = std::env::current_dir()?;
    let policy = load_policy(&default_cwd);
    let shared = Shared::restore(&log_path(), policy, default_cwd, auth_token)?;

    // 若指定了 `--listen <host:port>`，在背景執行緒啟動 TCP 監聽 (M5 遠端模式)。
    if let Some(addr) = listen_tcp {
        let tcp_listener =
            TcpListener::bind(&addr).with_context(|| tr!("綁定 TCP {addr} 失敗", addr = addr))?;
        tracing::info!("termade daemon 遠端監聽已啟動：tcp={addr}");
        eprintln!(
            "{}",
            tr!("termade daemon 遠端監聽已啟動：tcp={addr}", addr = addr)
        );
        let token_p = default_token_path();
        if token_p.exists() {
            eprintln!(
                "{}",
                tr!(
                    "遠端認證 Token 檔案：{path}",
                    path = format!("{:?}", token_p)
                )
            );
        }
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

    let running = Arc::new(AtomicBool::new(true));

    // 若指定了 `--web <host:port>`，啟動行動端隨身審核器 HTTP/WebSocket。
    if let Some(web_addr) = resolved_web {
        crate::web::start_web_server(shared.clone(), &web_addr, running.clone())?;
        tracing::info!("termade 行動端隨身審核器已啟動：http://{web_addr}");
        eprintln!(
            "{}",
            tr!(
                "termade 行動端隨身審核器已啟動：http://{web_addr}",
                web_addr = web_addr
            )
        );
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
