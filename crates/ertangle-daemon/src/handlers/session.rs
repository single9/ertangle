//! Session 生命週期與控制請求處理 (Spawn、Prompt、Kill、Sleep、Wake、Permission 等)。

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use anyhow::bail;

use ertangle_adapter::{spawn as spawn_adapter, AgentEvent, SessionSpec};
use ertangle_core::{
    now_millis, Artifact, BusMessage, Cost, Decision, Event, MsgKind, Role, Session, SessionStatus,
    Target, Task, TaskStatus,
};
use ertangle_protocol::{Frame, ParentChange, SpawnRequest};
use tr::tr;

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
    // 交接：沿用來源 session 的工作目錄、分支與 repo (不建立新 worktree)。
    if let Some(src) = &spawn.inherit_from {
        let sid = shared.resolve_session(src);
        if let Some(s) = lock(&shared.inner).state.sessions.get(&sid) {
            cwd = PathBuf::from(&s.cwd);
            branch = s.branch.clone();
            repo_root = s.repo_root.clone();
        }
    }

    let parent = spawn.parent.as_ref().map(|p| shared.resolve_session(p));

    let global_dir = config_dir();
    let cfg_dir = if cwd.join(".ertangle").join("config.toml").is_file() {
        cwd.clone()
    } else if shared
        .default_cwd()
        .join(".ertangle")
        .join("config.toml")
        .is_file()
    {
        shared.default_cwd()
    } else {
        cwd.clone()
    };
    let cfg = config::Config::load(&global_dir, &cfg_dir);
    let role = cfg.resolve_role(spawn.role.as_ref(), parent.is_some());
    let label = spawn
        .label
        .clone()
        .unwrap_or_else(|| format!("{}/{}", role.id(), id.get(4..8).unwrap_or(&id)));
    // 沒指定 kind：依 roles.<role>.agent → 父 session 的 kind → defaults.agent → opencode 決定。
    let parent_kind = parent.as_ref().and_then(|pid| {
        lock(&shared.inner)
            .state
            .sessions
            .get(pid)
            .map(|s| s.kind.clone())
    });
    let mut kind = cfg.resolve_kind(spawn.kind.as_ref(), &role, parent_kind.as_ref());
    let mut resolved = cfg.resolve(
        &kind,
        &role,
        spawn.model.as_deref(),
        spawn.effort.as_deref(),
        &spawn.args,
        spawn.permission,
    );

    // agent 自己建立 session：先檢查 model／effort 是否超出免確認範圍 (避免自動開出高消耗的 session)。
    let mut spawn_note: Option<String> = None;
    let mut spawn = spawn;
    if parent.is_some() && spawn.inherit_from.is_none() {
        // 額度吃緊：換 fallback agent 或直接拒絕，避免工作做到一半被額度打斷。
        let quotas = lock(&shared.inner).state.quotas.clone();
        let warn = cfg
            .handoff
            .warn_percent
            .unwrap_or(crate::quota_watch::DEFAULT_WARN_PERCENT);
        match crate::quota_watch::check_spawn_quota(
            &kind,
            &quotas,
            &cfg.handoff.fallback,
            warn,
            now_millis(),
        ) {
            crate::quota_watch::SpawnQuota::Ok(n) => spawn_note = n,
            crate::quota_watch::SpawnQuota::Fallback(k, m, n) => {
                kind = k;
                if m.is_some() {
                    spawn.model = m;
                }
                spawn_note = Some(n);
                resolved = cfg.resolve(
                    &kind,
                    &role,
                    spawn.model.as_deref(),
                    spawn.effort.as_deref(),
                    &spawn.args,
                    spawn.permission,
                );
            }
            crate::quota_watch::SpawnQuota::Reject(e) => {
                let _ = reply.send(Frame::error(e));
                return;
            }
        }
    }
    if let Some(pid) = &parent {
        if spawn.inherit_from.is_none() {
            let pol = &cfg.agent_spawn;
            let check = pol.check(resolved.model.as_deref(), resolved.effort.as_deref());
            let exceeds = !matches!(check, config::SpawnCheck::Fine);
            let must_ask = pol.confirm() == "always" || (pol.confirm() == "costly" && exceeds);
            if must_ask {
                let reasons = match &check {
                    config::SpawnCheck::Exceeds { reasons, .. } => reasons.join("；"),
                    config::SpawnCheck::Fine => tr!("設定要求每次都先確認"),
                };
                let detail = tr!(
                    "agent {pid} 要建立 {kind} session：model={model} effort={effort}\n原因：{reasons}\n任務：{task}",
                    pid = pid,
                    kind = kind,
                    model = resolved.model.clone().unwrap_or_else(|| tr!("預設")),
                    effort = resolved.effort.clone().unwrap_or_else(|| tr!("預設")),
                    reasons = reasons,
                    task = spawn
                        .prompt
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect::<String>(),
                );
                let d = ask_user(shared, pid, tr!("建立子 agent"), detail, false);
                if d == Decision::Deny {
                    let _ = reply.send(Frame::error(tr!(
                        "使用者拒絕 (或逾時未回應)建立這個 session；請改用較便宜的 model／較低的 effort，或請使用者自己建立"
                    )));
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
                    let _ = reply.send(Frame::error(tr!(
                        "{reasons} (agent_spawn.confirm = never)",
                        reasons = reasons.join("；")
                    )));
                    return;
                }
                if effort_too_high {
                    let max = pol.max_effort().to_string();
                    let n = tr!("effort 已調降為 {max}", max = max);
                    spawn_note = Some(match spawn_note.take() {
                        Some(q) => format!("{q}; {n}"),
                        None => n,
                    });
                    resolved.effort = Some(max);
                }
            }
        }
    }

    // 有 parent 時檢查子 agent 層數、同時執行數與整棵樹成本上限。
    let mut parent_remaining_usd: Option<f64> = None;
    if let Some(pid) = &parent {
        let inner = lock(&shared.inner);
        if spawn.inherit_from.is_none() {
            let depth = if inner.state.sessions.contains_key(pid) {
                inner.state.session_depth(pid) + 1
            } else {
                1
            };
            if let Some(max_depth) = cfg.limits.max_depth {
                if depth > max_depth {
                    let _ = reply.send(Frame::error(tr!(
                        "超過子 agent 層數上限 max_depth：目前第 {depth} 層 (上限 {max_depth})",
                        depth = depth,
                        max_depth = max_depth
                    )));
                    return;
                }
            }
            {
                let max_children = cfg
                    .limits
                    .max_children
                    .unwrap_or_else(|| cfg.agent_spawn.max_live_children());
                let live_children = inner.state.live_children_count(pid);
                if live_children >= max_children {
                    let _ = reply.send(Frame::error(tr!(
                        "已達直接子 agent 數量上限 max_children：目前 {live_children} 個執行中 (上限 {max_children})",
                        live_children = live_children,
                        max_children = max_children
                    )));
                    return;
                }
            }
            if let Some(tree_usd) = cfg.limits.tree_usd {
                let tree_spent = inner.state.tree_cost_usd(pid);
                if tree_spent >= tree_usd {
                    let _ = reply.send(Frame::error(tr!(
                        "已達整棵樹成本上限 tree_usd：目前已花費 ${spent} (上限 ${limit})",
                        spent = format!("{tree_spent:.2}"),
                        limit = format!("{tree_usd:.2}")
                    )));
                    return;
                }
            }
        }
        if let Some(p_sess) = inner.state.sessions.get(pid) {
            if p_sess.over_budget {
                let _ = reply.send(Frame::error(tr!(
                    "父 session {label} 已達成本上限 session_usd (over_budget)，無法產生子 agent",
                    label = p_sess.label
                )));
                return;
            }
            if let Some(p_limit) = p_sess.budget_usd {
                let rem = (p_limit - p_sess.cost.usd).max(0.0);
                if rem <= 0.0 {
                    let _ = reply.send(Frame::error(tr!(
                        "父 session {label} 剩餘預算 session_usd 為 $0.00 (已花費 ${spent} / 上限 ${limit})，無法產生子 agent",
                        label = p_sess.label,
                        spent = format!("{:.2}", p_sess.cost.usd),
                        limit = format!("{p_limit:.2}")
                    )));
                    return;
                }
                parent_remaining_usd = Some(rem);
            }
        }
    }

    // agent 自己建立的 session 預設用 worktree (`[agent_spawn] worktree`)；不在 git repo 內就略過。
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
                let _ = reply.send(Frame::error(tr!("建立 worktree 失敗：{e}", e = e)));
                return;
            }
        }
    }

    // 子 agent 的權限不得高於父 session 與設定的上限 (防呆，不是安全邊界)。
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
        kind: kind.clone(),
        label,
        cwd: cwd.to_string_lossy().to_string(),
        branch,
        role,
        status: SessionStatus::Starting,
        model: resolved.model.clone(),
        cost: Default::default(),
        parent: parent.clone(),
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
        active_tools: Vec::new(),
        bg_procs: Vec::new(),
        tool_calls: 0,
        ended_ms: None,
    };
    shared.emit(Event::SessionStarted {
        session: session.clone(),
    });
    let launch = Launch {
        id: id.clone(),
        kind: kind.clone(),
        cwd,
        transport: spawn.transport,
        resolved,
        permission,
        prompt: spawn.prompt.clone(),
        // 交接簡報是 ertangle 組的；agent 建立的 session 初始 prompt 來自父 session；其餘是使用者。
        prompt_from: if spawn.inherit_from.is_some() {
            Some(ertangle_core::PROMPT_FROM_ERTANGLE.to_string())
        } else {
            parent.clone()
        },
        attachments: spawn.attachments.clone(),
        resume: None,
        cost_base: Default::default(),
    };
    match launch_session(shared, launch) {
        Ok(()) => {
            // 交接：把舊 session 的匯流排身分移交給新 session (之後送給舊 session 的訊息改投這裡)。
            if let Some(src) = &spawn.inherit_from {
                let from = shared.resolve_session(src);
                shared.emit(Event::SessionHandedOff {
                    from: from.clone(),
                    to: id.clone(),
                });
                // 舊 session 還沒提醒的未讀訊息已列在交接簡報裡，不再另外提醒接手者 (T25)。
                lock(&shared.inner).pending_nudge.remove(&from);
            }
            let mut note = tr!("已建立 session {id}", id = id);
            if let Some((from, to)) = downgraded {
                note.push_str(&tr!(
                    "(權限由 {from} 調降為 {to}：不得高於父 session／設定上限)",
                    from = from.id(),
                    to = to.id()
                ));
            }
            if let Some(n) = &spawn_note {
                note.push_str(&tr!("({n})", n = n));
            }
            if forced_worktree && !spawn.worktree {
                note.push_str(&tr!("(依 agent_spawn.worktree 使用獨立 worktree)"));
            }
            if spawn.inherit_from.is_some() {
                note.push_str(&tr!(" (權限：{perm})", perm = permission.id()));
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
            let _ = reply.send(Frame::error(tr!("啟動 agent 失敗：{e}", e = e)));
        }
    }
}

