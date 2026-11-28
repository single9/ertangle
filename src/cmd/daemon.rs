//! Daemon 管理子命令 (daemon、shutdown)。

use std::path::PathBuf;

use anyhow::Result;
use ertangle_daemon::RemoteOptions;
use ertangle_protocol::Request;
use tr::tr;

use super::roundtrip;

pub(crate) fn cmd_daemon(
    listen: Option<String>,
    web: Option<String>,
    web_port: Option<u16>,
    token: Option<String>,
    token_file: Option<PathBuf>,
    insecure_listen_public: bool,
    keep_awake: Option<bool>,
) -> Result<()> {
    ertangle_daemon::run_remote(RemoteOptions {
        listen,
        web,
        web_port,
        token,
        token_file,
        insecure_listen_public,
        keep_awake,
    })
}

pub(crate) fn cmd_shutdown() -> Result<()> {
    roundtrip(&Request::Shutdown)?;
    println!("{}", tr!("已要求 daemon 關閉"));
    Ok(())
}

/// 要求 daemon 重啟，並等待新 daemon 就緒 (最多約 15 秒)。
pub(crate) fn cmd_restart() -> Result<()> {
    use std::time::{Duration, Instant};
    roundtrip(&Request::Restart)?;
    // 先等舊 daemon 讓出 socket，再等新 daemon 起來；避免誤把舊 daemon 當成新的。
    let down_deadline = Instant::now() + Duration::from_secs(5);
    while ertangle_daemon::is_running() && Instant::now() < down_deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let up_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < up_deadline {
        if ertangle_daemon::is_running() {
            println!("{}", tr!("daemon 已重啟"));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "{}",
        tr!("等不到新的 daemon 就緒 (逾時 10 秒)；請用 `ertangle daemon` 手動啟動")
    )
}

pub(crate) fn cmd_remote_info(json: bool) -> Result<()> {
    let sock = ertangle_daemon::socket_path();
    let running = ertangle_daemon::is_running();
    let token_path = ertangle_daemon::default_token_path();
    let token_exists = token_path.exists();
    let token = if token_exists {
        ertangle_daemon::read_token_file(&token_path).ok()
    } else {
        None
    };
    let tailscale_ip = ertangle_daemon::detect_tailscale_ip()
        .ok()
        .map(|ip| ip.to_string());

    let cfg = ertangle_daemon::config::Config::load_file(
        &ertangle_daemon::config_dir().join("config.toml"),
    );
    let web_port = cfg.web.resolved_port();
    let token_hint = token.as_deref().unwrap_or("<token>");
    let web_url = if let Some(ip) = &tailscale_ip {
        format!("http://{ip}:{web_port}/?token={token_hint}")
    } else {
        format!("http://127.0.0.1:{web_port}/?token={token_hint}")
    };

    if json {
        let val = serde_json::json!({
            "socket": sock.to_string_lossy(),
            "running": running,
            "token_path": token_path.to_string_lossy(),
            "token_exists": token_exists,
            "token": token,
            "tailscale_ip": tailscale_ip,
            "web_url": web_url,
        });
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }

    println!("{}", tr!("ertangle 遠端連線資訊："));
    if running {
        println!(
            "  {}",
            tr!(
                "Daemon 狀態：執行中 (socket: {sock})",
                sock = sock.to_string_lossy()
            )
        );
    } else {
        println!("  {}", tr!("Daemon 狀態：未執行"));
    }

    if token_exists {
        println!(
            "  {}",
            tr!(
                "遠端 Token 路徑：{path} (已就緒)",
                path = token_path.to_string_lossy()
            )
        );
    } else {
        println!(
            "  {}",
            tr!(
                "遠端 Token 路徑：{path} (尚未建立，啟動 --listen 時將自動產生)",
                path = token_path.to_string_lossy()
            )
        );
    }

    if let Some(ip) = &tailscale_ip {
        println!("  {}", tr!("Tailscale IP：{ip} (已連線)", ip = ip));
    } else {
        println!(
            "  {}",
            tr!("Tailscale IP：未偵測到 (請確認 Tailscale 已啟動並登入)")
        );
    }

    println!();
    println!("{}", tr!("連線範例："));
    let token_hint = token.as_deref().unwrap_or("<token>");
    println!(
        "  {}",
        tr!(
            "本機連線：ertangle --remote 127.0.0.1:7420 --token {token}",
            token = token_hint
        )
    );
    if let Some(ip) = &tailscale_ip {
        println!(
            "  {}",
            tr!(
                "Tailscale 遠端連線：ertangle --remote {ip}:7420 --token {token}",
                ip = ip,
                token = token_hint
            )
        );
    }

    println!();
    println!("  {}", tr!("Web 版網址：{url}", url = &web_url));
    if let Some(qr) = ertangle_daemon::web::qr::QrCode::encode(&web_url) {
        println!();
        println!("{}", tr!("掃描 QR Code 連線 Web 版："));
        print!("{}", qr.render_terminal());
    }
    Ok(())
}
