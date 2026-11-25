//! 多 Agent 團隊範本與協作報告匯出子命令 (playbook、export)。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use ertangle_core::AgentKind;
use ertangle_daemon::config::Config;
use ertangle_protocol::{Frame, Request};
use tr::tr;

use super::{join, roundtrip};

/// 檢查 agent 的可執行檔是否存在於 PATH (或為可直接執行的絕對路徑)。
fn agent_available(kind: &AgentKind) -> bool {
    let (bin, _) = kind.pty_command();
    if bin.contains('/') {
        return Path::new(bin).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
}

/// `ertangle playbook <name> "<goal>"`：展開範本並一次啟動整個團隊。
pub(crate) fn cmd_playbook(name: String, goal: Vec<String>) -> Result<()> {
    let goal = join(goal);
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = Config::load(&ertangle_daemon::config_dir(), &cwd);
    let spawns = cfg.playbook_spawns(&name, &goal).map_err(|e| anyhow!(e))?;
    if spawns.is_empty() {
        bail!("{}", tr!("playbook {name} 沒有成員", name = name));
    }

    // 啟動前先檢查所有成員，避免留下建立到一半的團隊。
    for kind in spawns.iter().filter_map(|req| req.kind.as_ref()) {
        if !agent_available(kind) {
            bail!(
                "{}",
                tr!(
                    "agent {kind} 未安裝；無法啟動 playbook {name}",
                    kind = kind.id(),
                    name = name
                )
            );
        }
    }

    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("{}", tr!("未取得快照"));
    };
    let mut known: BTreeSet<String> = state.sessions.keys().cloned().collect();
    let mut main_id: Option<String> = None;
    let spawn_count = spawns.len();

    for (i, mut req) in spawns.into_iter().enumerate() {
        if i > 0 {
            req.parent = main_id.clone();
        }
        roundtrip(&Request::Spawn(req))?;
        let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
            bail!("{}", tr!("未取得快照"));
        };
        let fresh = state
            .sessions_sorted()
            .into_iter()
            .find(|s| !known.contains(&s.id))
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow!(tr!("無法取得新建立的 session id")))?;
        known.insert(fresh.clone());
        if i == 0 {
            main_id = Some(fresh.clone());
        }
        println!(
            "{}",
            tr!(
                "已建立 {id} ({label})",
                id = fresh,
                label = state.sessions.get(&fresh).map_or("", |s| &s.label)
            )
        );
    }

    println!(
        "{}",
        tr!(
            "已從 playbook {name} 啟動 {count} 個 session (主 session：{id})",
            name = name,
            count = spawn_count,
            id = main_id.unwrap_or_default()
        )
    );
    Ok(())
}

/// `ertangle export <session> [--tree] [--diff] [-o <path>]`。
pub(crate) fn cmd_export(
    session: String,
    tree: bool,
    with_diff: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    let Frame::Snapshot { state } = roundtrip(&Request::Snapshot)? else {
        bail!("{}", tr!("未取得快照"));
    };
    let id = state
        .resolve_session_id(&session)
        .ok_or_else(|| anyhow!("{}", tr!("找不到 session：{session}", session = session)))?;

    let diff = if with_diff {
        match roundtrip(&Request::WorktreeDiff {
            session: id.clone(),
            stat_only: false,
            paths: Vec::new(),
            max_lines: None,
            fold_generated: false,
        })? {
            Frame::Diff { diff, .. } => Some(diff),
            _ => None,
        }
    } else {
        None
    };
    let md = state
        .export_markdown(&id, tree, diff.as_deref())
        .map_err(|e| anyhow!(e))?;

    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &md)?;
        println!("{}", tr!("已匯出至 {path}", path = path.display()));
    } else {
        print!("{md}");
    }
    Ok(())
}
