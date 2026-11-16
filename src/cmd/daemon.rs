//! Daemon 管理子命令 (daemon、shutdown)。

use std::path::PathBuf;

use anyhow::Result;
use termade_daemon::RemoteOptions;
use termade_protocol::Request;
use tr::tr;

use super::roundtrip;

pub(crate) fn cmd_daemon(
    listen: Option<String>,
    web: Option<String>,
    token: Option<String>,
    token_file: Option<PathBuf>,
    insecure_listen_public: bool,
) -> Result<()> {
    termade_daemon::run_remote(RemoteOptions {
        listen,
        web,
        token,
        token_file,
        insecure_listen_public,
    })
}

pub(crate) fn cmd_shutdown() -> Result<()> {
    roundtrip(&Request::Shutdown)?;
    println!("{}", tr!("已要求 daemon 關閉"));
    Ok(())
}

pub(crate) fn cmd_remote_info(json: bool) -> Result<()> {
    let sock = termade_daemon::socket_path();
    let running = termade_daemon::is_running();
    let token_path = termade_daemon::default_token_path();
    let token_exists = token_path.exists();
    let token = if token_exists {
        termade_daemon::read_token_file(&token_path).ok()
    } else {
        None
    };
    let tailscale_ip = termade_daemon::detect_tailscale_ip()
        .ok()
        .map(|ip| ip.to_string());

    let token_hint = token.as_deref().unwrap_or("<token>");
    let web_url = if let Some(ip) = &tailscale_ip {
        format!("http://{ip}:7421/#token={token_hint}")
    } else {
        format!("http://127.0.0.1:7421/#token={token_hint}")
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

    println!("{}", tr!("termade 遠端連線資訊："));
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
            "本機連線：termade --remote 127.0.0.1:7420 --token {token}",
            token = token_hint
        )
    );
    if let Some(ip) = &tailscale_ip {
        println!(
            "  {}",
            tr!(
                "Tailscale 遠端連線：termade --remote {ip}:7420 --token {token}",
                ip = ip,
                token = token_hint
            )
        );
    }

    println!();
    println!("  {}", tr!("Web 版網址：{url}", url = &web_url));
    if let Some(qr) = termade_daemon::web::qr::QrCode::encode(&web_url) {
        println!();
        println!("{}", tr!("掃描 QR Code 連線 Web 版："));
        print!("{}", qr.render_terminal());
    }
    Ok(())
}
