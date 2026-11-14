//! Daemon 管理子命令（daemon、shutdown）。

use std::path::PathBuf;

use anyhow::Result;
use termade_daemon::RemoteOptions;
use termade_protocol::Request;
use tr::tr;

use super::roundtrip;

pub(crate) fn cmd_daemon(
    listen: Option<String>,
    token: Option<String>,
    token_file: Option<PathBuf>,
    insecure_listen_public: bool,
) -> Result<()> {
    termade_daemon::run_remote(RemoteOptions {
        listen,
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
