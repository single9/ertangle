//! Daemon 管理子命令（daemon、shutdown）。

use std::path::PathBuf;

use ade_daemon::RemoteOptions;
use ade_protocol::Request;
use anyhow::Result;

use super::roundtrip;

pub(crate) fn cmd_daemon(
    listen: Option<String>,
    token: Option<String>,
    token_file: Option<PathBuf>,
    insecure_listen_public: bool,
) -> Result<()> {
    ade_daemon::run_remote(RemoteOptions {
        listen,
        token,
        token_file,
        insecure_listen_public,
    })
}

pub(crate) fn cmd_shutdown() -> Result<()> {
    roundtrip(&Request::Shutdown)?;
    println!("已要求 daemon 關閉");
    Ok(())
}
