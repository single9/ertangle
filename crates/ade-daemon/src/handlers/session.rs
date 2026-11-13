//! Session 生命週期與控制請求處理（Spawn、Prompt、Kill、Sleep、Wake、Permission 等）。

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use ade_adapter::{spawn as spawn_adapter, SessionSpec};
use ade_core::{
    now_millis, Artifact, BusMessage, Decision, Event, MsgKind, Session, SessionStatus, Target,
};
use ade_protocol::{Frame, SpawnRequest};

use crate::config;
use crate::git::{git_toplevel, make_worktree};
use crate::paths::{config_dir, socket_path};
use crate::{lock, new_id, ClientTx, SessionEntry, Shared};

pub(crate) fn handle_spawn(shared: &Arc<Shared>, reply: &ClientTx, spawn: SpawnRequest) {
    let id = new_id("ses");
    let mut cwd = spawn
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| shared.default_cwd());
    let mut branch = None;
    let mut repo_root = None;
    // 交接：沿用來源 session 的工作目錄、分支與 repo（不建立新 worktree）。
    if let Some(src) = &spawn.inherit_from {
        let sid = shared.resolve_session(src);
        if let Some(s) = lock(&shared.inner).state.sessions.get(&sid) {
            cwd = PathBuf::from(&s.cwd);
            branch = s.branch.clone();
            repo_root = s.repo_root.clone();
        }
    }

    if spawn.worktree {
        let root = git_toplevel(&cwd).unwrap_or_else(|| cwd.clone());
        match make_worktree(&root, &id) {
            Ok((dir, br)) => {
                repo_root = Some(root.to_string_lossy().to_string());
                cwd = dir;
                branch = Some(br);
            }
            Err(e) => {
                let _ = reply.send(Frame::error(format!("建立 worktree 失敗：{e}")));
                return;
            }
        }
    }

    let parent = spawn.parent.as_ref().map(|p| shared.resolve_session(p));
    let role = spawn.role.clone();
    let label = spawn
        .label
        .clone()
        .unwrap_or_else(|| format!("{}/{}", role.id(), &id[4..8]));

    let global_dir = config_dir();
    let resolved = config::Config::load(&global_dir, &cwd).resolve(
        &spawn.kind,
        &role,
        spawn.model.as_deref(),
        spawn.effort.as_deref(),
        &spawn.args,
        spawn.permission,
    );
    // 子 agent 的權限不得高於父 session 與設定的上限（防呆，不是安全邊界）。
    let mut permission = resolved.permission;
    let mut downgraded = None;
    if let Some(pid) = &parent {
        let parent_perm = lock(&shared.inner)
            .state
            .sessions
            .get(pid)
            .map(|p| p.permission);
        let mut cap = parent_perm;
        if let Some(m) = resolved.max_agent_permission {
            cap = Some(cap.map_or(m, |c| c.min_policy(m)));
        }
        if let Some(cap) = cap {
            let clamped = permission.min_policy(cap);
            if clamped != permission {
                downgraded = Some((permission, clamped));
                permission = clamped;
            }
        }
    }

    let session = Session {
        id: id.clone(),
        kind: spawn.kind.clone(),
        label,
        cwd: cwd.to_string_lossy().to_string(),
        branch,
        role,
        status: SessionStatus::Starting,
        model: resolved.model.clone(),
        cost: Default::default(),
        parent,
        created_ms: now_millis(),
        transport: String::new(),
        status_note: None,
        repo_root,
        context: None,
        permission,
    };
    shared.emit(Event::SessionStarted {
        session: session.clone(),
    });

    let mcp_exe = std::env::current_exe().ok();
    let mut env = vec![
        ("ADE_SESSION".to_string(), id.clone()),
        (
            "ADE_SOCKET".to_string(),
            socket_path().to_string_lossy().to_string(),
        ),
    ];
    if let Some(exe) = &mcp_exe {
        env.push(("ADE_BIN".to_string(), exe.to_string_lossy().to_string()));
        if let Some(bin_dir) = exe.parent() {
            let current_path = std::env::var("PATH").unwrap_or_default();
            env.push((
                "PATH".to_string(),
                format!("{}:{current_path}", bin_dir.to_string_lossy()),
            ));
        }
    }

    env.extend(resolved.env.iter().cloned());
    let (tx, rx) = mpsc::channel();
    let spec = SessionSpec {
        effort: resolved.effort.clone(),
        id: id.clone(),
        kind: spawn.kind.clone(),
        cwd: cwd.clone(),
        transport: spawn.transport,
        bin: resolved.bin,
        args: resolved.args.clone(),
        model: resolved.model.clone(),
        env,
        permission,
        allowed_tools: resolved.allowed_tools.clone(),
        disallowed_tools: resolved.disallowed_tools.clone(),
        read_only: resolved.read_only,
        sandbox: resolved.sandbox.clone(),
        mcp_exe,
        initial_prompt: spawn.prompt.clone(),
        initial_attachments: spawn.attachments.clone(),
        cols: 120,
        rows: 40,
    };

    match spawn_adapter(spec, tx) {
        Ok(ctl) => {
            let transport = ctl.transport().to_string();
            shared.emit(Event::SessionStatus {
                session: id.clone(),
                status: SessionStatus::Idle,
            });
            if transport != "pty" {
                if let Some(prompt) = spawn.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
                    shared.emit(Event::UserPrompt {
                        session: id.clone(),
                        text: with_attachment_summary(prompt, &spawn.attachments),
                    });
                }
            }
            let mut inner = lock(&shared.inner);
            if let Some(s) = inner.state.sessions.get_mut(&id) {
                s.transport = transport;
            }
            inner.sessions.insert(
                id.clone(),
                SessionEntry {
                    ctl: Arc::from(ctl),
                },
            );
            drop(inner);

            let shared_worker = shared.clone();
            let sid = id.clone();
            std::thread::spawn(move || {
                for ev in rx {
                    shared_worker.ingest(&sid, ev);
                }
            });

            let mut note = format!("已建立 session {id}");
            if let Some((from, to)) = downgraded {
                note.push_str(&format!(
                    "（權限由 {} 調降為 {}：不得高於父 session／設定上限）",
                    from.id(),
                    to.id()
                ));
            }
            let _ = reply.send(Frame::ok_note(note));
        }
        Err(e) => {
            // 啟動失敗：不留殘影。剛建立的 worktree 與分支一併清掉，session 直接移除。
            // 交接時 worktree 屬於來源 session，不能因為新 agent 啟動失敗就刪掉。
            if session.branch.is_some() && spawn.inherit_from.is_none() {
                let repo = session
                    .repo_root
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| shared.default_cwd());
                let _ = crate::git::remove_worktree(&repo, &session, true);
            }
            shared.emit(Event::SessionRemoved {
                session: id.clone(),
            });
            let _ = reply.send(Frame::error(format!("啟動 agent 失敗：{e}")));
        }
    }
}

