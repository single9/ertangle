//! Session 相關子命令 (whoami、sessions、spawn、prompt、cancel、kill、sleep、wake、status)。

use anyhow::{bail, Context, Result};
use ertangle_core::{AgentKind, PermissionPolicy, Role};
use ertangle_protocol::{Frame, Request, SpawnRequest, Transport};
use tr::tr;

use super::{join, resolve_self_session, roundtrip};

pub(crate) fn cmd_whoami(session: Option<String>, json: bool) -> Result<()> {
    let sid = session
        .or_else(|| ertangle_core::env_var("ERTANGLE_SESSION"))
        .unwrap_or_else(|| "human".into());
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("{}", tr!("未取得快照"));
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
        println!("{}", tr!("{sid}  (非受管 session)", sid = sid));
    }
    Ok(())
}

pub(crate) fn cmd_sessions(json: bool) -> Result<()> {
    let frame = roundtrip(&Request::Snapshot)?;
    let Frame::Snapshot { state } = frame else {
        bail!("{}", tr!("未取得快照"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&*state)?);
        return Ok(());
    }
    if state.sessions.is_empty() {
        println!("{}", tr!("(沒有 session)"));
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
            "{}  {:<22} {:<11} {:<9} {:<6} {:<14} {:<7} {} @{}{}",
            s.id,
            s.label,
            match &s.model {
                Some(m) => format!("{}:{m}", s.kind.id()),
                None => s.kind.id().to_string(),
            },
            status,
            s.permission.id(),
            s.context.map_or_else(|| "-".to_string(), |c| c.short()),
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
    kind: Option<String>,
    prompt: Option<String>,
    cwd: Option<String>,
    label: Option<String>,
    role: Option<String>,
    worktree: bool,
    transport: String,
    permission: Option<String>,
    parent: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    budget: Option<f64>,
    attach: Vec<String>,
    extra: Vec<String>,
) -> Result<()> {
    // `claude:opus` 形式同時指定 model；`--model` 優先。
    let (kind_str, kind_model) = match kind.as_deref() {
        None => (None, None),
        Some(k) => match k.split_once(':') {
            Some((k, m)) if !m.is_empty() => (Some(k.to_string()), Some(m.to_string())),
            _ => (Some(k.to_string()), None),
        },
    };
    let model = model.or(kind_model);
    let kind = match kind_str {
        None => None,
        Some(kind_str) => Some(AgentKind::parse(&kind_str).with_context(|| {
            tr!(
                "未知的 agent：{kind_str} (opencode/claude/codex/antigravity)",
                kind_str = kind_str
            )
        })?),
    };
    let transport = match transport.as_str() {
        "auto" => Transport::Auto,
        "structured" | "acp" => Transport::Structured,
        "pty" => Transport::Pty,
        other => bail!(
            "{}",
            tr!(
                "未知的 transport：{other} (auto/structured/acp/pty)",
                other = other
            )
        ),
    };
    let permission = match permission {
        Some(p) => Some(
            PermissionPolicy::parse(&p)
                .with_context(|| tr!("未知的權限政策：{p} (ask/auto/bypass)", p = p))?,
        ),
        None => None,
    };
    let parent = parent.or_else(|| ertangle_core::env_var("ERTANGLE_SESSION"));
    // 沒指定 --cwd 時用呼叫者所在的目錄 (daemon 是共用的，它的 cwd 不一定是這個專案)。
    let cwd = cwd.or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    });
    let req = Request::Spawn(SpawnRequest {
        kind,
        cwd,
        label,
        role: role.as_deref().map(Role::parse),
        prompt,
        worktree,
        transport,
        permission,
        parent,
        model,
        effort,
        attachments: load_attachments(&attach)?,
        inherit_from: None,
        args: extra,
        budget_usd: budget,
    });
    match roundtrip(&req)? {
        Frame::Ok { note } => {
            println!("{}", note.unwrap_or_else(|| tr!("已建立 session")));
            Ok(())
        }
        Frame::Error { message } => bail!("{message}"),
        _ => Ok(()),
    }
}

pub(crate) fn load_attachments(paths: &[String]) -> Result<Vec<ertangle_core::Attachment>> {
    paths
        .iter()
        .map(|p| {
            ertangle_core::Attachment::load(std::path::Path::new(p)).map_err(|e| anyhow::anyhow!(e))
        })
        .collect()
}

pub(crate) fn cmd_prompt(
    session: String,
    text: Vec<String>,
    attach: Vec<String>,
    queue: bool,
) -> Result<()> {
    if let Frame::Ok { note: Some(n) } = roundtrip(&Request::Prompt {
        session,
        text: join(text),
        attachments: load_attachments(&attach)?,
        queue,
    })? {
        println!("{n}");
    }
    Ok(())
}

