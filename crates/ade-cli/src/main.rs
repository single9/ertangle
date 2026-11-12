//! `ade` — 單一執行檔，多種模式。
//!
//! * `ade` / `ade tui`：確保 daemon 在跑，然後 attach TUI。
//! * `ade daemon`：前景執行 daemon。
//! * `ade mcp`：stdio MCP server（供 agent 子程序呼叫協作原語）。
//! * `ade whoami/peers/spawn/prompt/send/inbox/wait/ask/tasks/task/task-update/status/artifact/shutdown`：
//!   CLI，供人類 shell 與不走 MCP 的 agent 直接呼叫。

use ade_core::{
    AgentKind, ArtifactKind, MsgKind, PermissionPolicy, Role, Target, TaskStatus,
};
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
    /// 以 stdio 執行內建的 MCP 協作伺服器
    Mcp {
        /// 此 MCP 伺服器所代表的 session id（未指定則讀 $ADE_SESSION）
        #[arg(long)]
        session: Option<String>,
    },
    /// 查詢自己目前的 session 資訊（讀取 $ADE_SESSION 或 --session）
    Whoami {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 列出所有並行中的 agent sessions（別名 sessions）
    #[command(visible_alias = "peers")]
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
        /// 父 session id（未指定則讀 $ADE_SESSION）
        #[arg(long)]
        parent: Option<String>,
    },
    /// 送 prompt 給某個 session（可用 id 或 label）
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
        /// 收件 session（id 或 label；亦可用 #channel 或 *）
        #[arg(long)]
        to: Option<String>,
        /// 收件頻道名稱
        #[arg(long)]
        channel: Option<String>,
        /// 廣播給所有 session
        #[arg(long)]
        broadcast: bool,
        /// 發送者（未指定則讀 $ADE_SESSION，否則為 human）
        #[arg(long)]
        from: Option<String>,
        /// 訊息種類：chat | handoff | question | answer | status | artifact
        #[arg(long, default_value = "chat")]
        kind: String,
        /// 回覆哪一則訊息 id（msg_...）
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        body: Vec<String>,
    },
    /// 讀取收件匣訊息
    Inbox {
        /// 目標 session（未指定則讀 $ADE_SESSION）
        #[arg(long)]
        session: Option<String>,
        /// 讀取全部歷史訊息（預設只取未讀並推進游標）
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// 阻塞等待下一則訊息
    Wait {
        /// 目標 session（未指定則讀 $ADE_SESSION）
        #[arg(long)]
        session: Option<String>,
        /// 只等特定發送者（id 或 label）
        #[arg(long)]
        from: Option<String>,
        /// 等待逾時毫秒數
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// 向某個 peer 發問並等待回覆（request/response 交握）
    Ask {
        /// 對象 session（id 或 label）
        peer: String,
        /// 發送者（未指定則讀 $ADE_SESSION）
        #[arg(long)]
        session: Option<String>,
        /// 等待逾時毫秒數
        #[arg(long, default_value_t = 60_000)]
        timeout_ms: u64,
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        question: Vec<String>,
    },
    /// 列出任務
    Tasks {
        #[arg(long)]
        json: bool,
    },
    /// 建立任務
    Task {
        title: String,
        #[arg(long)]
        detail: Option<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
    },
    /// 更新任務狀態、指派對象或細節
    TaskUpdate {
        id: String,
        /// 狀態：todo | doing | blocked | review | done
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        detail: Option<String>,
    },
    /// 更新自己在 TUI 上的簡短進度狀態
    Status {
        #[arg(long)]
        session: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// 分享產出物（檔案、分支或 diff）到匯流排
    Artifact {
        path: String,
        /// 類型：file | branch | diff
        #[arg(long, default_value = "file")]
        kind: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        session: Option<String>,
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
        Cmd::Mcp { session } => ade_daemon::mcp::run(session),
        Cmd::Whoami { session, json } => cmd_whoami(session, json),
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
            parent,
        } => cmd_spawn(
            kind, prompt, cwd, label, role, worktree, transport, permission, parent,
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
            kind,
            reply_to,
            body,
        } => {
            let target = if broadcast {
                Target::Broadcast
            } else if let Some(c) = channel {
                Target::Channel(c)
            } else if let Some(t) = to {
                Target::parse(&t)
            } else {
                bail!("請指定 --to、--channel 或 --broadcast");
            };
            let kind = MsgKind::parse(&kind).with_context(|| {
                format!("未知的訊息種類：{kind}（chat/handoff/question/answer/status/artifact）")
            })?;
            let from = from.or_else(|| std::env::var("ADE_SESSION").ok());
            match roundtrip(&Request::BusSend {
                from,
                to: target,
                kind,
                body: join(body),
                reply_to,
            })? {
                Frame::Ok { note: Some(n) } => println!("{n}"),
                _ => println!("已送出訊息"),
            }
            Ok(())
        }
        Cmd::Inbox { session, all, json } => cmd_inbox(session, !all, json),
        Cmd::Wait {
            session,
            from,
            timeout_ms,
            json,
        } => cmd_wait(session, from, timeout_ms, json),
        Cmd::Ask {
            peer,
            session,
            timeout_ms,
            json,
            question,
        } => cmd_ask(peer, session, timeout_ms, json, join(question)),
        Cmd::Tasks { json } => cmd_tasks(json),
        Cmd::Task {
            title,
            detail,
            assignee,
            depends_on,
        } => {
            let created_by = std::env::var("ADE_SESSION").ok();
            match roundtrip(&Request::TaskCreate {
                title,
                detail: detail.unwrap_or_default(),
                assignee,
                created_by,
                depends_on,
            })? {
                Frame::Ok { note: Some(n) } => println!("{n}"),
                _ => println!("已建立任務"),
            }
            Ok(())
        }
        Cmd::TaskUpdate {
            id,
            status,
            assignee,
            detail,
        } => {
            let status = match status {
                Some(s) => Some(TaskStatus::parse(&s).with_context(|| {
                    format!("未知的任務狀態：{s}（todo/doing/blocked/review/done）")
                })?),
                None => None,
            };
            match roundtrip(&Request::TaskUpdate {
                id,
                status,
                assignee,
                detail,
            })? {
                Frame::Ok { note: Some(n) } => println!("{n}"),
                _ => println!("已更新任務"),
            }
            Ok(())
        }
        Cmd::Status { session, text } => {
            let session = resolve_self_session(session)?;
            roundtrip(&Request::SessionNote {
                session,
                note: join(text),
            })?;
            println!("已更新狀態");
            Ok(())
        }
        Cmd::Artifact {
            path,
            kind,
            note,
            session,
        } => {
            let session = resolve_self_session(session)?;
            let kind = ArtifactKind::parse(&kind)
                .with_context(|| format!("未知的產出物類型：{kind}（file/branch/diff）"))?;
            match roundtrip(&Request::ArtifactShare {
                session,
                kind,
                path,
                note: note.unwrap_or_default(),
            })? {
                Frame::Ok { note: Some(n) } => println!("{n}"),
                _ => println!("已分享產出"),
            }
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

fn resolve_self_session(explicit: Option<String>) -> Result<String> {
    explicit
        .or_else(|| std::env::var("ADE_SESSION").ok())
        .context("請指定 --session <id> 或設定環境變數 ADE_SESSION")
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

fn cmd_whoami(session: Option<String>, json: bool) -> Result<()> {
    let sid = session
        .or_else(|| std::env::var("ADE_SESSION").ok())
        .unwrap_or_else(|| "human".into());
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("未取得快照");
    };
    let resolved = state
        .resolve_session_id(&sid)
        .unwrap_or_else(|| sid.clone());
    if let Some(s) = state.sessions.get(&resolved) {
        if json {
            println!("{}", serde_json::to_string_pretty(s)?);
        } else {
            println!(
                "{}  label={}  kind={}  role={}  status={:?}  cwd={}  branch={}",
                s.id,
                s.label,
                s.kind.id(),
                s.role.id(),
                s.status,
                s.cwd,
                s.branch.as_deref().unwrap_or("-")
            );
        }
    } else if json {
        println!("{}", serde_json::json!({ "id": sid, "role": "human" }));
    } else {
        println!("{sid}  (非受管 session)");
    }
    Ok(())
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
        let note = s
            .status_note
            .as_deref()
            .map(|n| format!("  ({n})"))
            .unwrap_or_default();
        println!(
            "{}  {:<22} {:<11} {:<9} {:<4} {} @{}{}",
            s.id,
            s.label,
            s.kind.id(),
            status,
            transport,
            s.cwd,
            branch,
            note
        );
    }
    Ok(())
}

fn cmd_inbox(session: Option<String>, unread_only: bool, json: bool) -> Result<()> {
    let session = resolve_self_session(session)?;
    let Frame::Messages { messages, .. } = roundtrip(&Request::BusInbox {
        session,
        unread_only,
    })?
    else {
        bail!("未取得訊息清單");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
        return Ok(());
    }
    if messages.is_empty() {
        println!("(收件匣為空)");
        return Ok(());
    }
    for m in messages {
        println!(
            "{}  [{}] {} → {:?}: {}",
            m.id,
            m.kind.id(),
            m.from,
            m.to,
            m.body
        );
    }
    Ok(())
}

fn cmd_wait(
    session: Option<String>,
    from: Option<String>,
    timeout_ms: u64,
    json: bool,
) -> Result<()> {
    let session = resolve_self_session(session)?;
    let Frame::Messages {
        messages,
        timed_out,
    } = roundtrip(&Request::BusWait {
        session,
        from,
        reply_to: None,
        timeout_ms,
    })?
    else {
        bail!("等待訊息失敗");
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "messages": messages,
                "timed_out": timed_out,
            }))?
        );
        return Ok(());
    }
    if timed_out || messages.is_empty() {
        println!("(等待逾時)");
        return Ok(());
    }
    for m in messages {
        println!("{}  [{}] {}: {}", m.id, m.kind.id(), m.from, m.body);
    }
    Ok(())
}

