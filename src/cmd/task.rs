//! 任務看板子命令 (tasks、task、task-update)。

use anyhow::{bail, Context, Result};
use termade_core::TaskStatus;
use termade_protocol::{Frame, Request};
use tr::tr;

use super::roundtrip;

pub(crate) fn cmd_tasks(json: bool) -> Result<()> {
    let frame = roundtrip(&Request::Snapshot)?;
    let Frame::Snapshot { state } = frame else {
        bail!("{}", tr!("未取得快照"));
    };
    if json {
        let tasks: Vec<_> = state.tasks.values().collect();
        println!("{}", serde_json::to_string_pretty(&tasks)?);
        return Ok(());
    }
    if state.tasks.is_empty() {
        println!("{}", tr!("(沒有任務)"));
        return Ok(());
    }
    for t in state.tasks.values() {
        let assignee = t.assignee.as_deref().unwrap_or("-");
        let waiting: Vec<&str> = state
            .unfinished_deps(t)
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        let waiting = if waiting.is_empty() {
            String::new()
        } else {
            tr!("  (等待 {list})", list = waiting.join("、"))
        };
        println!(
            "{}  {:<9} {:<16} {}{waiting}",
            t.id,
            t.status.id(),
            assignee,
            t.title
        );
    }
    Ok(())
}

pub(crate) fn cmd_task_create(
    title: String,
    detail: Option<String>,
    assignee: Option<String>,
    depends_on: Vec<String>,
) -> Result<()> {
    let created_by = std::env::var("TERMADE_SESSION").ok();
    let frame = roundtrip(&Request::TaskCreate {
        title,
        detail: detail.unwrap_or_default(),
        assignee,
        created_by,
        depends_on,
    })?;
    if let Frame::Ok { note: Some(n) } = frame {
        println!("{n}");
    }
    Ok(())
}

pub(crate) fn cmd_task_update(
    id: String,
    status: Option<String>,
    assignee: Option<String>,
    detail: Option<String>,
) -> Result<()> {
    let status = match status.as_deref() {
        Some(s) => Some(
            TaskStatus::parse(s)
                .with_context(|| tr!("未知的狀態：{s} (todo/doing/done/blocked)", s = s))?,
        ),
        None => None,
    };
    let frame = roundtrip(&Request::TaskUpdate {
        id,
        status,
        assignee,
        detail,
    })?;
    if let Frame::Ok { note: Some(n) } = frame {
        println!("{n}");
    }
    Ok(())
}