pub(crate) fn cmd_queue(session: String, clear: bool) -> Result<()> {
    if clear {
        if let Frame::Ok { note } = roundtrip(&Request::QueueClear { session })? {
            println!("{}", note.unwrap_or_else(|| tr!("已清空排隊")));
        }
        return Ok(());
    }
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("{}", tr!("未取得快照"));
    };
    let sid = state
        .resolve_session_id(&session)
        .unwrap_or_else(|| session.clone());
    if !state.sessions.contains_key(&sid) {
        bail!("{}", tr!("找不到 session {session}", session = session));
    }
    match state.queues.get(&sid) {
        Some(items) if !items.is_empty() => {
            for (i, item) in items.iter().enumerate() {
                println!("{}. {item}", i + 1);
            }
        }
        _ => println!("{}", tr!("(排隊為空)")),
    }
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

fn home_project(cwd: &std::path::Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cwd.to_string_lossy().to_string())
}

pub(crate) fn format_resume_row(
    s: &ertangle_core::Session,
    is_archived: bool,
    parent_map: &std::collections::HashMap<String, String>,
) -> String {
    let tag = if is_archived {
        tr!("已移除")
    } else {
        tr!("已結束")
    };
    let model = s.model.as_deref().unwrap_or("-");
    let note = s.status_note.as_deref().unwrap_or("");
    let mut line = format!(
        "{:<16} {:<20} {:<12} {:<22} [{}] {} {}",
        s.id,
        s.label,
        s.kind.id(),
        model,
        tag,
        s.cwd,
        note
    );
    if let Some(pid) = &s.parent {
        let plabel = parent_map.get(pid).cloned().unwrap_or_else(|| {
            pid.rsplit('_')
                .next()
                .unwrap_or(pid)
                .chars()
                .take(6)
                .collect()
        });
        let trimmed = line.trim_end();
        line = format!("{trimmed} ↳ {plabel}");
    }
    line
}

pub(crate) fn filter_resume_sessions<'a>(
    sessions: impl IntoIterator<Item = (&'a ertangle_core::Session, bool)>,
    here_root: Option<&str>,
) -> Vec<(&'a ertangle_core::Session, bool)> {
    let mut rows: Vec<_> = sessions
        .into_iter()
        .filter(|(s, is_archived)| {
            let resumable = if *is_archived {
                s.native_id.is_some()
            } else {
                !s.status.is_live() && s.native_id.is_some()
            };
            if !resumable {
                return false;
            }
            if let Some(root) = here_root {
                s.project_root() == root
            } else {
                true
            }
        })
        .collect();
    rows.sort_by_key(|a| std::cmp::Reverse(a.0.created_ms));
    rows
}

pub(crate) fn cmd_resume(
    session: Option<String>,
    list: bool,
    scope: String,
    text: Vec<String>,
) -> Result<()> {
    if list || session.is_none() {
        let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
            bail!("{}", tr!("未取得快照"));
        };
        let mut parent_map = std::collections::HashMap::new();
        for s in state
            .sessions
            .values()
            .chain(state.archived_sessions.values())
        {
            parent_map.insert(s.id.clone(), s.label.clone());
        }
        let is_here = scope.eq_ignore_ascii_case("here") || scope.eq_ignore_ascii_case("this");
        let here_root = if is_here {
            let cwd = std::env::current_dir().unwrap_or_default();
            Some(home_project(&cwd))
        } else {
            None
        };
        let all_candidates = state
            .sessions
            .values()
            .map(|s| (s, false))
            .chain(state.archived_sessions.values().map(|s| (s, true)));
        let rows = filter_resume_sessions(all_candidates, here_root.as_deref());
        if rows.is_empty() {
            println!("{}", tr!("沒有可接續的已結束或歷史 session"));
            return Ok(());
        }
        for (s, is_archived) in rows {
            println!("{}", format_resume_row(s, is_archived, &parent_map));
        }
        return Ok(());
    }
    let session = session.unwrap_or_default();
    let prompt = Some(join(text)).filter(|t| !t.trim().is_empty());
    match roundtrip(&Request::Resume { session, prompt })? {
        Frame::Ok { note } => println!("{}", note.unwrap_or_else(|| tr!("已接續"))),
        Frame::Error { message } => bail!("{message}"),
        _ => {}
    }
    Ok(())
}

