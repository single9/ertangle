//! 任務管理請求處理。

use std::sync::{Arc, Mutex};

use ertangle_core::{now_millis, MsgKind, Role, State, Target, Task, TaskStatus};
use ertangle_protocol::Frame;
use tr::tr;

use crate::{lock, new_id, ClientTx, Shared};

/// 任務請求一次只處理一個：「檢查相依性 → 寫入狀態 → 解鎖後續任務」之間
/// 不能插進另一個任務更新，否則會漏掉解鎖或重複通知。
static TASK_OPS: Mutex<()> = Mutex::new(());

/// 指派訊息的內容。有前置任務時附上它們的標題與負責人，方便後手去問。
fn assignment_body(state: &State, task: &Task) -> String {
    let mut body = format!(
        "[ertangle 任務指派 id={}]\n標題：{}\n狀態：{}\n指派者：{}\n說明：{}\n",
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
                "- {}「{}」({}，負責人：{owner})\n",
                d.id,
                d.title,
                d.status.id()
            ));
        }
    }
    body.trim_end().to_string()
}

/// Prefix of the note line that reports a delivery problem (parsed by the MCP layer).
pub(crate) const WARNING_PREFIX: &str = "warning: ";

/// Whether the assignee session is currently live. Unknown ids count as not live.
fn assignee_is_live(shared: &Arc<Shared>, target: &str) -> bool {
    lock(&shared.inner)
        .state
        .sessions
        .get(target)
        .is_some_and(|s| s.status.is_live())
}

/// Builds the warning for an assignee that could not be woken.
fn dead_assignee_warning(shared: &Arc<Shared>, target: &str, reason: &str) -> String {
    let status = lock(&shared.inner)
        .state
        .sessions
        .get(target)
        .map(|s| format!("{:?}", s.status).to_lowercase())
        .unwrap_or_else(|| "unknown".into());
    format!(
        "{WARNING_PREFIX}assignee {target} is {status} and could not be resumed ({reason}); reassign the task or :resume it"
    )
}

/// Delivers the assignment to the assignee (an idle assignee is woken). Self-assignment
/// does not notify. A dead (failed/exited) assignee is auto-resumed with the assignment
/// as its prompt; if that is impossible, returns a warning for the caller to surface.
fn notify_assignee(shared: &Arc<Shared>, task: &Task) -> Option<String> {
    let target = task.assignee.clone().filter(|a| !a.is_empty())?;
    if task.created_by.as_ref() == Some(&target) {
        return None;
    }
    let body = assignment_body(&lock(&shared.inner).state, task);
    if !assignee_is_live(shared, &target) {
        return match crate::handlers::session::resume_session(
            shared,
            &target,
            Some(body),
            Some(ertangle_core::PROMPT_FROM_ERTANGLE.to_string()),
        ) {
            Ok(_) => None,
            Err(e) => Some(dead_assignee_warning(shared, &target, &e.to_string())),
        };
    }
    let _ = shared.deliver_message(
        task.created_by.clone(),
        Target::Direct(target),
        MsgKind::Handoff,
        body,
        None,
    );
    None
}

/// 當任務狀態變為 Review 時，通知同專案的 Reviewer 進行審查。
fn notify_reviewer_on_review(shared: &Arc<Shared>, task: &Task) {
    let (reviewer, worker_label, worker_branch) = {
        let inner = lock(&shared.inner);
        let worker_sess = task
            .assignee
            .as_ref()
            .and_then(|a| inner.state.sessions.get(a));
        let worker_label = worker_sess
            .map(|s| s.label.clone())
            .unwrap_or_else(|| task.assignee.clone().unwrap_or_else(|| "未知".into()));
        let worker_branch = worker_sess
            .and_then(|s| s.branch.clone())
            .unwrap_or_else(|| "未知".into());
        let proj_root = worker_sess
            .map(|s| s.project_root().to_string())
            .or_else(|| {
                task.created_by
                    .as_ref()
                    .and_then(|c| inner.state.sessions.get(c))
                    .map(|s| s.project_root().to_string())
            });
        let reviewer = inner
            .state
            .sessions
            .values()
            .find(|s| {
                s.role == Role::Reviewer
                    && s.status.is_live()
                    && proj_root.as_deref().is_none_or(|pr| s.project_root() == pr)
            })
            .map(|s| s.id.clone());
        (reviewer, worker_label, worker_branch)
    };

    if let Some(target) = reviewer {
        let body = format!(
            "[ertangle 審查請求 id={task_id}]\n任務：{title}\n負責 Worker：{worker_label}\n分支：{worker_branch}\n狀態：review\n\n請透過 ertangle_diff 檢視變更並進行程式碼審查。",
            task_id = task.id,
            title = task.title,
            worker_label = worker_label,
            worker_branch = worker_branch,
        );
        let _ = shared.deliver_message(
            task.assignee.clone(),
            Target::Direct(target),
            MsgKind::Handoff,
            body,
            None,
        );
    }
}

/// 任務是否還在等前置任務 (`blocked` 且有相依性沒完成)。
fn waiting_on_deps(state: &State, task: &Task) -> bool {
    task.status == TaskStatus::Blocked && !state.unfinished_deps(task).is_empty()
}

/// Appends the optional warning as its own line of the reply note.
fn with_warning(note: String, warning: Option<String>) -> String {
    match warning {
        Some(w) => format!("{note}\n{w}"),
        None => note,
    }
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
            let _ = reply.send(Frame::error(tr!(
                "找不到前置任務：{list}",
                list = missing.join("、")
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
    shared.emit(ertangle_core::Event::TaskChanged { task: task.clone() });

    if waiting.is_empty() {
        // 指派任務時直接投遞一則 handoff 訊息，讓閒置中的負責人被喚醒。
        let warning = notify_assignee(shared, &task);
        let _ = reply.send(Frame::ok_note(with_warning(
            tr!("已建立任務 {id}", id = id),
            warning,
        )));
    } else {
        // 還在等前置任務：先不通知負責人，等相依性完成再由 `unblock_dependents` 送。
        let _ = reply.send(Frame::ok_note(tr!(
            "已建立任務 {id} (blocked：等待 {waiting} 完成後自動解鎖並通知負責人)",
            id = id,
            waiting = waiting.join("、")
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
            let was_review = task.status == TaskStatus::Review;
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
            let now_review = task.status == TaskStatus::Review;

            shared.emit(ertangle_core::Event::TaskChanged { task: task.clone() });

            // 還在等前置任務的不通知：解鎖時會送給當時的負責人。
            let mut warning = None;
            if newly_assigned && !waiting_on_deps(&lock(&shared.inner).state, &task) {
                warning = notify_assignee(shared, &task);
            }
            if now_done && !was_done {
                unblock_dependents(shared, &task_id);
            }
            if now_review && !was_review {
                notify_reviewer_on_review(shared, &task);
            }

            let _ = reply.send(Frame::ok_note(with_warning(
                tr!("已更新任務 {task_id}", task_id = task_id),
                warning,
            )));
        }
        None => {
            let _ = reply.send(Frame::error(tr!("找不到任務 {id}", id = id)));
        }
    }
}

/// `done_id` 完成後，把相依於它、狀態是 `blocked`、且所有相依性都已完成的任務
/// 改回 `todo` 並通知負責人。
///
/// 分不出「在等相依性」和「被人手動標成 blocked」：有相依性的任務只要是 `blocked`，
/// 相依性全部完成時就會被解開。
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
        shared.emit(ertangle_core::Event::TaskChanged { task: task.clone() });
        notify_assignee(shared, &task);
    }
}