pub(crate) fn handle_prompt(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    text: String,
    attachments: Vec<ade_core::Attachment>,
) {
    match shared.control(&session) {
        Some((sid, ctl)) => {
            shared.wake_if_sleeping(&sid, &ctl);
            let is_pty = lock(&shared.inner)
                .state
                .sessions
                .get(&sid)
                .is_some_and(|s| s.transport == "pty");
            if !is_pty {
                shared.emit(Event::UserPrompt {
                    session: sid.clone(),
                    text: with_attachment_summary(&text, &attachments),
                });
            }
            match ctl.send_input_with(&text, &attachments) {
                Ok(()) => {
                    let _ = reply.send(Frame::ok());
                }
                Err(e) => {
                    let _ = reply.send(Frame::error(format!("送 prompt 失敗：{e}")));
                }
            }
        }
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

/// transcript 的 prompt 回顯：原文後面接上每個附件的一行摘要。
fn with_attachment_summary(text: &str, atts: &[ade_core::Attachment]) -> String {
    let mut out = text.to_string();
    for a in atts {
        out.push('\n');
        out.push_str(&a.summary());
    }
    out
}

/// 交接：停掉舊 session、組簡報、讓新 agent 沿用同一個工作目錄接手。
pub(crate) fn handle_handoff(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    kind: ade_core::AgentKind,
    model: Option<String>,
    effort: Option<String>,
    note: Option<String>,
) {
    let sid = shared.resolve_session(&session);
    let Some(src) = lock(&shared.inner).state.sessions.get(&sid).cloned() else {
        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        return;
    };
    // 舊 agent 還在跑就先停掉，避免兩個 agent 同時改同一個目錄。
    if let Some((_, ctl)) = shared.control(&sid) {
        let _ = ctl.kill();
        shared.emit(Event::SessionExited {
            session: sid.clone(),
            code: None,
        });
    }
    let wt = std::path::Path::new(&src.cwd);
    let mut git_note = String::new();
    if src.branch.is_some() && wt.is_dir() {
        if let Err(e) = crate::git::auto_commit_worktree(wt, &src.label) {
            git_note = format!("（自動 commit 失敗：{e}）\n");
        }
    }
    let (_, status) = crate::git::git_status_summary(wt);
    let log = crate::git::git_output(wt, &["log", "--oneline", "-8"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let stat = crate::git::git_output(wt, &["diff", "--stat", "HEAD~1", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let git_summary = format!("{git_note}工作區狀態：{status}\n最近的 commit：\n{log}\n最後一個 commit 的變更：\n{stat}\n");
    let brief = {
        let inner = lock(&shared.inner);
        crate::handoff::build_handoff_brief(&inner.state, &src, &git_summary, note.as_deref())
    };
    let mut attachments = Vec::new();
    if wt.is_dir() {
        if let Ok(diff) = crate::git::compute_worktree_diff(wt) {
            let mut text = diff;
            if text.len() > 200 * 1024 {
                text.truncate(200 * 1024);
                text.push_str("\n…(diff 過長已截斷)\n");
            }
            attachments.push(ade_core::Attachment {
                name: "handoff.diff".into(),
                mime: "text/plain".into(),
                size: text.len() as u64,
                text: Some(text),
                ..Default::default()
            });
        }
    }
    shared.emit(Event::SessionNote {
        session: sid.clone(),
        note: format!("已交接給 {}", kind.id()),
    });
    handle_spawn(
        shared,
        reply,
        SpawnRequest {
            kind: kind.clone(),
            cwd: Some(src.cwd.clone()),
            label: Some(format!("{}→{}", src.label, kind.id())),
            role: src.role.clone(),
            prompt: Some(brief),
            worktree: false,
            transport: Default::default(),
            permission: Some(src.permission),
            parent: src.parent.clone(),
            model,
            effort,
            attachments,
            inherit_from: Some(sid),
            args: vec![],
        },
    );
}

pub(crate) fn handle_cancel(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    match shared.control(&session) {
        Some((_, ctl)) => {
            let _ = ctl.cancel();
            let _ = reply.send(Frame::ok());
        }
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

pub(crate) fn handle_remove(shared: &Arc<Shared>, reply: &ClientTx, session: String, force: bool) {
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    let Some(s) = sess else {
        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        return;
    };
    if s.status.is_live() {
        let _ = reply.send(Frame::error(
            "session 仍在執行，請先結束（kill）".to_string(),
        ));
        return;
    }
    if let Some(user) = shared.cwd_in_use_by_other(&s) {
        let _ = reply.send(Frame::error(format!("worktree 仍被 {user} 使用，不能移除")));
        return;
    }
    if s.branch.is_some() {
        let repo = crate::git::session_repo(&shared.default_cwd(), &s);
        if !force {
            if let Some(reason) = crate::git::gc_block_reason(&repo, &s) {
                let _ = reply.send(Frame::error(format!(
                    "{}（{reason}）：請先 merge，或用 force 強制移除",
                    s.label
                )));
                return;
            }
        }
        if let Err(e) = crate::git::remove_worktree(&repo, &s, force) {
            let _ = reply.send(Frame::error(format!("移除 worktree 失敗：{e}")));
            return;
        }
    }
    shared.emit(Event::SessionRemoved { session: sid });
    let _ = reply.send(Frame::ok_note(format!("已移除 {}", s.label)));
}

pub(crate) fn handle_kill(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    match shared.control(&session) {
        Some((sid, ctl)) => {
            let _ = ctl.kill();
            shared.emit(Event::SessionExited {
                session: sid,
                code: None,
            });
            let _ = reply.send(Frame::ok());
        }
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

pub(crate) fn handle_sleep(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    match shared.control(&session) {
        Some((sid, ctl)) => match ctl.sleep() {
            Ok(()) => {
                shared.emit(Event::SessionStatus {
                    session: sid.clone(),
                    status: SessionStatus::Sleeping,
                });
                let _ = reply.send(Frame::ok_note(format!("已休眠 {sid}")));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(format!("休眠失敗：{e}")));
            }
        },
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

pub(crate) fn handle_wake(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    match shared.control(&session) {
        Some((sid, ctl)) => match ctl.wake() {
            Ok(()) => {
                shared.emit(Event::SessionStatus {
                    session: sid.clone(),
                    status: SessionStatus::Idle,
                });
                let _ = reply.send(Frame::ok_note(format!("已喚醒 {sid}")));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(format!("喚醒失敗：{e}")));
            }
        },
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

pub(crate) fn handle_permission(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    req: String,
    decision: Decision,
) {
    // MCP 審批請求：把決定交給等待中的 `handle_approval_ask`。
    let waiter = lock(&shared.inner).approval_waiters.remove(&req);
    if let Some(tx) = waiter {
        let _ = tx.send(decision);
        shared.emit(Event::PermissionResolved {
            session: shared.resolve_session(&session),
            req,
            decision,
        });
        let _ = reply.send(Frame::ok());
        return;
    }
    match shared.control(&session) {
        Some((sid, ctl)) => match ctl.resolve_permission(&req, decision) {
            Ok(()) => {
                shared.emit(Event::PermissionResolved {
                    session: sid,
                    req,
                    decision,
                });
                let _ = reply.send(Frame::ok());
            }
            Err(e) => {
                let _ = reply.send(Frame::error(format!("審批回覆失敗：{e}")));
            }
        },
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

/// MCP `ade_approve`：先套用規則與政策，沒有結論就發審批請求並等人回覆（逾時視為拒絕）。
pub(crate) fn handle_approval_ask(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    tool: String,
    input: String,
) {
    const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let sid = shared.resolve_session(&session);
    let p = ade_core::PermissionRequest {
        req: ade_core::new_id("perm"),
        session: sid.clone(),
        title: tool,
        detail: input,
    };
    let word = |d: Decision| match d {
        Decision::Deny => "deny",
        _ => "allow",
    };
    if let Some(d) = shared.auto_decision(&sid, &p) {
        shared.emit(Event::PermissionResolved {
            session: sid,
            req: p.req,
            decision: d,
        });
        let _ = reply.send(Frame::ok_note(word(d)));
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    lock(&shared.inner)
        .approval_waiters
        .insert(p.req.clone(), tx);
    let req = p.req.clone();
    shared.emit(Event::PermissionRequested { request: p });
    let d = match rx.recv_timeout(APPROVAL_TIMEOUT) {
        Ok(d) => d,
        Err(_) => {
            lock(&shared.inner).approval_waiters.remove(&req);
            shared.emit(Event::PermissionResolved {
                session: sid,
                req,
                decision: Decision::Deny,
            });
            Decision::Deny
        }
    };
    let _ = reply.send(Frame::ok_note(word(d)));
}

pub(crate) fn handle_note(shared: &Arc<Shared>, reply: &ClientTx, session: String, note: String) {
    let sid = shared.resolve_session(&session);
    shared.emit(Event::SessionNote { session: sid, note });
    let _ = reply.send(Frame::ok());
}

pub(crate) fn handle_artifact(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    kind: ade_core::ArtifactKind,
    path: String,
    note: String,
) {
    let sid = shared.resolve_session(&session);
    let id = new_id("art");
    let ts = now_millis();
    let artifact = Artifact {
        id: id.clone(),
        session: sid.clone(),
        kind,
        path: path.clone(),
        note: note.clone(),
        ts,
    };
    shared.emit(Event::ArtifactShared { artifact });
    let summary = if note.is_empty() {
        format!("[{}] {}", kind.id(), path)
    } else {
        format!("[{}] {} — {}", kind.id(), path, note)
    };
    let message = BusMessage {
        id: new_id("msg"),
        from: sid,
        to: Target::Broadcast,
        kind: MsgKind::Artifact,
        body: summary,
        reply_to: None,
        ts,
    };
    shared.emit(Event::BusMessage { message });
    let _ = reply.send(Frame::ok_note(format!("已分享產出 {id}")));
}