pub(crate) fn cmd_handoff(
    session: String,
    to: String,
    effort: Option<String>,
    note: Option<String>,
    role: Option<String>,
    parent: Option<String>,
    force: bool,
) -> Result<()> {
    let (kind_str, model) = match to.split_once(':') {
        Some((k, m)) if !m.is_empty() => (k.to_string(), Some(m.to_string())),
        _ => (to.clone(), None),
    };
    let kind = AgentKind::parse(&kind_str).with_context(|| {
        tr!(
            "未知的 agent：{kind_str} (opencode/claude/codex/antigravity)",
            kind_str = kind_str
        )
    })?;
    let parent_change = match parent.as_deref().map(str::trim) {
        None => None,
        Some(p) if p.eq_ignore_ascii_case("none") => Some(ertangle_protocol::ParentChange::None),
        Some(p) if p.eq_ignore_ascii_case("keep") => Some(ertangle_protocol::ParentChange::Keep),
        Some(p) => Some(ertangle_protocol::ParentChange::Session(p.to_string())),
    };
    let role = role.as_deref().map(Role::parse);
    match roundtrip(&Request::Handoff {
        session,
        kind,
        model,
        effort,
        note,
        role,
        parent: parent_change,
        force,
    })? {
        Frame::Ok { note } => println!("{}", note.unwrap_or_else(|| tr!("已交接"))),
        Frame::Error { message } => bail!("{message}"),
        _ => {}
    }
    Ok(())
}

pub(crate) fn cmd_rm(session: String, force: bool) -> Result<()> {
    if let Frame::Ok { note } = roundtrip(&Request::SessionRemove { session, force })? {
        println!("{}", note.unwrap_or_else(|| tr!("已移除")));
    }
    Ok(())
}

pub(crate) fn cmd_sleep(session: String) -> Result<()> {
    let frame = roundtrip(&Request::Sleep { session })?;
    if let Frame::Ok { note } = frame {
        println!("{}", note.unwrap_or_else(|| tr!("已休眠")));
    }
    Ok(())
}

pub(crate) fn cmd_model(
    session: String,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    if model.is_none() && effort.is_none() {
        anyhow::bail!("{}", tr!("請指定 model 或 --effort"));
    }
    let frame = roundtrip(&Request::SessionConfig {
        session,
        model,
        effort,
    })?;
    if let Frame::Ok { note } = frame {
        println!("{}", note.unwrap_or_else(|| tr!("已切換")));
    }
    Ok(())
}