fn cmd_ask(
    peer: String,
    session: Option<String>,
    timeout_ms: u64,
    json: bool,
    question: String,
) -> Result<()> {
    let session = resolve_self_session(session)?;
    let send_frame = roundtrip(&Request::BusSend {
        from: Some(session.clone()),
        to: Target::Direct(peer.clone()),
        kind: MsgKind::Question,
        body: question,
        reply_to: None,
    })?;
    let qid = match send_frame {
        Frame::Ok { note: Some(n) } => n.rsplit_once(' ').map(|(_, id)| id.to_string()).unwrap_or(n),
        _ => String::new(),
    };
    let Frame::Messages {
        mut messages,
        timed_out,
    } = roundtrip(&Request::BusWait {
        session,
        from: Some(peer),
        reply_to: if qid.is_empty() { None } else { Some(qid.clone()) },
        timeout_ms,
    })?
    else {
        bail!("等待回覆失敗");
    };
    let reply = messages.pop();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "question_id": qid,
                "reply": reply,
                "timed_out": timed_out,
            }))?
        );
        return Ok(());
    }
    match reply {
        Some(m) => println!("{}", m.body),
        None => println!("(等待回覆逾時)"),
    }
    Ok(())
}

fn cmd_tasks(json: bool) -> Result<()> {
    let frame = roundtrip(&Request::Snapshot)?;
    let Frame::Snapshot { state } = frame else {
        bail!("未取得快照");
    };
    if json {
        let tasks: Vec<_> = state.tasks.values().collect();
        println!("{}", serde_json::to_string_pretty(&tasks)?);
        return Ok(());
    }
    if state.tasks.is_empty() {
        println!("(沒有任務)");
        return Ok(());
    }
    for t in state.tasks.values() {
        let assignee = t.assignee.as_deref().unwrap_or("-");
        println!(
            "{}  {:<9} {:<16} {}",
            t.id,
            t.status.id(),
            assignee,
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
    parent: Option<String>,
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
    let parent = parent.or_else(|| std::env::var("ADE_SESSION").ok());
    let req = Request::Spawn(SpawnRequest {
        kind,
        cwd,
        label,
        role: Role::parse(&role),
        prompt,
        worktree,
        transport,
        permission,
        parent,
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
