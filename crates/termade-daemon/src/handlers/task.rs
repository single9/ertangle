//! 任務管理請求處理。

use std::sync::{Arc, Mutex};

use termade_core::{now_millis, MsgKind, State, Target, Task, TaskStatus};
use termade_protocol::Frame;

use crate::{lock, new_id, ClientTx, Shared};

/// 任務請求一次只處理一個：「檢查依賴 → 寫入狀態 → 解鎖後續任務」之間
/// 不能插進另一個任務更新，否則會漏掉解鎖或重複通知。
static TASK_OPS: Mutex<()> = Mutex::new(());

/// 指派訊息的內容。有前置任務時附上它們的標題與負責人，方便後手去問。
fn assignment_body(state: &State, task: &Task) -> String {
    let mut body = format!(
        "[termade 任務指派 id={}]\n標題：{}\n狀態：{}\n指派者：{}\n說明：{}\n",
        task.id,
        task.title,
        task.status.id(),
        task.created_by.as_deref().unwrap_or("human"),
        if task.detail.is_empty() {
            "(無)"
        } else {
            &task.detail
        }
    );
    let deps: Vec<&Task> = task
        .depends_on
        .iter()
        .filter_map(|id| state.tasks.get(id))
        .collect();
    if !deps.is_empty() {
        body.push_str("前置任務：\n");
        for d in deps {
            let owner = d
                .assignee
                .as_ref()
                .map(|a| match state.sessions.get(a) {
                    Some(s) => s.label.clone(),
                    None => a.clone(),
                })
                .unwrap_or_else(|| "無負責人".into());
            body.push_str(&format!(
                "- {}「{}」（{}，負責人：{owner}）\n",
                d.id,
                d.title,
                d.status.id()
            ));
        }
    }
    body.push_str(
        "請先用 termade_task_update 把狀態改為 doing，完成後改為 done 並用 termade_send 回報。",
    );
    body
}

/// 把指派訊息投遞給負責人（閒置中的負責人會被喚醒）。自己指派給自己不通知。
fn notify_assignee(shared: &Arc<Shared>, task: &Task) {
    let Some(target) = task.assignee.clone().filter(|a| !a.is_empty()) else {
        return;
    };
    if task.created_by.as_ref() == Some(&target) {
        return;
    }
    let body = assignment_body(&lock(&shared.inner).state, task);
    let _ = shared.deliver_message(
        task.created_by.clone(),
        Target::Direct(target),
        MsgKind::Handoff,
        body,
        None,
    );
}

/// 任務是否還在等前置任務（`blocked` 且有依賴沒完成）。
fn waiting_on_deps(state: &State, task: &Task) -> bool {
    task.status == TaskStatus::Blocked && !state.unfinished_deps(task).is_empty()
}

pub(crate) fn handle_create(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    title: String,
    detail: String,
    assignee: Option<String>,
    created_by: Option<String>,
    depends_on: Vec<String>,
) {
    let _serial = lock(&TASK_OPS);
    let assignee = assignee
        .map(|a| shared.resolve_session(&a))
        .filter(|a| !a.is_empty());
    let created_by = created_by.map(|c| shared.resolve_session(&c));
    let mut deps: Vec<String> = Vec::new();
    for d in depends_on {
        let d = d.trim().to_string();
        if !d.is_empty() && !deps.contains(&d) {
            deps.push(d);
        }
    }

    let id = new_id("task");
    let mut task = Task {
        id: id.clone(),
        title,
        detail,
        status: TaskStatus::Todo,
        assignee,
        created_by,
        depends_on: deps,
        updated_ms: now_millis(),
    };
    let waiting: Vec<String> = {
        let inner = lock(&shared.inner);
        let missing: Vec<&str> = task
            .depends_on
            .iter()
            .filter(|d| !inner.state.tasks.contains_key(*d))
            .map(|d| d.as_str())
            .collect();
        if !missing.is_empty() {
            let _ = reply.send(Frame::error(format!(
                "找不到前置任務：{}",
                missing.join("、")
            )));
            return;
        }
        inner
            .state
            .unfinished_deps(&task)
            .iter()
            .map(|t| t.id.clone())
            .collect()
    };
    if !waiting.is_empty() {
        task.status = TaskStatus::Blocked;
    }
    shared.emit(termade_core::Event::TaskChanged { task: task.clone() });

    if waiting.is_empty() {
        // 指派任務時直接投遞一則 handoff 訊息，讓閒置中的負責人被喚醒。
        notify_assignee(shared, &task);
        let _ = reply.send(Frame::ok_note(format!("已建立任務 {id}")));
    } else {
        // 還在等前置任務：先不通知負責人，等依賴完成再由 `unblock_dependents` 送。
        let _ = reply.send(Frame::ok_note(format!(
            "已建立任務 {id} （blocked：等待 {} 完成後自動解鎖並通知負責人）",
            waiting.join("、")
        )));
    }
}

pub(crate) fn handle_update(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    id: String,
    status: Option<TaskStatus>,
    assignee: Option<String>,
    detail: Option<String>,
) {
    let _serial = lock(&TASK_OPS);
    let resolved_assignee = assignee.map(|a| shared.resolve_session(&a));
    let existing = lock(&shared.inner).state.tasks.get(&id).cloned();
    match existing {
        Some(mut task) => {
            let previous_assignee = task.assignee.clone();
            let was_done = task.status == TaskStatus::Done;
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
            let newly_assigned = task.assignee.is_some() && task.assignee != previous_assignee;
            let task_id = task.id.clone();
            let now_done = task.status == TaskStatus::Done;

            shared.emit(termade_core::Event::TaskChanged { task: task.clone() });

            // 還在等前置任務的不通知：解鎖時會送給當時的負責人。
            if newly_assigned && !waiting_on_deps(&lock(&shared.inner).state, &task) {
                notify_assignee(shared, &task);
            }
            if now_done && !was_done {
                unblock_dependents(shared, &task_id);
            }

            let _ = reply.send(Frame::ok_note(format!("已更新任務 {task_id}")));
        }
        None => {
            let _ = reply.send(Frame::error(format!("找不到任務 {id}")));
        }
    }
}

/// `done_id` 完成後，把依賴它、狀態是 `blocked`、且所有依賴都已完成的任務
/// 改回 `todo` 並通知負責人。
///
/// 分不出「在等依賴」和「被人手動標成 blocked」：有依賴的任務只要是 `blocked`，
/// 依賴全部完成時就會被解開。
fn unblock_dependents(shared: &Arc<Shared>, done_id: &str) {
    let ready: Vec<Task> = {
        let inner = lock(&shared.inner);
        inner
            .state
            .tasks
            .values()
            .filter(|t| {
                t.status == TaskStatus::Blocked
                    && t.depends_on.iter().any(|d| d == done_id)
                    && inner.state.unfinished_deps(t).is_empty()
            })
            .cloned()
            .collect()
    };
    for mut task in ready {
        task.status = TaskStatus::Todo;
        task.updated_ms = now_millis();
        shared.emit(termade_core::Event::TaskChanged { task: task.clone() });
        notify_assignee(shared, &task);
    }
}