pub(crate) fn cmd_wake(session: String) -> Result<()> {
    let frame = roundtrip(&Request::Wake { session })?;
    if let Frame::Ok { note } = frame {
        println!("{}", note.unwrap_or_else(|| tr!("已喚醒")));
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

pub(crate) fn cmd_budget(session: String, budget_usd: f64) -> Result<()> {
    if let Frame::Ok { note } = roundtrip(&Request::SessionBudget {
        session,
        budget_usd,
    })? {
        println!("{}", note.unwrap_or_else(|| tr!("已更新成本上限")));
    }
    Ok(())
}

pub(crate) fn cmd_ps(session: Option<String>, json: bool) -> Result<()> {
    let frame = roundtrip(&Request::Snapshot)?;
    let Frame::Snapshot { state } = frame else {
        bail!("{}", tr!("未取得快照"));
    };

    let target_sid = session
        .as_ref()
        .map(|s| state.resolve_session_id(s).unwrap_or_else(|| s.clone()));

    let target_sessions: Vec<_> = state
        .sessions_sorted()
        .into_iter()
        .filter(|s| match &target_sid {
            Some(target) => s.id == *target,
            None => true,
        })
        .collect();

    if target_sessions.is_empty() {
        if let Some(target) = session {
            println!("{}", tr!("找不到 session {target}", target = target));
        } else {
            println!("{}", tr!("(沒有 session)"));
        }
        return Ok(());
    }

    if json {
        let list: Vec<_> = target_sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "session": s.id,
                    "label": s.label,
                    "active_tools": s.active_tools,
                    "bg_procs": s.bg_procs,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }

    for s in target_sessions {
        println!("=== {} ({}) ===", s.id, s.label);
        if !s.active_tools.is_empty() {
            println!("{}", tr!("執行中工具／Subagent："));
            for t in &s.active_tools {
                let tag = if t.is_subagent {
                    "[subagent]"
                } else {
                    "[tool]"
                };
                let elapsed = (ertangle_core::now_millis().saturating_sub(t.started_ms)) / 1000;
                println!("  {:<10} {:<15} {} ({}s)", tag, t.name, t.summary, elapsed);
            }
        }
        if !s.bg_procs.is_empty() {
            println!("{}", tr!("背景子處理程序："));
            println!("  {:<8} {:<10} {}", "PID", tr!("執行時間"), tr!("命令列"));
            for p in &s.bg_procs {
                let etime = format_elapsed(p.elapsed_secs);
                println!("  {:<8} {:<10} {}", p.pid, etime, p.cmdline);
            }
        }
        if s.active_tools.is_empty() && s.bg_procs.is_empty() {
            println!("  {}", tr!("(無執行中工具或背景處理程序)"));
        }
        println!();
    }
    Ok(())
}

fn format_elapsed(secs: u64) -> String {
    if secs >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

pub(crate) fn cmd_bg_kill(session: String, pid: Option<u32>, all: bool, force: bool) -> Result<()> {
    if pid.is_none() && !all {
        bail!("{}", tr!("請指定 --pid <PID> 或 --all"));
    }
    let target_pid = if all { None } else { pid };
    match roundtrip(&Request::KillBgProc {
        session: session.clone(),
        pid: target_pid,
        force,
    })? {
        Frame::Ok { note } => {
            if let Some(p) = target_pid {
                println!(
                    "{}",
                    note.unwrap_or_else(|| tr!("已終止處理程序 PID {p}", p = p))
                );
            } else {
                println!(
                    "{}",
                    note.unwrap_or_else(|| tr!(
                        "已清理 {session} 所有背景處理程序",
                        session = session
                    ))
                );
            }
        }
        Frame::Error { message } => bail!("{message}"),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ertangle_core::{AgentKind, Cost, Role, Session, SessionStatus};
    use std::collections::HashMap;

    fn make_session(
        id: &str,
        cwd: &str,
        root: Option<&str>,
        parent: Option<&str>,
        status: SessionStatus,
        native_id: Option<&str>,
    ) -> Session {
        Session {
            id: id.into(),
            kind: AgentKind::OpenCode,
            label: id.into(),
            cwd: cwd.into(),
            branch: None,
            role: Role::Worker,
            status,
            model: Some("test-model".into()),
            cost: Cost::default(),
            parent: parent.map(String::from),
            created_ms: 1000,
            transport: "acp".into(),
            status_note: Some("done".into()),
            repo_root: root.map(String::from),
            context: None,
            native_id: native_id.map(String::from),
            permission: Default::default(),
            budget_usd: None,
            over_budget: false,
            handed_off_to: None,
            effort: None,
            slash_commands: Vec::new(),
            active_tools: Vec::new(),
            bg_procs: Vec::new(),
            tool_calls: 0,
            ended_ms: None,
        }
    }

    #[test]
    fn test_filter_resume_sessions_scope() {
        let s1 = make_session(
            "s1",
            "/proj_a",
            Some("/proj_a"),
            None,
            SessionStatus::Exited,
            Some("n1"),
        );
        let s2 = make_session(
            "s2",
            "/proj_b",
            Some("/proj_b"),
            None,
            SessionStatus::Exited,
            Some("n2"),
        );
        let s3 = make_session(
            "s3",
            "/proj_a",
            Some("/proj_a"),
            None,
            SessionStatus::Busy,
            Some("n3"),
        ); // live
        let s4 = make_session(
            "s4",
            "/proj_a",
            Some("/proj_a"),
            None,
            SessionStatus::Exited,
            None,
        ); // no native_id
        let s_arch = make_session(
            "s_arch",
            "/proj_a",
            Some("/proj_a"),
            None,
            SessionStatus::Idle,
            Some("n4"),
        ); // archived

        let list = vec![
            (&s1, false),
            (&s2, false),
            (&s3, false),
            (&s4, false),
            (&s_arch, true),
        ];

        // Filter scope: here (/proj_a)
        let filtered_here = filter_resume_sessions(list.clone(), Some("/proj_a"));
        let ids: Vec<_> = filtered_here.iter().map(|(s, _)| s.id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s_arch"]);

        // Filter scope: all (None)
        let filtered_all = filter_resume_sessions(list, None);
        let ids_all: Vec<_> = filtered_all.iter().map(|(s, _)| s.id.as_str()).collect();
        assert_eq!(ids_all, vec!["s1", "s2", "s_arch"]);
    }

    #[test]
    fn test_format_resume_row_parent() {
        let mut parent_map = HashMap::new();
        parent_map.insert("p1".into(), "parent-worker".into());

        // 1. With known parent
        let s_with_parent = make_session(
            "c1",
            "/proj",
            None,
            Some("p1"),
            SessionStatus::Exited,
            Some("n1"),
        );
        let row1 = format_resume_row(&s_with_parent, false, &parent_map);
        assert!(
            row1.contains("↳ parent-worker"),
            "Row should contain resolved parent label: {row1}"
        );

        // 2. With unknown parent (short id fallback)
        let s_unknown_parent = make_session(
            "c2",
            "/proj",
            None,
            Some("ses_1234567890"),
            SessionStatus::Exited,
            Some("n2"),
        );
        let row2 = format_resume_row(&s_unknown_parent, false, &parent_map);
        assert!(
            row2.contains("↳ 123456"),
            "Row should contain short parent id: {row2}"
        );

        // 3. Without parent
        let s_no_parent =
            make_session("c3", "/proj", None, None, SessionStatus::Exited, Some("n3"));
        let row3 = format_resume_row(&s_no_parent, false, &parent_map);
        assert!(
            !row3.contains("↳"),
            "Row should not contain parent symbol: {row3}"
        );
    }
}
