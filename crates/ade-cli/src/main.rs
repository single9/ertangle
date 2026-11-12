//! `ade` — 單一執行檔，多種模式。
//!
//! * `ade` / `ade tui`：確保 daemon 在跑，然後 attach TUI。
//! * `ade daemon`：前景執行 daemon。
//! * `ade spawn/prompt/send/tasks/task/sessions/shutdown`：CLI，供 shell 與 agent 使用。

use ade_core::{AgentKind, MsgKind, PermissionPolicy, Role, Target};
use ade_daemon::Client;
use ade_protocol::{Frame, Request, SpawnRequest, Transport};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ade",
    version,
    about = "ade — 終端機原生的 Agent Development Environment"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 啟動 TUI（預設行為）
    Tui,
    /// 以前景方式執行 daemon
    Daemon,
    /// 列出 sessions
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// 建立一個 agent session
    Spawn {
        /// agent 種類：opencode | claude | codex | antigravity
        #[arg(long, default_value = "opencode")]
        kind: String,
        /// 啟動後立刻送出的 prompt
        #[arg(long)]
        prompt: Option<String>,
        /// 工作目錄
        #[arg(long)]
        cwd: Option<String>,
        /// 人類可讀標籤
        #[arg(long)]
        label: Option<String>,
        /// 角色：coordinator | worker | reviewer
        #[arg(long, default_value = "worker")]
        role: String,
        /// 建立 git worktree 並在其中工作
        #[arg(long)]
        worktree: bool,
        /// 傳輸方式：auto | structured（別名 acp）| pty
        #[arg(long, default_value = "auto")]
        transport: String,
        /// 權限政策：ask | auto | bypass
        #[arg(long, default_value = "auto")]
        permission: String,
    },
    /// 送 prompt 給某個 session
    Prompt {
        session: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// 中斷某個 session 的當前回合
    Cancel { session: String },
    /// 結束某個 session
    Kill { session: String },
    /// 送訊息到匯流排（agent 之間溝通）
    Send {
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        broadcast: bool,
        #[arg(long)]
        from: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        body: Vec<String>,
    },
    /// 列出任務
    Tasks,
    /// 建立任務
    Task {
        title: String,
        #[arg(long)]
        detail: Option<String>,
    },
    /// 關閉 daemon
    Shutdown,
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Tui => ade_tui::run(),
        Cmd::Daemon => ade_daemon::run(),
        Cmd::Sessions { json } => cmd_sessions(json),
        Cmd::Spawn {
            kind,
            prompt,
            cwd,
            label,
            role,
            worktree,
            transport,
            permission,
        } => cmd_spawn(
            kind, prompt, cwd, label, role, worktree, transport, permission,
        ),
        Cmd::Prompt { session, text } => {
            roundtrip(&Request::Prompt {
                session,
                text: join(text),
            })?;
            println!("已送出");
            Ok(())
        }
        Cmd::Cancel { session } => {
            roundtrip(&Request::Cancel { session })?;
            println!("已送出中斷");
            Ok(())
        }
        Cmd::Kill { session } => {
            roundtrip(&Request::Kill { session })?;
            println!("已送出結束");
            Ok(())
        }
        Cmd::Send {
            to,
            channel,
            broadcast,
            from,
            body,
        } => {
            let target = if broadcast {
                Target::Broadcast
            } else if let Some(c) = channel {
                Target::Channel(c)
            } else if let Some(t) = to {
                Target::Direct(t)
            } else {
                bail!("請指定 --to、--channel 或 --broadcast");
            };
            roundtrip(&Request::BusSend {
                from,
                to: target,
                kind: MsgKind::Chat,
                body: join(body),
            })?;
            println!("已送出訊息");
            Ok(())
        }
        Cmd::Tasks => cmd_tasks(),
        Cmd::Task { title, detail } => {
            roundtrip(&Request::TaskCreate {
                title,
                detail: detail.unwrap_or_default(),
                assignee: None,
            })?;
            println!("已建立任務");
            Ok(())
        }
        Cmd::Shutdown => {
            roundtrip(&Request::Shutdown)?;
            println!("已要求 daemon 關閉");
            Ok(())
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("ADE_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn join(parts: Vec<String>) -> String {
    parts.join(" ")
}

fn connect() -> Result<Client> {
    Client::connect().context("無法連線 daemon（先執行 `ade daemon` 或用 `ade` 啟動）")
}

/// 送一個請求並等待第一個非 Ready 的回覆。
fn roundtrip(req: &Request) -> Result<Frame> {
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

fn cmd_sessions(json: bool) -> Result<()> {
    let frame = roundtrip(&Request::Snapshot)?;
    let Frame::Snapshot { state } = frame else {
        bail!("未取得快照");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&*state)?);
        return Ok(());
    }
    if state.sessions.is_empty() {
        println!("(沒有 session)");
        return Ok(());
    }
    for s in state.sessions_sorted() {
        let branch = s.branch.as_deref().unwrap_or("-");
        let status = format!("{:?}", s.status).to_lowercase();
        let transport = if s.transport.is_empty() {
            "-"
        } else {
            s.transport.as_str()
        };
        println!(
            "{}  {:<22} {:<11} {:<9} {:<4} {} @{}",
            s.id,
            s.label,
            s.kind.id(),
            status,
            transport,
            s.cwd,
            branch
        );
    }
    Ok(())
}

fn cmd_tasks() -> Result<()> {
    let frame = roundtrip(&Request::Snapshot)?;
    let Frame::Snapshot { state } = frame else {
        bail!("未取得快照");
    };
    if state.tasks.is_empty() {
        println!("(沒有任務)");
        return Ok(());
    }
    for t in state.tasks.values() {
        println!(
            "{}  {:<9} {}",
            t.id,
            format!("{:?}", t.status).to_lowercase(),
            t.title
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_spawn(
    kind: String,
    prompt: Option<String>,
    cwd: Option<String>,
    label: Option<String>,
    role: String,
    worktree: bool,
    transport: String,
    permission: String,
) -> Result<()> {
    let kind = AgentKind::parse(&kind)
        .with_context(|| format!("未知的 agent：{kind}（opencode/claude/codex/antigravity）"))?;
    let transport = match transport.as_str() {
        "auto" => Transport::Auto,
        "structured" | "acp" => Transport::Structured,
        "pty" => Transport::Pty,
        other => bail!("未知的 transport：{other}（auto/structured/acp/pty）"),
    };
    let permission = PermissionPolicy::parse(&permission)
        .with_context(|| format!("未知的權限政策：{permission}（ask/auto/bypass）"))?;
    let req = Request::Spawn(SpawnRequest {
        kind,
        cwd,
        label,
        role: Role::parse(&role),
        prompt,
        worktree,
        transport,
        permission,
        parent: None,
    });
    match roundtrip(&req)? {
        Frame::Ok { note } => {
            println!("{}", note.unwrap_or_else(|| "已建立 session".into()));
            Ok(())
        }
        Frame::Error { message } => bail!("{message}"),
        _ => Ok(()),
    }
}
