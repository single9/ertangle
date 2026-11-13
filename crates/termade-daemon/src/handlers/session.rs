//! Session 生命週期與控制請求處理（Spawn、Prompt、Kill、Sleep、Wake、Permission 等）。

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use termade_adapter::{spawn as spawn_adapter, AgentEvent, SessionSpec};
use termade_core::{
    now_millis, Artifact, BusMessage, Cost, Decision, Event, MsgKind, Session, SessionStatus,
    Target,
};
use termade_protocol::{Frame, SpawnRequest};

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
        // 沒指定就沿用父 session 所在的專案，最後才是 daemon 啟動的目錄。
        .or_else(|| {
            let pid = shared.resolve_session(spawn.parent.as_ref()?);
            lock(&shared.inner)
                .state
                .sessions
                .get(&pid)
                .map(|s| PathBuf::from(s.project_root()))
        })
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

    let parent = spawn.parent.as_ref().map(|p| shared.resolve_session(p));
    let role = spawn.role.clone();
    let label = spawn
        .label
        .clone()
        .unwrap_or_else(|| format!("{}/{}", role.id(), &id[4..8]));

    let global_dir = config_dir();
    let cfg_dir = if cwd.join(".termade").join("config.toml").is_file() {
        cwd.clone()
    } else if shared
        .default_cwd()
        .join(".termade")
        .join("config.toml")
        .is_file()
    {
        shared.default_cwd()
    } else {
        cwd.clone()
    };
    let cfg = config::Config::load(&global_dir, &cfg_dir);
    let resolved = cfg.resolve(
        &spawn.kind,
        &role,
        spawn.model.as_deref(),
        spawn.effort.as_deref(),
        &spawn.args,
        spawn.permission,
    );

    // 有 parent 時檢查子 agent 層數、同時執行數與整棵樹成本上限。
    let mut parent_remaining_usd: Option<f64> = None;
    if let Some(pid) = &parent {
        let inner = lock(&shared.inner);
        let depth = if inner.state.sessions.contains_key(pid) {
            inner.state.session_depth(pid) + 1
        } else {
            1
        };
        if let Some(max_depth) = cfg.limits.max_depth {
            if depth > max_depth {
                let _ = reply.send(Frame::error(format!(
                    "超過子 agent 層數上限 max_depth：目前第 {depth} 層（上限 {max_depth}）"
                )));
                return;
            }
        }
        if let Some(max_children) = cfg.limits.max_children {
            let live_children = inner.state.live_children_count(pid);
            if live_children >= max_children {
                let _ = reply.send(Frame::error(format!(
                    "已達直接子 agent 數量上限 max_children：目前 {live_children} 個執行中（上限 {max_children}）"
                )));
                return;
            }
        }
        if let Some(tree_usd) = cfg.limits.tree_usd {
            let tree_spent = inner.state.tree_cost_usd(pid);
            if tree_spent >= tree_usd {
                let _ = reply.send(Frame::error(format!(
                    "已達整棵樹成本上限 tree_usd：目前已花費 ${tree_spent:.2}（上限 ${tree_usd:.2}）"
                )));
                return;
            }
        }
        if let Some(p_sess) = inner.state.sessions.get(pid) {
            if p_sess.over_budget {
                let _ = reply.send(Frame::error(format!(
                    "父 session {} 已達成本上限 session_usd（over_budget），無法產生子 agent",
                    p_sess.label
                )));
                return;
            }
            if let Some(p_limit) = p_sess.budget_usd {
                let rem = (p_limit - p_sess.cost.usd).max(0.0);
                if rem <= 0.0 {
                    let _ = reply.send(Frame::error(format!(
                        "父 session {} 剩餘預算 session_usd 為 $0.00（已花費 ${:.2} / 上限 ${:.2}），無法產生子 agent",
                        p_sess.label, p_sess.cost.usd, p_limit
                    )));
                    return;
                }
                parent_remaining_usd = Some(rem);
            }
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

    // 解析此 session 的實際成本上限；子 agent 的上限不得高於設定與父 session 剩餘額度。
    let mut budget_usd = cfg.resolve_budget_usd(&role, spawn.budget_usd);
    if parent.is_some() {
        if let Some(cfg_cap) = resolved.session_usd {
            budget_usd = Some(budget_usd.map_or(cfg_cap, |b| b.min(cfg_cap)));
        }
        if let Some(rem) = parent_remaining_usd {
            budget_usd = Some(budget_usd.map_or(rem, |b| b.min(rem)));
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
        native_id: None,
        permission,
        budget_usd,
        over_budget: false,
    };
    shared.emit(Event::SessionStarted {
        session: session.clone(),
    });

    let launch = Launch {
        id: id.clone(),
        kind: spawn.kind.clone(),
        cwd,
        transport: spawn.transport,
        resolved,
        permission,
        prompt: spawn.prompt.clone(),
        attachments: spawn.attachments.clone(),
        resume: None,
        cost_base: Default::default(),
    };
    match launch_session(shared, launch) {
        Ok(()) => {
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

/// 啟動（或接續）一個 agent 程序所需的資訊；`Session` 本身必須已經在 state 裡。
struct Launch {
    id: String,
    kind: termade_core::AgentKind,
    cwd: PathBuf,
    transport: termade_protocol::Transport,
    resolved: config::Resolved,
    permission: termade_core::PermissionPolicy,
    prompt: Option<String>,
    attachments: Vec<termade_core::Attachment>,
    /// 接續用的 agent 原生對話 id；`None` 表示開新對話。
    resume: Option<String>,
    /// 接續前已累計的成本：agent 的成本是程序內累計時，新程序回報的數字要加上這個基底。
    cost_base: Cost,
}

/// spawn 與 resume 共用：組 `SessionSpec` → 啟動 adapter → 登記 `SessionEntry` → 起 ingest 執行緒。
/// 成功時送出 `Idle`（以及初始 prompt 的回顯）；失敗時不動 state，由呼叫端決定怎麼收尾。
fn launch_session(shared: &Arc<Shared>, launch: Launch) -> anyhow::Result<()> {
    let Launch {
        id,
        kind,
        cwd,
        transport,
        resolved,
        permission,
        prompt,
        attachments,
        resume,
        cost_base,
    } = launch;
    let mcp_exe = std::env::current_exe().ok();
    let mut env = vec![
        ("TERMADE_SESSION".to_string(), id.clone()),
        (
            "TERMADE_SOCKET".to_string(),
            socket_path().to_string_lossy().to_string(),
        ),
    ];
    if let Some(exe) = &mcp_exe {
        env.push(("TERMADE_BIN".to_string(), exe.to_string_lossy().to_string()));
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
        kind,
        cwd,
        transport,
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
        initial_prompt: prompt.clone(),
        initial_attachments: attachments.clone(),
        resume,
        cols: 120,
        rows: 40,
    };

    let ctl = spawn_adapter(spec, tx)?;
    let transport = ctl.transport().to_string();
    let run = new_id("run");
    {
        // 先登記再送 Idle：舊程序遲到的事件從這一刻起就會被擋掉（見下方的 ingest 執行緒）。
        let mut inner = lock(&shared.inner);
        if let Some(s) = inner.state.sessions.get_mut(&id) {
            s.transport = transport.clone();
        }
        inner.sessions.insert(
            id.clone(),
            SessionEntry {
                ctl: Arc::from(ctl),
                run: run.clone(),
            },
        );
    }
    shared.emit(Event::SessionStatus {
        session: id.clone(),
        status: SessionStatus::Idle,
    });
    if transport != "pty" {
        if let Some(prompt) = prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            shared.emit(Event::UserPrompt {
                session: id.clone(),
                text: with_attachment_summary(prompt, &attachments),
            });
        }
    }

    let shared_worker = shared.clone();
    std::thread::spawn(move || {
        for mut ev in rx {
            // 這個 session 已被接續成另一個程序：舊程序遲到的事件（尤其是 Exited）不能再套用，
            // 否則會把剛接回來的 session 標成結束並拿掉新的控制把手。
            let superseded = lock(&shared_worker.inner)
                .sessions
                .get(&id)
                .is_some_and(|e| e.run != run);
            if superseded {
                break;
            }
            if let AgentEvent::Cost(c) = &mut ev {
                c.usd += cost_base.usd;
                c.input_tokens += cost_base.input_tokens;
                c.output_tokens += cost_base.output_tokens;
            }
            shared_worker.ingest(&id, ev);
        }
    });
    Ok(())
}

/// 接續：讓已結束的 session 用 agent 原生的對話 id 接回原對話，沿用同一個 termade session id。
pub(crate) fn handle_resume(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    prompt: Option<String>,
) {
    let sid = shared.resolve_session(&session);
    let Some(s) = lock(&shared.inner).state.sessions.get(&sid).cloned() else {
        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        return;
    };
    if s.status.is_live() {
        let _ = reply.send(Frame::error(format!("{} 仍在執行，不需要接續", s.label)));
        return;
    }
    let Some(native_id) = s.native_id.clone() else {
        let _ = reply.send(Frame::error(format!(
            "{} 沒有可接續的對話 id（PTY、舊版 log，或 agent 還沒回報就結束了）；請改用交接（handoff）",
            s.label
        )));
        return;
    };
    let cwd = PathBuf::from(&s.cwd);
    if !cwd.is_dir() {
        let _ = reply.send(Frame::error(format!(
            "{} 的工作目錄 {} 已不存在，無法接續",
            s.label, s.cwd
        )));
        return;
    }
    // 交接後新舊 session 共用同一個 worktree；不要讓兩個 agent 同時改同一個目錄。
    if let Some(user) = s
        .branch
        .as_ref()
        .and_then(|_| shared.cwd_in_use_by_other(&s))
    {
        let _ = reply.send(Frame::error(format!(
            "worktree 正由 {user} 使用，不能接續 {}",
            s.label
        )));
        return;
    }
    // bin、args、env、工具限制重新讀設定；model 與權限以這個 session 原本的為準。
    let mut resolved = config::Config::load(&config_dir(), &cwd).resolve(
        &s.kind,
        &s.role,
        s.model.as_deref(),
        None,
        &[],
        Some(s.permission),
    );
    resolved.permission = s.permission;
    // 只有 Claude 的成本是程序內累計（新程序從 0 算）；Codex 回報的是 thread 累計的 token。
    let cost_base = if matches!(s.kind, termade_core::AgentKind::ClaudeCode) {
        s.cost
    } else {
        Default::default()
    };
    let launch = Launch {
        id: sid.clone(),
        kind: s.kind.clone(),
        cwd,
        transport: termade_protocol::Transport::Structured,
        resolved,
        permission: s.permission,
        prompt: prompt.filter(|p| !p.trim().is_empty()),
        attachments: vec![],
        resume: Some(native_id),
        cost_base,
    };
    match launch_session(shared, launch) {
        Ok(()) => {
            // 清掉「可用 :resume 接續」之類的舊說明。
            if s.status_note.is_some() {
                shared.emit(Event::SessionNote {
                    session: sid.clone(),
                    note: String::new(),
                });
            }
            let _ = reply.send(Frame::ok_note(format!("已接續 {}", s.label)));
        }
        Err(e) => {
            let _ = reply.send(Frame::error(format!("接續 {} 失敗：{e}", s.label)));
        }
    }
}

pub(crate) fn handle_prompt(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    text: String,
    attachments: Vec<termade_core::Attachment>,
) {
    let resolved_id = shared.resolve_session(&session);
    if let Some(s) = lock(&shared.inner).state.sessions.get(&resolved_id) {
        if s.over_budget {
            let _ = reply.send(Frame::error(format!(
                "session {} 已超過成本上限（over_budget）；請先用 `termade budget {} <金額>` 或 TUI `:budget <金額>` 調高上限",
                s.label, s.label
            )));
            return;
        }
    }
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
            let sid = shared.resolve_session(&session);
            let resumable = lock(&shared.inner)
                .state
                .sessions
                .get(&sid)
                .map(|s| s.native_id.is_some());
            let msg = match resumable {
                Some(true) => {
                    format!("{session} 已結束；可用 :resume（或 `termade resume`）接續原對話")
                }
                Some(false) => format!("{session} 已結束，且沒有可接續的對話 id"),
                None => format!("找不到 session {session}"),
            };
            let _ = reply.send(Frame::error(msg));
        }
    }
}

/// transcript 的 prompt 回顯：原文後面接上每個附件的一行摘要。
fn with_attachment_summary(text: &str, atts: &[termade_core::Attachment]) -> String {
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
    kind: termade_core::AgentKind,
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
            attachments.push(termade_core::Attachment {
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
            budget_usd: None,
        },
    );
}

pub(crate) fn handle_budget(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    budget_usd: f64,
) {
    if !budget_usd.is_finite() || budget_usd <= 0.0 {
        let _ = reply.send(Frame::error("預算上限必須為大於 0 的數字"));
        return;
    }
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    let Some(s) = sess else {
        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        return;
    };
    if budget_usd <= s.cost.usd {
        let _ = reply.send(Frame::error(format!(
            "新的上限（${budget_usd:.2}）必須高於目前已花費金額（${:.2}）",
            s.cost.usd
        )));
        return;
    }
    if let Ok(mut warned) = shared.budget_warned.lock() {
        warned.remove(&sid);
    }
    shared.emit(Event::SessionBudget {
        session: sid,
        budget_usd: Some(budget_usd),
    });
    let _ = reply.send(Frame::ok_note(format!(
        "已將 {} 的預算上限設為 ${budget_usd:.2}",
        s.label
    )));
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
            // 立刻拿掉控制把手：程序的結束事件要過一會才到，這段期間送來的 prompt
            // 不該再寫進已經被殺掉的程序（會把剛標成結束的 session 又變回 busy，擋住接續）。
            lock(&shared.inner).sessions.remove(&sid);
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

/// MCP `termade_approve`：先套用規則與政策，沒有結論就發審批請求並等人回覆（逾時視為拒絕）。
pub(crate) fn handle_approval_ask(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    tool: String,
    input: String,
) {
    const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let sid = shared.resolve_session(&session);
    let p = termade_core::PermissionRequest {
        req: termade_core::new_id("perm"),
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
    kind: termade_core::ArtifactKind,
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
