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
use crate::{lock, new_id, ClientTx, QueuedPrompt, SessionEntry, Shared, MAX_QUEUE_LEN};

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
    let mut resolved = cfg.resolve(
        &spawn.kind,
        &role,
        spawn.model.as_deref(),
        spawn.effort.as_deref(),
        &spawn.args,
        spawn.permission,
    );

    // agent 自己建立 session：先檢查 model／effort 是否超出免確認範圍（避免自動開出高消耗的 session）。
    let mut spawn_note: Option<String> = None;
    if let Some(pid) = &parent {
        let pol = &cfg.agent_spawn;
        let check = pol.check(resolved.model.as_deref(), resolved.effort.as_deref());
        let exceeds = !matches!(check, config::SpawnCheck::Fine);
        let must_ask = pol.confirm() == "always" || (pol.confirm() == "costly" && exceeds);
        if must_ask {
            let reasons = match &check {
                config::SpawnCheck::Exceeds { reasons, .. } => reasons.join("；"),
                config::SpawnCheck::Fine => "設定要求每次都先確認".to_string(),
            };
            let detail = format!(
                "agent {pid} 要建立 {} session：model={} effort={}\n原因：{reasons}\n任務：{}",
                spawn.kind,
                resolved.model.as_deref().unwrap_or("預設"),
                resolved.effort.as_deref().unwrap_or("預設"),
                spawn
                    .prompt
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect::<String>(),
            );
            let d = ask_user(shared, pid, "建立子 agent".into(), detail, false);
            if d == Decision::Deny {
                let _ = reply.send(Frame::error(
                    "使用者拒絕（或逾時未回應）建立這個 session；請改用較便宜的 model／較低的 effort，或請使用者自己建立",
                ));
                return;
            }
        } else if let config::SpawnCheck::Exceeds {
            effort_too_high,
            model_not_allowed,
            reasons,
        } = check
        {
            // confirm = never：effort 調降，model 直接拒絕。
            if model_not_allowed {
                let _ = reply.send(Frame::error(format!(
                    "{}（agent_spawn.confirm = never）",
                    reasons.join("；")
                )));
                return;
            }
            if effort_too_high {
                let max = pol.max_effort().to_string();
                spawn_note = Some(format!("effort 已調降為 {max}"));
                resolved.effort = Some(max);
            }
        }
    }

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

    // agent 自己建立的 session 預設用 worktree（`[agent_spawn] worktree`）；不在 git repo 內就略過。
    let forced_worktree = parent.is_some()
        && spawn.inherit_from.is_none()
        && cfg.agent_spawn.worktree()
        && git_toplevel(&cwd).is_some();
    if spawn.worktree || forced_worktree {
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
        handed_off_to: None,
        effort: resolved.effort.clone(),
        slash_commands: Vec::new(),
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
            // 交接：把舊 session 的匯流排身分移交給新 session（之後送給舊 session 的訊息改投這裡）。
            if let Some(src) = &spawn.inherit_from {
                let from = shared.resolve_session(src);
                shared.emit(Event::SessionHandedOff {
                    from: from.clone(),
                    to: id.clone(),
                });
                // 舊 session 還沒提醒的未讀訊息已列在交接簡報裡，不再另外提醒接手者（T25）。
                lock(&shared.inner).pending_nudge.remove(&from);
            }
            let mut note = format!("已建立 session {id}");
            if let Some((from, to)) = downgraded {
                note.push_str(&format!(
                    "（權限由 {} 調降為 {}：不得高於父 session／設定上限）",
                    from.id(),
                    to.id()
                ));
            }
            if let Some(n) = &spawn_note {
                note.push_str(&format!("（{n}）"));
            }
            if forced_worktree && !spawn.worktree {
                note.push_str("（依 agent_spawn.worktree 使用獨立 worktree）");
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

/// 啟動（或接續）一個 agent 處理程序所需的資訊；`Session` 本身必須已經在 state 裡。
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
    /// 接續前已累計的成本：agent 的成本是處理程序內累計時，新處理程序回報的數字要加上這個基底。
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
    // 協作說明（T20）：spawn 與 resume 都重新組，接續時 agent 的系統提示不會保留上一次的。
    let system_append = {
        let inner = lock(&shared.inner);
        crate::collab::for_session(&inner.state, &id, &config_dir(), &cwd)
    };
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
        system_append,
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
        // 先登記再送 Idle：舊處理程序遲到的事件從這一刻起就會被擋掉（見下方的 ingest 執行緒）。
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
            // 這個 session 已被接續成另一個處理程序：舊處理程序遲到的事件（尤其是 Exited）不能再套用，
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
    // 只有 Claude 的成本是處理程序內累計（新處理程序從 0 算）；Codex 回報的是 thread 累計的 token。
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

/// 實際送出一則 prompt 給目標 session（由 `handle_prompt` 與佇列自動送出共用）。
pub(crate) fn send_prompt(
    shared: &Arc<Shared>,
    sid: &str,
    text: &str,
    attachments: &[termade_core::Attachment],
) -> anyhow::Result<()> {
    let Some((sid, ctl)) = shared.control(sid) else {
        anyhow::bail!("找不到 session {sid}");
    };
    shared.wake_if_sleeping(&sid, &ctl);
    let (is_pty, was_idle) = {
        let inner = lock(&shared.inner);
        let sess = inner.state.sessions.get(&sid);
        (
            sess.is_some_and(|s| s.transport == "pty"),
            sess.is_some_and(|s| s.status == SessionStatus::Idle),
        )
    };
    if !is_pty {
        shared.emit(Event::UserPrompt {
            session: sid.clone(),
            text: with_attachment_summary(text, attachments),
        });
        if was_idle {
            shared.emit(Event::SessionStatus {
                session: sid.clone(),
                status: SessionStatus::Busy,
            });
        }
    }
    if let Err(e) = ctl.send_input_with(text, attachments) {
        if !is_pty && was_idle {
            shared.emit(Event::SessionStatus {
                session: sid,
                status: SessionStatus::Idle,
            });
        }
        return Err(e);
    }
    Ok(())
}

pub(crate) fn handle_prompt(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    text: String,
    attachments: Vec<termade_core::Attachment>,
    queue: bool,
) {
    let Some((sid, _)) = shared.control(&session) else {
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
        return;
    };
    // 超過成本上限（T17）：立即送出與排隊都拒絕，已在佇列裡的保留到調高上限。
    if let Some(s) = lock(&shared.inner)
        .state
        .sessions
        .get(&sid)
        .filter(|s| s.over_budget)
    {
        let _ = reply.send(Frame::error(format!(
            "session {} 已超過成本上限（over_budget）；請先用 `termade budget {} <金額>` 或 TUI `:budget <金額>` 調高上限",
            s.label, s.label
        )));
        return;
    }
    let mut clean_attachments = attachments;
    let raw_text = clean_attachments
        .iter()
        .find_map(|a| a.raw_prompt.clone())
        .unwrap_or_else(|| text.clone());
    for a in &mut clean_attachments {
        a.raw_prompt = None;
    }

    if queue {
        let (send_now, enqueue_res) = {
            let mut inner = lock(&shared.inner);
            let is_idle = inner
                .state
                .sessions
                .get(&sid)
                .is_some_and(|s| s.status == SessionStatus::Idle);
            let q_empty = inner.queues.get(&sid).is_none_or(|q| q.is_empty());
            if is_idle && q_empty {
                (true, Ok(Vec::new()))
            } else {
                let q = inner.queues.entry(sid.clone()).or_default();
                if q.len() >= MAX_QUEUE_LEN {
                    (false, Err(format!("排隊已滿（上限 {MAX_QUEUE_LEN} 則）")))
                } else {
                    q.push_back(QueuedPrompt {
                        text: text.clone(),
                        attachments: clean_attachments.clone(),
                        raw_text,
                    });
                    let items: Vec<String> = q.iter().map(|p| p.raw_text.clone()).collect();
                    (false, Ok(items))
                }
            }
        };
        if !send_now {
            match enqueue_res {
                Ok(items) => {
                    let n = items.len();
                    shared.emit(Event::QueueChanged {
                        session: sid,
                        items,
                    });
                    let _ = reply.send(Frame::ok_note(format!(
                        "已排隊（共 {n} 則，閒下來自動送出）"
                    )));
                }
                Err(msg) => {
                    let _ = reply.send(Frame::error(msg));
                }
            }
            return;
        }
    }

    match send_prompt(shared, &sid, &text, &clean_attachments) {
        Ok(()) => {
            let _ = reply.send(Frame::ok());
        }
        Err(e) => {
            let _ = reply.send(Frame::error(format!("送 prompt 失敗：{e}")));
        }
    }
}

pub(crate) fn handle_queue_clear(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    let sid = shared.resolve_session(&session);
    let (exists, count, had_state) = {
        let mut inner = lock(&shared.inner);
        let exists = inner.state.sessions.contains_key(&sid);
        let count = inner.queues.remove(&sid).map_or(0, |q| q.len());
        let had_state = inner.state.queues.contains_key(&sid);
        (exists, count, had_state)
    };
    if !exists {
        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        return;
    }
    if count > 0 || had_state {
        shared.emit(Event::QueueChanged {
            session: sid,
            items: Vec::new(),
        });
    }
    let _ = reply.send(Frame::ok_note(format!("已清空 {count} 則排隊 prompt")));
}

pub(crate) fn handle_queue_pop(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    let sid = shared.resolve_session(&session);
    let (exists, popped) = {
        let mut inner = lock(&shared.inner);
        if !inner.state.sessions.contains_key(&sid) {
            (false, None)
        } else {
            let item = inner.queues.get_mut(&sid).and_then(|q| q.pop_back());
            let items: Vec<String> = inner
                .queues
                .get(&sid)
                .map(|q| q.iter().map(|p| p.raw_text.clone()).collect())
                .unwrap_or_default();
            if items.is_empty() {
                inner.queues.remove(&sid);
            }
            (true, item.map(|p| (p.raw_text, items)))
        }
    };
    if !exists {
        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        return;
    }
    match popped {
        Some((raw_text, items)) => {
            shared.emit(Event::QueueChanged {
                session: sid,
                items,
            });
            let _ = reply.send(Frame::ok_note(raw_text));
        }
        None => {
            let _ = reply.send(Frame::ok());
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
        shared.clear_queue_for_ended(&sid, false);
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
        let after = inner.inbox_cursors.get(&sid).cloned();
        let unread = inner.state.inbox_for(&sid, after.as_deref());
        crate::handoff::build_handoff_brief(
            &inner.state,
            &src,
            &git_summary,
            note.as_deref(),
            &unread,
        )
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
        session: sid.clone(),
        budget_usd: Some(budget_usd),
    });
    // 超限期間保留的排隊 prompt（T19）在這裡恢復送出。
    // 佇列沒有送出東西時，補提醒超限期間累積的未讀訊息（T25）。
    if !shared.flush_queue_if_idle(&sid) {
        shared.nudge_unread_if_idle(&sid);
    }
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
    // 共用 worktree（handoff）：還有其他 session 參照時只移除紀錄，worktree 與分支留給最後一個。
    let sharers = if s.branch.is_some() {
        shared.cwd_shared_with(&s)
    } else {
        Vec::new()
    };
    if !sharers.is_empty() {
        shared.emit(Event::SessionRemoved {
            session: sid.clone(),
        });
        shared.clear_queue_for_ended(&sid, true);
        let _ = reply.send(Frame::ok_note(format!(
            "已移除 {}（worktree 仍由 {} 參照，保留）",
            s.label,
            sharers.join("、")
        )));
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
    shared.emit(Event::SessionRemoved {
        session: sid.clone(),
    });
    lock(&shared.inner).pending_nudge.remove(&sid);
    shared.clear_queue_for_ended(&sid, true);
    let _ = reply.send(Frame::ok_note(format!("已移除 {}", s.label)));
}

pub(crate) fn handle_kill(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    match shared.control(&session) {
        Some((sid, ctl)) => {
            let _ = ctl.kill();
            // 立刻拿掉控制把手：處理程序的結束事件要過一會才到，這段期間送來的 prompt
            // 不該再寫進已經被殺掉的處理程序（會把剛標成結束的 session 又變回 busy，擋住接續）。
            lock(&shared.inner).sessions.remove(&sid);
            shared.emit(Event::SessionExited {
                session: sid.clone(),
                code: None,
            });
            shared.clear_queue_for_ended(&sid, false);
            let _ = reply.send(Frame::ok());
        }
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

pub(crate) fn handle_config(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    model: Option<String>,
    effort: Option<String>,
) {
    let model = model.filter(|m| !m.trim().is_empty());
    let effort = effort.filter(|e| !e.trim().is_empty());
    if model.is_none() && effort.is_none() {
        let _ = reply.send(Frame::error("請指定 model 或 effort"));
        return;
    }
    let Some((sid, ctl)) = shared.control(&session) else {
        let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        return;
    };
    match ctl.set_config(model.as_deref(), effort.as_deref()) {
        Ok(applied) => {
            if let Some(m) = applied.or(model.clone()) {
                shared.emit(Event::SessionModel {
                    session: sid.clone(),
                    model: m,
                });
            }
            if let Some(e) = &effort {
                shared.emit(Event::SessionEffort {
                    session: sid.clone(),
                    effort: e.clone(),
                });
            }
            let mut parts = Vec::new();
            if let Some(m) = &model {
                parts.push(format!("model={m}"));
            }
            if let Some(e) = &effort {
                parts.push(format!("effort={e}"));
            }
            let _ = reply.send(Frame::ok_note(format!(
                "{sid} 已切換 {}（下一個回合起生效）",
                parts.join(" ")
            )));
        }
        Err(e) => {
            let _ = reply.send(Frame::error(format!("切換失敗：{e}")));
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
                shared.flush_queue_if_idle(&sid);
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
    // MCP 核准請求：把決定交給等待中的 `handle_approval_ask`。
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
                let _ = reply.send(Frame::error(format!("核准回覆失敗：{e}")));
            }
        },
        None => {
            let _ = reply.send(Frame::error(format!("找不到 session {session}")));
        }
    }
}

/// MCP `termade_approve`：先套用規則與政策，沒有結論就發核准請求並等人回覆（逾時視為拒絕）。
pub(crate) fn handle_approval_ask(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    tool: String,
    input: String,
) {
    let sid = shared.resolve_session(&session);
    let d = ask_user(shared, &sid, tool, input, true);
    let word = match d {
        Decision::Deny => "deny",
        _ => "allow",
    };
    let _ = reply.send(Frame::ok_note(word));
}

/// 向使用者請求核准並阻塞等待（最多 5 分鐘，逾時視為拒絕）。
/// `use_policy` 為真時先讓政策引擎自動決定（agent 的工具核准用）；
/// 對 spawn 這類「一定要問人」的確認傳 `false`。
pub(crate) fn ask_user(
    shared: &Arc<Shared>,
    sid: &str,
    title: String,
    detail: String,
    use_policy: bool,
) -> Decision {
    const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let sid = sid.to_string();
    let p = termade_core::PermissionRequest {
        req: termade_core::new_id("perm"),
        session: sid.clone(),
        title,
        detail,
    };
    if use_policy {
        if let Some(d) = shared.auto_decision(&sid, &p) {
            shared.emit(Event::PermissionResolved {
                session: sid,
                req: p.req,
                decision: d,
            });
            return d;
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    lock(&shared.inner)
        .approval_waiters
        .insert(p.req.clone(), tx);
    let req = p.req.clone();
    shared.emit(Event::PermissionRequested { request: p });
    match rx.recv_timeout(APPROVAL_TIMEOUT) {
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
    }
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