/// 啟動 (或接續)一個 agent 處理程序所需的資訊；`Session` 本身必須已經在 state 裡。
struct Launch {
    id: String,
    kind: ertangle_core::AgentKind,
    cwd: PathBuf,
    transport: ertangle_protocol::Transport,
    resolved: config::Resolved,
    permission: ertangle_core::PermissionPolicy,
    prompt: Option<String>,
    /// 初始 prompt 的來源 (`UserPrompt.from`)：`None` = 使用者。
    prompt_from: Option<String>,
    attachments: Vec<ertangle_core::Attachment>,
    /// 接續用的 agent 原生對話 id；`None` 表示開新對話。
    resume: Option<String>,
    /// 接續前已累計的成本：agent 的成本是處理程序內累計時，新處理程序回報的數字要加上這個基底。
    cost_base: Cost,
}

/// spawn 與 resume 共用：組 `SessionSpec` → 啟動 adapter → 登記 `SessionEntry` → 起 ingest 執行緒。
/// 成功時送出 `Idle` (以及初始 prompt 的回顯)；失敗時不動 state，由呼叫端決定怎麼收尾。
fn launch_session(shared: &Arc<Shared>, launch: Launch) -> anyhow::Result<()> {
    let Launch {
        id,
        kind,
        cwd,
        transport,
        resolved,
        permission,
        prompt,
        prompt_from,
        attachments,
        resume,
        cost_base,
    } = launch;
    // 協作說明 (T20)：spawn 與 resume 都重新組，接續時 agent 的系統提示不會保留上一次的。
    let system_append = {
        let inner = lock(&shared.inner);
        crate::collab::for_session(&inner.state, &id, &config_dir(), &cwd)
    };
    let mcp_exe = std::env::current_exe().ok();
    let mut env = vec![
        ("ERTANGLE_SESSION".to_string(), id.clone()),
        (
            "ERTANGLE_SOCKET".to_string(),
            socket_path().to_string_lossy().to_string(),
        ),
    ];
    if let Some(exe) = &mcp_exe {
        env.push((
            "ERTANGLE_BIN".to_string(),
            exe.to_string_lossy().to_string(),
        ));
        if let Some(bin_dir) = exe.parent() {
            let current_path = std::env::var("PATH").unwrap_or_default();
            env.push((
                "PATH".to_string(),
                format!("{}:{current_path}", bin_dir.to_string_lossy()),
            ));
        }
    }

    // worktree 裡的 agent 共用一個 cargo target：不必各編一份，cargo 的目錄鎖也讓多個 worker 的
    // 編譯／測試自動排隊，不會同時吃滿 CPU。放在 resolved.env 之前，設定檔可以覆寫。
    if config::Config::load(&config_dir(), &cwd)
        .agent_spawn
        .shared_cargo_target()
        && std::env::var_os("CARGO_TARGET_DIR").is_none()
    {
        let target = lock(&shared.inner)
            .state
            .sessions
            .get(&id)
            .and_then(shared_cargo_target_dir);
        if let Some(t) = target {
            env.push(("CARGO_TARGET_DIR".to_string(), t));
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
        // 先登記再送 Idle：舊處理程序遲到的事件從這一刻起就會被擋掉 (見下方的 ingest 執行緒)。
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
                from: prompt_from.clone(),
            });
        }
    }

    let shared_worker = shared.clone();
    std::thread::spawn(move || {
        for mut ev in rx {
            // 這個 session 已被接續成另一個處理程序：舊處理程序遲到的事件 (尤其是 Exited)不能再套用，
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

/// 接續：讓已結束或已封存的 session 用 agent 原生的對話 id 接回原對話，沿用同一個 ertangle session id。
pub(crate) fn resume_session(
    shared: &Arc<Shared>,
    sid_or_label: &str,
    prompt: Option<String>,
    prompt_from: Option<String>,
) -> anyhow::Result<String> {
    let Some((mut s, is_archived)) = lock(&shared.inner)
        .state
        .resolve_resumable_session(sid_or_label)
    else {
        bail!(
            "{}",
            tr!("找不到 session {session}", session = sid_or_label)
        );
    };
    let sid = s.id.clone();
    if s.status.is_live() && !is_archived {
        bail!("{}", tr!("{label} 仍在執行，不需要接續", label = s.label));
    }
    let Some(native_id) = s.native_id.clone() else {
        bail!(
            "{}",
            tr!(
                "{label} 沒有可接續的對話 id (PTY、舊版 log，或 agent 還沒回報就結束了)；請改用交接 (handoff)",
                label = s.label
            )
        );
    };
    let mut cwd = PathBuf::from(&s.cwd);
    if !cwd.is_dir() {
        if let Some(root) = s
            .repo_root
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
        {
            cwd = root;
            s.cwd = cwd.to_string_lossy().to_string();
            s.branch = None;
        } else {
            bail!(
                "{}",
                tr!(
                    "{label} 的工作目錄 {cwd} 已不存在，無法接續",
                    label = s.label,
                    cwd = s.cwd
                )
            );
        }
    }
    // 交接後新舊 session 共用同一個 worktree；不要讓兩個 agent 同時改同一個目錄。
    if let Some(user) = s
        .branch
        .as_ref()
        .and_then(|_| shared.cwd_in_use_by_other(&s))
    {
        bail!(
            "{}",
            tr!(
                "worktree 正由 {user} 使用，不能接續 {label}",
                user = user,
                label = s.label
            )
        );
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
    // 只有 Claude 的成本是處理程序內累計 (新處理程序從 0 算)；Codex 回報的是 thread 累計的 token。
    let cost_base = if matches!(s.kind, ertangle_core::AgentKind::ClaudeCode) {
        s.cost
    } else {
        Default::default()
    };
    if is_archived {
        s.status = SessionStatus::Starting;
        shared.emit(Event::SessionStarted { session: s.clone() });
    }
    let launch = Launch {
        id: sid.clone(),
        kind: s.kind.clone(),
        cwd,
        transport: ertangle_protocol::Transport::Structured,
        resolved,
        permission: s.permission,
        prompt: prompt.filter(|p| !p.trim().is_empty()),
        prompt_from,
        attachments: vec![],
        resume: Some(native_id),
        cost_base,
    };
    if let Err(e) = launch_session(shared, launch) {
        if is_archived {
            shared.emit(Event::SessionRemoved { session: sid });
        }
        return Err(e);
    }
    // 清掉「可用 :resume 接續」之類的舊說明。
    if s.status_note.is_some() {
        shared.emit(Event::SessionNote {
            session: sid,
            note: String::new(),
        });
    }
    Ok(s.label)
}

pub(crate) fn handle_resume(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    prompt: Option<String>,
) {
    match resume_session(shared, &session, prompt, None) {
        Ok(label) => {
            let _ = reply.send(Frame::ok_note(tr!("已接續 {label}", label = label)));
        }
        Err(e) => {
            let _ = reply.send(Frame::error(tr!(
                "接續 {session} 失敗：{e}",
                session = session,
                e = e
            )));
        }
    }
}

/// 實際送出一則 prompt 給目標 session (由 `handle_prompt` 與佇列自動送出共用)。
pub(crate) fn send_prompt(
    shared: &Arc<Shared>,
    sid: &str,
    text: &str,
    attachments: &[ertangle_core::Attachment],
    from: Option<&str>,
) -> anyhow::Result<()> {
    let Some((sid, ctl)) = shared.control(sid) else {
        anyhow::bail!("{}", tr!("找不到 session {session}", session = sid));
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
            from: from.map(str::to_string),
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
    attachments: Vec<ertangle_core::Attachment>,
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
                tr!(
                    "{session} 已結束；可用 :resume (或 `ertangle resume`)接續原對話",
                    session = session
                )
            }
            Some(false) => tr!("{session} 已結束，且沒有可接續的對話 id", session = session),
            None => tr!("找不到 session {session}", session = session),
        };
        let _ = reply.send(Frame::error(msg));
        return;
    };
    // 超過成本上限 (T17)：立即送出與排隊都拒絕，已在佇列裡的保留到調高上限。
    if let Some(s) = lock(&shared.inner)
        .state
        .sessions
        .get(&sid)
        .filter(|s| s.over_budget)
    {
        let _ = reply.send(Frame::error(tr!(
            "session {label} 已超過成本上限 (over_budget)；請先用 `ertangle budget {label} <金額>` 或 TUI `:budget <金額>` 調高上限",
            label = s.label
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
                    (
                        false,
                        Err(tr!("排隊已滿 (上限 {max} 則)", max = MAX_QUEUE_LEN)),
                    )
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
                    let _ = reply.send(Frame::ok_note(tr!(
                        "已排隊 (共 {n} 則，閒下來自動送出)",
                        n = n
                    )));
                }
                Err(msg) => {
                    let _ = reply.send(Frame::error(msg));
                }
            }
            return;
        }
    }

    match send_prompt(shared, &sid, &text, &clean_attachments, None) {
        Ok(()) => {
            let _ = reply.send(Frame::ok());
        }
        Err(e) => {
            let _ = reply.send(Frame::error(tr!("送 prompt 失敗：{e}", e = e)));
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
        let _ = reply.send(Frame::error(tr!(
            "找不到 session {session}",
            session = session
        )));
        return;
    }
    if count > 0 || had_state {
        shared.emit(Event::QueueChanged {
            session: sid,
            items: Vec::new(),
        });
    }
    let _ = reply.send(Frame::ok_note(tr!(
        "已清空 {count} 則排隊 prompt",
        count = count
    )));
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
        let _ = reply.send(Frame::error(tr!(
            "找不到 session {session}",
            session = session
        )));
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
fn with_attachment_summary(text: &str, atts: &[ertangle_core::Attachment]) -> String {
    let mut out = text.to_string();
    for a in atts {
        out.push('\n');
        out.push_str(&a.summary());
    }
    out
}

/// 交接：停掉舊 session、組簡報、讓新 agent 沿用同一個工作目錄接手。
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_handoff(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    kind: ertangle_core::AgentKind,
    model: Option<String>,
    effort: Option<String>,
    note: Option<String>,
    role: Option<Role>,
    parent: Option<ParentChange>,
    force: bool,
) {
    let sid = shared.resolve_session(&session);
    let Some(src) = lock(&shared.inner).state.sessions.get(&sid).cloned() else {
        let _ = reply.send(Frame::error(tr!(
            "找不到 session {session}",
            session = session
        )));
        return;
    };

    let old_role = src.role.clone();
    let old_parent = src.parent.clone();
    let new_role = role.unwrap_or_else(|| old_role.clone());
    let new_parent = match parent {
        Some(ParentChange::None) => None,
        Some(ParentChange::Session(pid)) => Some(shared.resolve_session(&pid)),
        Some(ParentChange::Keep) => old_parent.clone(),
        None => {
            if new_role == Role::Coordinator {
                None
            } else {
                old_parent.clone()
            }
        }
    };
    let role_changed = new_role != old_role;

    // 檢查降級防護 (Decision 6)：coordinator 降級且仍有執行中子 session 時，未給 force 則拒絕
    let live_children: Vec<Session> = {
        let inner = lock(&shared.inner);
        inner
            .state
            .sessions
            .values()
            .filter(|s| s.parent.as_deref() == Some(&sid) && s.status.is_live())
            .cloned()
            .collect()
    };
    if old_role == Role::Coordinator
        && new_role != Role::Coordinator
        && !live_children.is_empty()
        && !force
    {
        let _ = reply.send(Frame::error(tr!(
            "無法將仍有執行中子 session 的 coordinator 降級；請先結束子 session 或加上 --force"
        )));
        return;
    }

    // 晉升 worker 為 coordinator (Decision 5)：
    // 指派給舊 session 的未完成任務退回 todo (無負責人)，並送 TASK_BLOCKED 給舊 parent
    if role_changed && new_role == Role::Coordinator {
        if let Some(parent_id) = &old_parent {
            let open_tasks: Vec<Task> = {
                let inner = lock(&shared.inner);
                inner
                    .state
                    .tasks
                    .values()
                    .filter(|t| t.assignee.as_deref() == Some(&sid) && t.status != TaskStatus::Done)
                    .cloned()
                    .collect()
            };
            for mut task in open_tasks {
                task.status = TaskStatus::Todo;
                task.assignee = None;
                task.updated_ms = now_millis();
                shared.emit(Event::TaskChanged { task: task.clone() });
                let body = format!("TASK_BLOCKED {} promoted to coordinator by user", task.id);
                let _ = shared.deliver_message(
                    Some(sid.clone()),
                    Target::Direct(parent_id.clone()),
                    MsgKind::Chat,
                    body,
                    None,
                );
            }
        }
    }

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
            git_note = format!("(自動 commit 失敗：{e})\n");
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
        let after = inner.state.inbox_cursors.get(&sid).cloned();
        let unread = inner.state.inbox_for(&sid, after.as_deref());
        crate::handoff::build_handoff_brief(
            &inner.state,
            &src,
            &git_summary,
            note.as_deref(),
            &unread,
            &new_role,
            new_parent.as_deref(),
            &live_children,
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
            attachments.push(ertangle_core::Attachment {
                name: "handoff.diff".into(),
                mime: "text/plain".into(),
                size: text.len() as u64,
                text: Some(text),
                ..Default::default()
            });
        }
    }

    if role_changed {
        shared.emit(Event::SessionNote {
            session: sid.clone(),
            note: tr!(
                "已交接給 {kind} (角色變更為 {role})",
                kind = kind.id(),
                role = new_role.id()
            ),
        });
    } else {
        shared.emit(Event::SessionNote {
            session: sid.clone(),
            note: tr!("已交接給 {kind}", kind = kind.id()),
        });
    }

    // 權限繼承 (Decision 4)：角色變更時不繼承 (包含手動 bypass)，交由 handle_spawn 依新角色重新解析；
    // 角色不變的交接 (含額度自動交接) 維持原本的權限。
    let permission = if role_changed {
        None
    } else {
        Some(src.permission)
    };

    handle_spawn(
        shared,
        reply,
        SpawnRequest {
            kind: Some(kind.clone()),
            cwd: Some(src.cwd.clone()),
            label: Some(format!("{}→{}", src.label, kind.id())),
            role: Some(new_role),
            prompt: Some(brief),
            worktree: false,
            transport: Default::default(),
            permission,
            parent: new_parent,
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
        let _ = reply.send(Frame::error(tr!("預算上限必須為大於 0 的數字")));
        return;
    }
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    let Some(s) = sess else {
        let _ = reply.send(Frame::error(tr!(
            "找不到 session {session}",
            session = session
        )));
        return;
    };
    if budget_usd <= s.cost.usd {
        let _ = reply.send(Frame::error(tr!(
            "新的上限 (${budget})必須高於目前已花費金額 (${spent})",
            budget = format!("{budget_usd:.2}"),
            spent = format!("{:.2}", s.cost.usd)
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
    // 超限期間保留的排隊 prompt (T19)在這裡恢復送出。
    // 佇列沒有送出東西時，補提醒超限期間累積的未讀訊息 (T25)。
    if !shared.flush_queue_if_idle(&sid) {
        shared.nudge_unread_if_idle(&sid);
    }
    let _ = reply.send(Frame::ok_note(tr!(
        "已將 {label} 的預算上限設為 ${budget}",
        label = s.label,
        budget = format!("{budget_usd:.2}")
    )));
}

pub(crate) fn handle_cancel(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    match shared.control(&session) {
        Some((_, ctl)) => {
            let _ = ctl.cancel();
            let _ = reply.send(Frame::ok());
        }
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
        }
    }
}

pub(crate) fn handle_remove(shared: &Arc<Shared>, reply: &ClientTx, session: String, force: bool) {
    let sid = shared.resolve_session(&session);
    let sess = lock(&shared.inner).state.sessions.get(&sid).cloned();
    let Some(s) = sess else {
        let _ = reply.send(Frame::error(tr!(
            "找不到 session {session}",
            session = session
        )));
        return;
    };
    if s.status.is_live() {
        let _ = reply.send(Frame::error(tr!("session 仍在執行，請先結束 (kill)")));
        return;
    }
    // 共用 worktree 檢查只對有 branch 的 session 有意義：一般 session 共用主 repo 的
    // cwd 是正常情況，移除紀錄不會動到任何檔案，不該因此被另一個執行中的 session 擋下。
    if s.branch.is_some() {
        if let Some(user) = shared.cwd_in_use_by_other(&s) {
            let _ = reply.send(Frame::error(tr!(
                "worktree 仍被 {user} 使用，不能移除",
                user = user
            )));
            return;
        }
    }
    // 共用 worktree (handoff)：還有其他 session 參照時只移除紀錄，worktree 與分支留給最後一個。
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
        let _ = reply.send(Frame::ok_note(tr!(
            "已移除 {label} (worktree 仍由 {sharers} 參照，保留)",
            label = s.label,
            sharers = sharers.join("、")
        )));
        return;
    }
    if s.branch.is_some() {
        let repo = crate::git::session_repo(&shared.default_cwd(), &s);
        if !force {
            if let Some(reason) = crate::git::gc_block_reason(&repo, &s) {
                let _ = reply.send(Frame::error(tr!(
                    "{label} ({reason})：請先 merge，或用 force 強制移除",
                    label = s.label,
                    reason = reason
                )));
                return;
            }
        }
        if let Err(e) = crate::git::remove_worktree(&repo, &s, force) {
            let _ = reply.send(Frame::error(tr!("移除 worktree 失敗：{e}", e = e)));
            return;
        }
    }
    shared.emit(Event::SessionRemoved {
        session: sid.clone(),
    });
    lock(&shared.inner).pending_nudge.remove(&sid);
    shared.clear_queue_for_ended(&sid, true);
    let _ = reply.send(Frame::ok_note(tr!("已移除 {label}", label = s.label)));
}

/// Kills a live session's agent process and marks it exited. Returns the resolved id, or
/// `None` when no live control handle exists for `session`.
pub(crate) fn stop_session(
    shared: &Arc<Shared>,
    session: &str,
) -> Option<ertangle_core::SessionId> {
    let (sid, ctl) = shared.control(session)?;
    let _ = ctl.kill();
    // 立刻拿掉控制把手：處理程序的結束事件要過一會才到，這段期間送來的 prompt
    // 不該再寫進已經被殺掉的處理程序 (會把剛標成結束的 session 又變回 busy，擋住接續)。
    lock(&shared.inner).sessions.remove(&sid);
    shared.emit(Event::SessionExited {
        session: sid.clone(),
        code: None,
    });
    shared.clear_queue_for_ended(&sid, false);
    Some(sid)
}

pub(crate) fn handle_kill(shared: &Arc<Shared>, reply: &ClientTx, session: String) {
    match stop_session(shared, &session) {
        Some(_) => {
            let _ = reply.send(Frame::ok());
        }
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
        }
    }
}

pub(crate) fn handle_kill_bg_proc(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    pid: Option<u32>,
    force: bool,
) {
    let Some((sid, ctl)) = shared.control(&session) else {
        let _ = reply.send(Frame::error(tr!(
            "找不到 session {session}",
            session = session
        )));
        return;
    };

    let Some(root_pid) = ctl.pid() else {
        let _ = reply.send(Frame::error(tr!("無法取得 session 主處理程序 PID")));
        return;
    };

    let sig = if force { "-KILL" } else { "-TERM" };

    if let Some(target_pid) = pid {
        // 只允許終止該 session 工具指令啟動的處理程序；MCP 伺服器等基礎設施不在此列。
        if !crate::procs::is_killable(root_pid, target_pid) {
            let _ = reply.send(Frame::error(tr!(
                "PID {pid} 不屬於 session {sid} 的後代處理程序",
                pid = target_pid,
                sid = sid
            )));
            return;
        }

        if let Err(e) = ertangle_adapter::signal_pid(target_pid, sig) {
            let _ = reply.send(Frame::error(tr!(
                "終止 PID {pid} 失敗：{e}",
                pid = target_pid,
                e = e
            )));
            return;
        }
    } else {
        // pid: None 代表清理該 Session 工具指令啟動的所有處理程序 (不含 MCP 伺服器等基礎設施)
        for dpid in crate::procs::killable_pids(root_pid) {
            let _ = ertangle_adapter::signal_pid(dpid, sig);
        }
    }

    // 重新探測並更新事件
    shared.scan_bg_procs_for_session(&sid);
    let _ = reply.send(Frame::ok());
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
        let _ = reply.send(Frame::error(tr!("請指定 model 或 effort")));
        return;
    }
    let Some((sid, ctl)) = shared.control(&session) else {
        let _ = reply.send(Frame::error(tr!(
            "找不到 session {session}",
            session = session
        )));
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
            let _ = reply.send(Frame::ok_note(tr!(
                "{sid} 已切換 {parts} (下一個回合起生效)",
                sid = sid,
                parts = parts.join(" ")
            )));
        }
        Err(e) => {
            let _ = reply.send(Frame::error(tr!("切換失敗：{e}", e = e)));
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
                let _ = reply.send(Frame::ok_note(tr!("已休眠 {sid}", sid = sid)));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(tr!("休眠失敗：{e}", e = e)));
            }
        },
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
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
                let _ = reply.send(Frame::ok_note(tr!("已喚醒 {sid}", sid = sid)));
            }
            Err(e) => {
                let _ = reply.send(Frame::error(tr!("喚醒失敗：{e}", e = e)));
            }
        },
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
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
                let _ = reply.send(Frame::error(tr!("核准回覆失敗：{e}", e = e)));
            }
        },
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
        }
    }
}

