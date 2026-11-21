//! 請求分派模組。

pub(crate) mod session;
pub(crate) mod task;
pub(crate) mod worktree;

use std::sync::Arc;
use std::time::Duration;

use ertangle_protocol::{Frame, Request};
use tr::tr;

use crate::{lock, token_matches, ClientTx, Shared, VERSION};

pub(crate) fn dispatch(shared: &Arc<Shared>, reply: &ClientTx, req: Request) {
    match req {
        Request::Hello {
            client,
            since,
            token,
            epoch,
        } => {
            let expected_token = shared.auth_token();
            if let Some(expected) = expected_token.as_deref() {
                if !token_matches(token.as_deref(), expected) {
                    let _ = reply.send(Frame::error(tr!("遠端認證失敗：token 不符")));
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
            // epoch 不符 (或未帶)表示 client 的 seq 屬於別的 log，不能增量。
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
            let _ = reply.send(Frame::ok_note(tr!("daemon 關閉中")));
            let shared = shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                shared.shutdown();
            });
        }

        Request::Restart => {
            let _ = reply.send(Frame::ok_note(tr!("daemon 重啟中")));
            let shared = shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                shared.restart();
            });
        }

        // Session
        Request::Spawn(spawn) => session::handle_spawn(shared, reply, spawn),
        Request::Prompt {
            session,
            text,
            attachments,
            queue,
        } => session::handle_prompt(shared, reply, session, text, attachments, queue),
        Request::Exec { session, command } => session::handle_exec(shared, reply, session, command),
        Request::QueueClear { session } => session::handle_queue_clear(shared, reply, session),
        Request::QueuePop { session } => session::handle_queue_pop(shared, reply, session),
        Request::Cancel { session } => session::handle_cancel(shared, reply, session),
        Request::Kill { session } => session::handle_kill(shared, reply, session),
        Request::KillBgProc {
            session,
            pid,
            force,
        } => session::handle_kill_bg_proc(shared, reply, session, pid, force),
        Request::Resume { session, prompt } => {
            session::handle_resume(shared, reply, session, prompt)
        }
        Request::Handoff {
            session,
            kind,
            model,
            effort,
            note,
            role,
            parent,
            force,
        } => session::handle_handoff(
            shared, reply, session, kind, model, effort, note, role, parent, force,
        ),
        Request::SessionRemove { session, force } => {
            session::handle_remove(shared, reply, session, force)
        }
        Request::Clear { session, force } => session::handle_clear(shared, reply, session, force),
        Request::Sleep { session } => session::handle_sleep(shared, reply, session),
        Request::Wake { session } => session::handle_wake(shared, reply, session),
        Request::SessionConfig {
            session,
            model,
            effort,
        } => session::handle_config(shared, reply, session, model, effort),
        Request::SessionBudget {
            session,
            budget_usd,
        } => session::handle_budget(shared, reply, session, budget_usd),
        Request::McpPermission {
            session,
            title,
            detail,
        } => session::handle_mcp_permission(shared, reply, session, title, detail),
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
        Request::McpQuestion {
            session,
            question,
            header,
            options,
            multi_select,
        } => session::handle_mcp_question(
            shared,
            reply,
            session,
            question,
            header,
            options,
            multi_select,
        ),
        Request::AnswerQuestion {
            session,
            id,
            answers,
        } => session::handle_answer_question(shared, reply, session, id, answers),
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
        Request::WorktreeDiscard {
            session,
            file,
            patch,
        } => worktree::handle_discard(shared, reply, session, file, patch),
        Request::WorktreeMerge {
            session,
            dry_run,
            resolve,
            strategy,
        } => worktree::handle_merge(shared, reply, session, dry_run, resolve, strategy),
        Request::WorktreePr {
            session,
            title,
            body,
        } => worktree::handle_pr(shared, reply, session, title, body),
        Request::WorktreeGc { session, force } => {
            worktree::handle_gc(shared, reply, session, force)
        }
        Request::Prune { project } => crate::sweep::handle_prune(shared, reply, project),
        Request::ReviewComment {
            session,
            location,
            comment,
        } => worktree::handle_comment(shared, reply, session, location, comment),

        Request::ListModels { kind } => list_models(shared, reply, kind),
        // Web Support
        Request::WebControl { enabled, addr } => match enabled {
            None => {
                let info = shared.web_info();
                let _ = reply.send(Frame::ok_note(info));
            }
            Some(true) => {
                let addr_spec = addr.as_deref().unwrap_or("tailscale");
                match shared.start_web(addr_spec, false) {
                    Ok(url) => {
                        let _ = reply.send(Frame::ok_note(tr!("Web 版已啟動：{url}", url = url)));
                    }
                    Err(e) => {
                        let _ = reply.send(Frame::error(tr!("啟動 Web 版失敗：{e}", e = e)));
                    }
                }
            }
            Some(false) => {
                let stopped = shared.stop_web();
                let msg = if stopped {
                    tr!("已停止 Web 版")
                } else {
                    tr!("Web 版未在執行")
                };
                let _ = reply.send(Frame::ok_note(msg));
            }
        },
    }
}

/// `ListModels`：內建／CLI 查到的清單 + config.toml 的 `model`／`models`。
/// 查 CLI 最多要幾秒，放到背景執行緒以免卡住這條連線。
fn list_models(shared: &Arc<Shared>, reply: &ClientTx, kind: String) {
    let Some(agent) = ertangle_core::AgentKind::parse(&kind) else {
        let _ = reply.send(Frame::error(tr!("未知的 agent：{kind}", kind = kind)));
        return;
    };
    let reply = reply.clone();
    let cwd = shared.default_cwd();
    std::thread::spawn(move || {
        let _ = reply.send(Frame::Models {
            kind,
            models: model_list(&agent, &cwd),
        });
    });
}

pub(crate) fn model_list(
    agent: &ertangle_core::AgentKind,
    cwd: &std::path::Path,
) -> Vec<ertangle_protocol::ModelInfo> {
    use ertangle_protocol::ModelInfo;
    let mut out = vec![ModelInfo {
        id: String::new(),
        label: tr!("預設 (依 config.toml)"),
    }];
    if matches!(agent, ertangle_core::AgentKind::ClaudeCode) {
        out.extend(
            ertangle_adapter::models::CLAUDE_MODELS
                .iter()
                .map(|(id, name)| ModelInfo {
                    id: id.to_string(),
                    label: ertangle_core::tr_dyn!(name),
                }),
        );
    } else {
        out.extend(
            ertangle_adapter::models::query_models(agent)
                .into_iter()
                .map(|(label, id)| ModelInfo { id, label }),
        );
    }
    let cfg = crate::config::Config::load(&crate::paths::config_dir(), cwd);
    if let Some(a) = cfg.agents.get(agent.id()) {
        for m in a.model.iter().chain(a.models.iter()) {
            if !out.iter().any(|x| &x.id == m) {
                out.push(ModelInfo {
                    id: m.clone(),
                    label: m.clone(),
                });
            }
        }
    }
    out
}
