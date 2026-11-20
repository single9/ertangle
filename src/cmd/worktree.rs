//! Worktree 與程式碼審查子命令 (worktrees、diff、review、merge、pr、gc)。

use anyhow::{bail, Result};
use ertangle_protocol::{Frame, MergeStrategy, Request};
use tr::tr;

use super::{join, roundtrip};

pub(crate) fn cmd_worktrees(json: bool) -> Result<()> {
    let Frame::Worktrees { worktrees } = roundtrip(&Request::WorktreeList)? else {
        bail!("{}", tr!("未取得 worktree 清單"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&worktrees)?);
        return Ok(());
    }
    if worktrees.is_empty() {
        println!("{}", tr!("(沒有受管 worktree)"));
        return Ok(());
    }
    for w in worktrees {
        let state_str = if w.live { "live" } else { "exited" };
        println!(
            "{}  {:<18} {:<24} {:<6} {:<24} {}",
            w.session, w.label, w.branch, state_str, w.summary, w.path
        );
    }
    Ok(())
}

pub(crate) fn cmd_diff(session: String) -> Result<()> {
    let Frame::Diff { diff, .. } = roundtrip(&Request::WorktreeDiff { session })? else {
        bail!("{}", tr!("未取得 diff"));
    };
    print!("{diff}");
    Ok(())
}

pub(crate) fn cmd_review(session: String, location: String, comment: Vec<String>) -> Result<()> {
    let frame = roundtrip(&Request::ReviewComment {
        session,
        location,
        comment: join(comment),
    })?;
    if let Frame::Ok { note: Some(n) } = frame {
        println!("{n}");
    }
    Ok(())
}

pub(crate) fn cmd_merge(
    session: String,
    dry_run: bool,
    resolve: bool,
    strategy: Option<MergeStrategy>,
) -> Result<()> {
    let frame = roundtrip(&Request::WorktreeMerge {
        session,
        dry_run,
        resolve,
        strategy,
    })?;
    if let Frame::Ok { note } = frame {
        println!("{}", note.unwrap_or_else(|| tr!("已合併至 main")));
    }
    Ok(())
}

pub(crate) fn cmd_pr(session: String, title: String, body: String) -> Result<()> {
    let frame = roundtrip(&Request::WorktreePr {
        session,
        title,
        body,
    })?;
    if let Frame::Ok { note } = frame {
        println!("{}", note.unwrap_or_else(|| tr!("已建立 PR")));
    }
    Ok(())
}

pub(crate) fn cmd_gc(session: Option<String>, force: bool) -> Result<()> {
    let frame = roundtrip(&Request::WorktreeGc { session, force })?;
    if let Frame::Ok { note: Some(n) } = frame {
        println!("{n}");
    }
    Ok(())
}

pub(crate) fn cmd_prune(project: Option<String>) -> Result<()> {
    let frame = roundtrip(&Request::Prune { project })?;
    if let Frame::Ok { note: Some(n) } = frame {
        println!("{n}");
    }
    Ok(())
}
