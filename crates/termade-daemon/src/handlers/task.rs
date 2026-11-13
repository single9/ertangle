//! 任務管理請求處理。

use std::sync::Arc;

use termade_core::{now_millis, MsgKind, Target, Task, TaskStatus};
use termade_protocol::Frame;

use crate::{lock, new_id, ClientTx, Shared};

pub(crate) fn handle_create(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    title: String,
    detail: String,
    assignee: Option<String>,
    created_by: Option<String>,
    depends_on: Vec<String>,
) {
    let assignee = assignee.map(|a| shared.resolve_session(&a));
    let created_by = created_by.map(|c| shared.resolve_session(&c));
    let id = new_id("task");
    let body = format!(
        "[termade 任務指派 id={id}]\n標題：{title}\n指派者：{}\n說明：{}\n請先用 termade_task_update 把狀態改為 doing，完成後改為 done 並用 termade_send 回報。",
        created_by.as_deref().unwrap_or("human"),
        if detail.is_empty() { "(無)" } else { &detail }
    );
    let task = Task {
        id: id.clone(),
        title,
        detail,
        status: TaskStatus::Todo,
        assignee: assignee.clone(),
        created_by: created_by.clone(),
        depends_on,
        updated_ms: now_millis(),
    };
    shared.emit(termade_core::Event::TaskChanged { task });

    // 指派任務時直接投遞一則 handoff 訊息，讓閒置中的負責人被喚醒。
    if let Some(target) = assignee.filter(|a| !a.is_empty()) {
        if created_by.as_ref() != Some(&target) {
            let _ = shared.deliver_message(
                created_by,
                Target::Direct(target),
                MsgKind::Handoff,
                body,
                None,
            );
        }
    }

    let _ = reply.send(Frame::ok_note(format!("已建立任務 {id}")));
}

pub(crate) fn handle_update(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    id: String,
    status: Option<TaskStatus>,
    assignee: Option<String>,
    detail: Option<String>,
) {
    let resolved_assignee = assignee.map(|a| shared.resolve_session(&a));
    let existing = lock(&shared.inner).state.tasks.get(&id).cloned();
    match existing {
        Some(mut task) => {
            let previous_assignee = task.assignee.clone();
            if let Some(st) = status {
                task.status = st;
            }
            if let Some(a) = resolved_assignee {
                task.assignee = if a.is_empty() { None } else { Some(a) };
            }
            if let Some(d) = detail {
                task.detail = d;
            }
            task.updated_ms = now_millis();

            // 只有「換人做」時才發通知，避免每次狀態更新都吵醒負責人。
            let newly_assigned = task
                .assignee
                .clone()
                .filter(|a| Some(a) != previous_assignee.as_ref());
            let creator = task.created_by.clone();
            let task_id = task.id.clone();
            let title = task.title.clone();
            let task_detail = task.detail.clone();
            let task_status = task.status;

            shared.emit(termade_core::Event::TaskChanged { task });

            if let Some(target) = newly_assigned {
                if creator.as_ref() != Some(&target) {
                    let body = format!(
                        "[termade 任務指派 id={task_id}]\n標題：{title}\n狀態：{}\n說明：{}\n請先用 termade_task_update 把狀態改為 doing，完成後改為 done 並用 termade_send 回報。",
                        task_status.id(),
                        if task_detail.is_empty() { "(無)" } else { &task_detail }
                    );
                    let _ = shared.deliver_message(
                        creator,
                        Target::Direct(target),
                        MsgKind::Handoff,
                        body,
                        None,
                    );
                }
            }

            let _ = reply.send(Frame::ok_note(format!("已更新任務 {task_id}")));
        }
        None => {
            let _ = reply.send(Frame::error(format!("找不到任務 {id}")));
        }
    }
}
