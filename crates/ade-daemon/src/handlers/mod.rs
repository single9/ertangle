//! 請求分派模組。

pub(crate) mod session;
pub(crate) mod task;
pub(crate) mod worktree;

use std::sync::Arc;
use std::time::Duration;

use ade_protocol::{Frame, Request};

use crate::{lock, token_matches, ClientTx, Shared, VERSION};

pub(crate) fn dispatch(shared: &Arc<Shared>, reply: &ClientTx, req: Request) {
    match req {
        Request::Hello {
            client,
            since,
            token,
            epoch,
        } => {
            if let Some(expected) = &shared.auth_token {
                if !token_matches(token.as_deref(), expected) {
                    let _ = reply.send(Frame::error("遠端認證失敗：token 不符"));
                    return;
                }
            }
            if client == "auth" {
                return;
            }
            let _ = reply.send(Frame::Ready {
                pid: std::process::id(),
                version: VERSION.to_string(),
                epoch: shared.epoch.clone(),
            });
            // epoch 不符（或未帶）表示 client 的 seq 屬於別的 log，不能增量。
            let since = since.filter(|_| epoch.as_deref() == Some(shared.epoch.as_str()));

            // M4 增量事件同步：若 `since` 在 backlog 範圍內，只補齊差異事件。
            let mut inner = lock(&shared.inner);
            let can_incremental = since.is_some_and(|s| {
                inner
                    .backlog
                    .front()
                    .is_some_and(|first| s + 1 >= first.seq)
                    || s == inner.seq
            });
            if let (Some(since_seq), true) = (since, can_incremental) {
                for rec in inner.backlog.iter().filter(|r| r.seq > since_seq) {
                    let _ = reply.send(Frame::Record {
                        record: Box::new(rec.clone()),
                    });
                }
            } else {
                let snap = inner.state.clone();
                let _ = reply.send(Frame::Snapshot {
                    state: Box::new(snap),
                });
            }
            inner.clients.push(reply.clone());
        }

        Request::Snapshot => {
            let snap = shared.snapshot();
            let _ = reply.send(Frame::Snapshot {
                state: Box::new(snap),
            });
        }

        Request::Shutdown => {
            let _ = reply.send(Frame::ok_note("daemon 關閉中"));
            let shared = shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                shared.shutdown();
            });
        }

        // Session
        Request::Spawn(spawn) => session::handle_spawn(shared, reply, spawn),
        Request::Prompt {
            session,
            text,
            attachments,
        } => session::handle_prompt(shared, reply, session, text, attachments),
        Request::Cancel { session } => session::handle_cancel(shared, reply, session),
        Request::Kill { session } => session::handle_kill(shared, reply, session),
        Request::Handoff {
            session,
            kind,
            model,
            effort,
            note,
        } => session::handle_handoff(shared, reply, session, kind, model, effort, note),
        Request::SessionRemove { session, force } => {
            session::handle_remove(shared, reply, session, force)
        }
        Request::Sleep { session } => session::handle_sleep(shared, reply, session),
        Request::Wake { session } => session::handle_wake(shared, reply, session),
        Request::ApprovalAsk {
            session,
            tool,
            input,
        } => session::handle_approval_ask(shared, reply, session, tool, input),
        Request::Permission {
            session,
            req,
            decision,
        } => session::handle_permission(shared, reply, session, req, decision),
        Request::SessionNote { session, note } => {
            session::handle_note(shared, reply, session, note)
        }
        Request::ArtifactShare {
            session,
            kind,
            path,
            note,
        } => session::handle_artifact(shared, reply, session, kind, path, note),

        // Bus
        Request::BusSend {
            from,
            to,
            kind,
            body,
            reply_to,
        } => shared.handle_bus_send(reply, from, to, kind, body, reply_to),
        Request::BusInbox {
            session,
            unread_only,
        } => shared.handle_bus_inbox(reply, session, unread_only),
        Request::BusWait {
            session,
            from,
            reply_to,
            timeout_ms,
        } => shared.handle_bus_wait(reply, session, from, reply_to, timeout_ms),

        // Task
        Request::TaskCreate {
            title,
            detail,
            assignee,
            created_by,
            depends_on,
        } => task::handle_create(
            shared, reply, title, detail, assignee, created_by, depends_on,
        ),
        Request::TaskUpdate {
            id,
            status,
            assignee,
            detail,
        } => task::handle_update(shared, reply, id, status, assignee, detail),

        // Worktree & Review
        Request::WorktreeList => worktree::handle_list(shared, reply),
        Request::WorktreeDiff { session } => worktree::handle_diff(shared, reply, session),
        Request::WorktreeMerge { session, dry_run } => {
            worktree::handle_merge(shared, reply, session, dry_run)
        }
        Request::WorktreePr {
            session,
            title,
            body,
        } => worktree::handle_pr(shared, reply, session, title, body),
        Request::WorktreeGc { session, force } => {
            worktree::handle_gc(shared, reply, session, force)
        }
        Request::ReviewComment {
            session,
            location,
            comment,
        } => worktree::handle_comment(shared, reply, session, location, comment),
    }
}
