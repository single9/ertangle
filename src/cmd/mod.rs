//! 子命令實作 (CLI 定義留在 main.rs)。

pub(crate) mod bus;
pub(crate) mod check;
pub(crate) mod completions;
pub(crate) mod cost;
pub(crate) mod daemon;
pub(crate) mod export;
pub(crate) mod helptext;
pub(crate) mod session;
pub(crate) mod skill;
pub(crate) mod task;
pub(crate) mod worktree;

use anyhow::{bail, Context, Result};
use ertangle_daemon::Client;
use ertangle_protocol::{Frame, Request};
use tr::tr;

pub(crate) fn join(parts: Vec<String>) -> String {
    parts.join(" ")
}

pub(crate) fn resolve_self_session(explicit: Option<String>) -> Result<String> {
    explicit
        .or_else(|| ertangle_core::env_var("ERTANGLE_SESSION"))
        .with_context(|| tr!("請指定 --session <id> 或設定環境變數 ERTANGLE_SESSION"))
}

pub(crate) fn connect() -> Result<Client> {
    Client::connect()
        .with_context(|| tr!("無法連線 daemon (先執行 `ertangle daemon` 或用 `ertangle` 啟動)"))
}

/// 送一個請求並等待第一個非 Ready 的回覆。
pub(crate) fn roundtrip(req: &Request) -> Result<Frame> {
    let mut c = connect()?;
    c.send(req)?;
    loop {
        match c.recv()? {
            Frame::Ready { .. } => continue,
            Frame::Error { message } => bail!("{message}"),
            other => return Ok(other),
        }
    }
}
