//! Session 相關子命令（whoami、sessions、spawn、prompt、cancel、kill、sleep、wake、status）。

use ade_core::{AgentKind, PermissionPolicy, Role};
use ade_protocol::{Frame, Request, SpawnRequest, Transport};
use anyhow::{bail, Context, Result};

use super::{join, resolve_self_session, roundtrip};

pub(crate) fn cmd_whoami(session: Option<String>, json: bool) -> Result<()> {
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

pub(crate) fn cmd_sessions(json: bool) -> Result<()> {
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
            "{}  {:<22} {:<11} {:<9} {:<6} {:<7} {} @{}{}",
            s.id,
            s.label,
            match &s.model {
                Some(m) => format!("{}:{m}", s.kind.id()),
                None => s.kind.id().to_string(),
            },
            status,
            s.permission.id(),
            transport,
            s.cwd,
            branch,
            note
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_spawn(
    kind: String,
    prompt: Option<String>,
    cwd: Option<String>,
    label: Option<String>,
    role: String,
    worktree: bool,
    transport: String,
    permission: Option<String>,
    parent: Option<String>,
    model: Option<String>,
    extra: Vec<String>,
) -> Result<()> {
    // `claude:opus` 形式同時指定 model；`--model` 優先。
    let (kind_str, kind_model) = match kind.split_once(':') {
        Some((k, m)) if !m.is_empty() => (k.to_string(), Some(m.to_string())),
        _ => (kind.clone(), None),
    };
    let model = model.or(kind_model);
    let kind = AgentKind::parse(&kind_str).with_context(|| {
        format!("未知的 agent：{kind_str}（opencode/claude/codex/antigravity）")
    })?;
    let transport = match transport.as_str() {
        "auto" => Transport::Auto,
        "structured" | "acp" => Transport::Structured,
        "pty" => Transport::Pty,
        other => bail!("未知的 transport：{other}（auto/structured/acp/pty）"),
    };
    let permission = match permission {
        Some(p) => Some(
            PermissionPolicy::parse(&p)
                .with_context(|| format!("未知的權限政策：{p}（ask/auto/bypass）"))?,
        ),
        None => None,
    };
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
        model,
        args: extra,
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

pub(crate) fn cmd_prompt(session: String, text: Vec<String>) -> Result<()> {
    roundtrip(&Request::Prompt {
        session,
        text: join(text),
    })?;
    Ok(())
}

pub(crate) fn cmd_cancel(session: String) -> Result<()> {
    roundtrip(&Request::Cancel { session })?;
    Ok(())
}

pub(crate) fn cmd_kill(session: String) -> Result<()> {
    roundtrip(&Request::Kill { session })?;
    Ok(())
}

pub(crate) fn cmd_sleep(session: String) -> Result<()> {
    let frame = roundtrip(&Request::Sleep { session })?;
    if let Frame::Ok { note } = frame {
        println!("{}", note.unwrap_or_else(|| "已休眠".into()));
    }
    Ok(())
}

pub(crate) fn cmd_wake(session: String) -> Result<()> {
    let frame = roundtrip(&Request::Wake { session })?;
    if let Frame::Ok { note } = frame {
        println!("{}", note.unwrap_or_else(|| "已喚醒".into()));
    }
    Ok(())
}

pub(crate) fn cmd_status(session: Option<String>, text: Vec<String>) -> Result<()> {
    let session = resolve_self_session(session)?;
    roundtrip(&Request::SessionNote {
        session,
        note: join(text),
    })?;
    Ok(())
}