/// MCP `ertangle_approve`：先套用規則與政策，沒有結論就發核准請求並等人回覆 (逾時視為拒絕)。
pub(crate) fn handle_mcp_permission(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    title: String,
    detail: String,
) {
    let sid = shared.resolve_session(&session);
    let decision = ask_user(shared, &sid, title, detail, true);
    let _ = reply.send(Frame::PermissionDecision { decision });
}

/// 相容舊版 MCP 核准呼叫 (回覆 Ok 帶字串)。
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

/// 向使用者請求核准並阻塞等待 (最多 5 分鐘，逾時視為拒絕)。
/// `use_policy` 為真時先讓政策引擎自動決定 (agent 的工具核准用)；
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
    let p = ertangle_core::PermissionRequest {
        req: ertangle_core::new_id("perm"),
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

/// Ask the user an interactive question and block until answered (5-minute timeout, treated as abandoned).
pub(crate) fn ask_user_question(
    shared: &Arc<Shared>,
    sid: &str,
    question: ertangle_core::UserQuestion,
) -> Vec<String> {
    const QUESTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let (tx, rx) = std::sync::mpsc::channel();
    let q_id = question.id.clone();
    let sid_str = sid.to_string();
    lock(&shared.inner)
        .question_waiters
        .insert(q_id.clone(), tx);
    shared.emit(Event::QuestionRequested { question });
    match rx.recv_timeout(QUESTION_TIMEOUT) {
        Ok(answers) => answers,
        Err(_) => {
            lock(&shared.inner).question_waiters.remove(&q_id);
            shared.emit(Event::QuestionResolved {
                session: sid_str,
                id: q_id,
                answers: Vec::new(),
            });
            Vec::new()
        }
    }
}

/// MCP `ertangle_question`: ask a question and wait for the human answer.
pub(crate) fn handle_mcp_question(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    question: String,
    header: Option<String>,
    options: Vec<String>,
    multi_select: bool,
) {
    let sid = shared.resolve_session(&session);
    let q_options = options
        .into_iter()
        .map(ertangle_core::QuestionOption::plain)
        .collect();
    let q = ertangle_core::UserQuestion {
        id: ertangle_core::new_id("qst"),
        session: sid.clone(),
        question,
        header,
        options: q_options,
        multi_select,
        custom: true,
        group: None,
    };
    let answers = ask_user_question(shared, &sid, q);
    let _ = reply.send(Frame::QuestionAnswer { answers });
}

/// Answer an interactive question from a human.
///
/// Grouped questions are resolved only by their own answers: clients send `[]` for
/// siblings they skip, so no sibling is auto-cleared here.
pub(crate) fn handle_answer_question(
    shared: &Arc<Shared>,
    reply: &ClientTx,
    session: String,
    id: String,
    answers: Vec<String>,
) {
    let sid = shared.resolve_session(&session);
    let waiter = lock(&shared.inner).question_waiters.remove(&id);
    if let Some(tx) = waiter {
        let _ = tx.send(answers.clone());
        shared.emit(Event::QuestionResolved {
            session: sid,
            id,
            answers,
        });
        let _ = reply.send(Frame::ok());
        return;
    }
    match shared.control(&session) {
        Some((real_sid, ctl)) => match ctl.resolve_question(&id, &answers) {
            Ok(()) => {
                shared.emit(Event::QuestionResolved {
                    session: real_sid,
                    id,
                    answers,
                });
                let _ = reply.send(Frame::ok());
            }
            Err(e) => {
                let _ = reply.send(Frame::error(tr!("問題回覆失敗：{e}", e = e)));
            }
        },
        None => {
            let _ = reply.send(Frame::error(tr!(
                "找不到 session {session}",
                session = session
            )));
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
    kind: ertangle_core::ArtifactKind,
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
    let _ = reply.send(Frame::ok_note(tr!("已分享產出 {id}", id = id)));
}

/// worktree session 共用的 cargo target 目錄 (`<repo>/.ertangle/target`)；不在 worktree 或不是 Cargo 專案時為 `None`。
pub(crate) fn shared_cargo_target_dir(s: &ertangle_core::Session) -> Option<String> {
    s.branch.as_ref()?;
    let root = std::path::Path::new(s.repo_root.as_deref()?);
    if root == std::path::Path::new(&s.cwd) || !root.join("Cargo.toml").is_file() {
        return None;
    }
    Some(
        root.join(".ertangle")
            .join("target")
            .to_string_lossy()
            .to_string(),
    )
}
