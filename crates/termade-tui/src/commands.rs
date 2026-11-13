//! TUI 命令面板、跳轉、啟動與審批回覆。

use anyhow::Result;
use termade_core::{AgentKind, Decision, MsgKind, PermissionPolicy, Role, Target};
use termade_daemon::ConnStream;
use termade_protocol::{Request, SpawnRequest, Transport};

use crate::app::{toggle_scope, toggle_thoughts, toggle_tools, App, CenterView, LayoutMode, Mode};
use crate::clipboard::copy_main_pane;
use crate::{request_selected_diff, send};

/// 在輸入框支援 `@session 訊息`、`#channel 訊息`、`/command` 或直接送 prompt 給當前 session。
pub(crate) fn submit_prompt_or_route(
    app: &mut App,
    writer: &mut ConnStream,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if let Some(cmd) = trimmed.strip_prefix('/') {
        return execute_command(app, writer, cmd);
    }
    // `@存在的檔案` 是附件，不是 `@peer` 訊息。
    if (trimmed.starts_with('@') && !crate::attach::leads_with_file(trimmed, &app.search_root()))
        || trimmed.starts_with('#')
    {
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let target_str = parts.next().unwrap_or("");
        let body = parts.next().unwrap_or("").trim();
        if !body.is_empty() {
            send(
                writer,
                &Request::BusSend {
                    from: None,
                    to: Target::parse(target_str),
                    kind: MsgKind::Chat,
                    body: body.to_string(),
                    reply_to: None,
                },
            )?;
            app.status = format!("已傳送訊息至 {target_str}");
            return Ok(());
        }
    }
    if let Some(id) = app.selected_id() {
        let (body, attachments) =
            match crate::attach::extract_attachments(trimmed, &app.search_root()) {
                Ok(r) => r,
                Err(e) => {
                    app.status = format!("附件錯誤：{e}");
                    return Ok(());
                }
            };
        let n = attachments.len();
        send(
            writer,
            &Request::Prompt {
                session: id,
                text: body,
                attachments,
            },
        )?;
        app.status = if n > 0 {
            format!("已送出 prompt（含 {n} 個附件）")
        } else {
            "已送出 prompt".into()
        };
    } else {
        app.status = "沒有選取的 session".into();
    }
    Ok(())
}

/// 把一則 prompt 排進選取 session 的佇列；session 已閒置就直接送。
/// 只處理一般 prompt（含 `@檔案` 附件）；`/命令`、`@peer`、`#channel` 不排隊。
pub(crate) fn queue_prompt(app: &mut App, writer: &mut ConnStream, text: &str) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let routed = trimmed.starts_with('/')
        || trimmed.starts_with('#')
        || (trimmed.starts_with('@')
            && !crate::attach::leads_with_file(trimmed, &app.search_root()));
    let Some(id) = app.selected_id() else {
        app.status = "沒有選取的 session".into();
        return Ok(());
    };
    if routed {
        return submit_prompt_or_route(app, writer, text);
    }
    let idle = app
        .state
        .sessions
        .get(&id)
        .is_some_and(|s| s.status == termade_core::SessionStatus::Idle);
    if idle && app.queues.len(&id) == 0 {
        return submit_prompt_or_route(app, writer, text);
    }
    match crate::attach::extract_attachments(trimmed, &app.search_root()) {
        Ok((body, attachments)) => {
            app.queues.push(
                &id,
                crate::history::Queued {
                    text: body,
                    attachments,
                },
            );
            app.status = format!("已排隊（共 {} 則，閒下來自動送出）", app.queues.len(&id));
        }
        Err(e) => app.status = format!("附件錯誤：{e}"),
    }
    Ok(())
}

/// 每輪事件迴圈呼叫：session 閒下來就送出它佇列裡的下一則。
pub(crate) fn flush_queues(app: &mut App, writer: &mut ConnStream) -> Result<()> {
    let (ready, dropped) = {
        let sessions = &app.state.sessions;
        app.queues.take_ready(
            |id| sessions.get(id).map(|s| s.status),
            std::time::Instant::now(),
        )
    };
    if dropped > 0 {
        app.status = format!("session 已結束，丟棄 {dropped} 則排隊 prompt");
    }
    for (id, q) in ready {
        send(
            writer,
            &Request::Prompt {
                session: id.clone(),
                text: q.text,
                attachments: q.attachments,
            },
        )?;
        let left = app.queues.len(&id);
        app.status = if left > 0 {
            format!("已送出排隊的 prompt（還有 {left} 則）")
        } else {
            "已送出排隊的 prompt".into()
        };
    }
    Ok(())
}

/// `:queue [文字|clear]`、`:unqueue`。
fn queue_command(app: &mut App, writer: &mut ConnStream, rest: &str) -> Result<()> {
    let Some(id) = app.selected_id() else {
        app.status = "沒有選取的 session".into();
        return Ok(());
    };
    match rest.trim() {
        "" => {
            let items = app.queues.texts(&id);
            app.status = if items.is_empty() {
                "排隊為空（:queue <文字> 排入，或在輸入框按 Alt-Enter）".into()
            } else {
                let list: Vec<String> = items
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("{}.{}", i + 1, t.chars().take(24).collect::<String>()))
                    .collect();
                format!("排隊 {} 則：{}", items.len(), list.join(" | "))
            };
            Ok(())
        }
        "clear" => {
            let n = app.queues.clear(&id);
            app.status = format!("已清空排隊（{n} 則）");
            Ok(())
        }
        text => queue_prompt(app, writer, text),
    }
}

/// `:command` 命令：`:queue`、`:unqueue`。
fn unqueue(app: &mut App) {
    let Some(id) = app.selected_id() else {
        return;
    };
    app.status = match app.queues.pop_last(&id) {
        Some(q) => {
            // 取回輸入框方便修改後重新排入。
            app.input_cursor = q.text.chars().count();
            app.input = q.text;
            app.mode = Mode::Prompt;
            "已取消最後一則排隊，內容放回輸入框".into()
        }
        None => "沒有排隊中的 prompt".into(),
    };
}

/// `:` 命令列可用的命令：(名稱, 用法與說明)，供提示與 Tab 補全。
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    (
        "spawn",
        "[agent[:model] prompt]  新增 agent；不帶參數開選擇器",
    ),
    ("new", "開選擇器：agent → model → prompt"),
    ("help", "說明面板：所有按鍵與命令"),
    ("quota", "顯示各 agent 帳號的額度與重置時間"),
    (
        "handoff",
        "[agent[:model]] [說明]  把選取 session 的工作交接給另一個 agent",
    ),
    ("kill", "結束選取的 session"),
    ("cancel", "中斷選取 session 的目前回合"),
    ("sleep", "休眠選取的 session"),
    ("wake", "喚醒選取的 session"),
    ("rm", "[session] [--force]  移除已結束的 session"),
    ("clean", "清除所有已結束（且沒有分支）的 session"),
    ("merge", "[session] [--dry-run]  合併 worktree 回主工作區"),
    ("diff", "顯示選取 session 的 diff"),
    ("gc", "[session]  清理已合併的 worktree"),
    ("worktrees", "顯示 worktree 清單"),
    ("overview", "回到 transcript 視圖"),
    ("layout", "[three|two|tabs]  切換版面"),
    ("raw", "原始文字／Markdown 切換"),
    ("thoughts", "顯示／隱藏思考"),
    ("tools", "展開／摺疊工具呼叫"),
    ("scope", "[here|all]  只看本專案／顯示全部專案的 session"),
    (
        "queue",
        "[文字|clear]  排隊 prompt，session 閒下來自動送；不帶參數列出",
    ),
    ("unqueue", "取消最後一則排隊並放回輸入框"),
    ("bus", "顯示／隱藏 Bus 浮層"),
    ("task", "<標題>  新增任務"),
    ("broadcast", "<訊息>  廣播給所有 session"),
    ("copy", "複製目前畫面內容"),
    ("quit", "離開（保留 daemon）"),
    ("shutdown", "關閉 daemon 並離開"),
];

/// 依目前輸入給出提示：還在打命令名稱時列出符合的命令；已打完命令名稱則顯示該命令的用法。
pub(crate) fn command_hints(input: &str) -> Vec<(&'static str, &'static str)> {
    let input = input.trim_start().trim_start_matches(['/', ':']);
    let verb = input.split_whitespace().next().unwrap_or("");
    if input.contains(char::is_whitespace) {
        return COMMANDS
            .iter()
            .copied()
            .filter(|(n, _)| *n == verb)
            .collect();
    }
    COMMANDS
        .iter()
        .copied()
        .filter(|(n, _)| n.starts_with(verb))
        .collect()
}

pub(crate) fn execute_command(app: &mut App, writer: &mut ConnStream, raw: &str) -> Result<()> {
    let raw = raw.trim().trim_start_matches('/');
    let mut parts = raw.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match verb.as_str() {
        "help" | "h" | "?" => crate::help::open_help(app),
        "quota" => {
            let now = termade_core::now_millis();
            app.status = if app.state.quotas.is_empty() {
                "尚無額度資料（agent 執行過一個回合後才會有）".into()
            } else {
                app.state
                    .quotas
                    .values()
                    .map(|q| q.summary(now))
                    .collect::<Vec<_>>()
                    .join("   |   ")
            };
        }
        "handoff" => {
            let Some(src) = app.selected_id() else {
                app.status = "沒有選取的 session".into();
                return Ok(());
            };
            if rest.is_empty() {
                crate::menu::start_handoff_picker(app);
                return Ok(());
            }
            let mut parts = rest.splitn(2, char::is_whitespace);
            let head = parts.next().unwrap_or("");
            let note = parts
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let (kind_str, model) = match head.split_once(':') {
                Some((k, m)) if !m.is_empty() => (k, Some(m.to_string())),
                _ => (head, None),
            };
            let Some(kind) = AgentKind::parse(kind_str) else {
                app.status = format!("未知的 agent：{kind_str}");
                return Ok(());
            };
            send(
                writer,
                &Request::Handoff {
                    session: src,
                    kind: kind.clone(),
                    model,
                    effort: None,
                    note,
                },
            )?;
            app.status = format!("已要求交接給 {kind}");
        }
        "spawn" if rest.is_empty() => crate::menu::start_spawn_picker(app),
        "new" => crate::menu::start_spawn_picker(app),
        "spawn" => submit_spawn(app, writer, rest)?,
        "kill" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Kill { session: id })?;
                app.status = "已送出結束".into();
            }
        }
        "cancel" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Cancel { session: id })?;
                app.status = "已送出中斷".into();
            }
        }
        "rm" | "remove" => {
            let force = rest.split_whitespace().any(|w| w == "--force" || w == "-f");
            let target = rest
                .split_whitespace()
                .find(|w| !w.starts_with('-'))
                .map(str::to_string)
                .or_else(|| app.selected_id());
            if let Some(session) = target {
                send(writer, &Request::SessionRemove { session, force })?;
            } else {
                app.status = "沒有選取的 session".into();
            }
        }
        "clean" => {
            // 清除所有已結束且沒有 worktree 分支的 session。
            let ids: Vec<String> = app
                .state
                .sessions
                .values()
                .filter(|s| !s.status.is_live() && s.branch.is_none())
                .map(|s| s.id.clone())
                .collect();
            let n = ids.len();
            for session in ids {
                send(
                    writer,
                    &Request::SessionRemove {
                        session,
                        force: false,
                    },
                )?;
            }
            app.status = format!("已要求清除 {n} 個已結束的 session");
        }
        "raw" => {
            app.raw_md = !app.raw_md;
            app.md_cache = None;
            app.selection = None;
            app.status = format!(
                "逐字稿顯示：{}",
                if app.raw_md {
                    "原始文字"
                } else {
                    "Markdown 渲染"
                }
            );
        }
        "bus" => {
            app.bus_overlay = !app.bus_overlay;
            app.status = if app.bus_overlay {
                "Bus：顯示".into()
            } else {
                "Bus：隱藏".into()
            };
        }
        "merge" => {
            let parts = rest.split_whitespace();
            let mut dry_run = false;
            let mut target = None;
            for p in parts {
                if p == "--dry-run" || p == "-n" || p == "dry-run" {
                    dry_run = true;
                } else if target.is_none() {
                    target = Some(p.to_string());
                }
            }
            let target = target.or_else(|| app.selected_id());
            if let Some(s) = target {
                send(
                    writer,
                    &Request::WorktreeMerge {
                        session: s,
                        dry_run,
                    },
                )?;
            }
        }
        "diff" => {
            app.view = CenterView::Diff;
            app.selection = None;
            request_selected_diff(app, writer)?;
        }
        "transcript" | "overview" => {
            app.view = CenterView::Transcript;
            app.selection = None;
        }
        "worktrees" | "wt" => {
            app.view = CenterView::Worktrees;
            app.selection = None;
            send(writer, &Request::WorktreeList)?;
        }
        "thoughts" | "think" => toggle_thoughts(app),
        "tools" => toggle_tools(app),
        "scope" | "project" => match rest {
            "here" | "this" => toggle_scope(app, Some(true)),
            "all" => toggle_scope(app, Some(false)),
            "" => toggle_scope(app, None),
            _ => app.status = "用法：:scope [here|all]".into(),
        },
        "queue" => queue_command(app, writer, rest)?,
        "unqueue" => unqueue(app),
        "layout" => {
            if rest.is_empty() {
                app.cycle_layout();
            } else if let Some(mode) = LayoutMode::parse(rest) {
                app.set_layout(mode);
            } else {
                app.status = "用法：/layout three|two|tabs".into();
            }
        }
        "gc" => {
            let session = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            send(
                writer,
                &Request::WorktreeGc {
                    session,
                    force: false,
                },
            )?;
        }
        "sleep" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Sleep { session: id })?;
            }
        }
        "wake" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Wake { session: id })?;
            }
        }
        "task" if !rest.is_empty() => {
            send(
                writer,
                &Request::TaskCreate {
                    title: rest.to_string(),
                    detail: String::new(),
                    assignee: None,
                    created_by: None,
                    depends_on: Vec::new(),
                },
            )?;
        }
        "broadcast" if !rest.is_empty() => {
            send(
                writer,
                &Request::BusSend {
                    from: None,
                    to: Target::Broadcast,
                    kind: MsgKind::Chat,
                    body: rest.to_string(),
                    reply_to: None,
                },
            )?;
        }
        "copy" | "yank" => copy_main_pane(app),
        "shutdown" => {
            send(writer, &Request::Shutdown)?;
            app.should_quit = true;
        }
        "q" | "quit" => {
            app.should_quit = true;
        }
        other => {
            app.status = format!("未知的命令：/{other}");
        }
    }
    Ok(())
}

pub(crate) fn execute_jump(app: &mut App, writer: &mut ConnStream, query: &str) -> Result<()> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Ok(());
    }
    let sessions = app.state.sessions_sorted();
    if let Some((idx, s)) = sessions.iter().enumerate().find(|(_, s)| {
        s.label.to_ascii_lowercase().contains(&q)
            || s.id.to_ascii_lowercase().contains(&q)
            || s.kind.id().to_ascii_lowercase().contains(&q)
            || s.branch
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains(&q)
    }) {
        app.selected = idx;
        app.status = format!("已跳轉至 {} ({})", s.label, s.id);
        if app.view == CenterView::Diff {
            request_selected_diff(app, writer)?;
        }
    } else {
        app.status = format!("找不到符合 `{query}` 的 session");
    }
    Ok(())
}

pub(crate) fn submit_spawn(app: &mut App, writer: &mut ConnStream, text: &str) -> Result<()> {
    // 由選擇器選好 agent／model 時，輸入框的內容就是 prompt；
    // 否則維持舊格式 `<agent[:model]> <prompt>`（`:spawn claude:opus …`）。
    let (kind, model, prompt) = if let Some((kind, model)) = app.spawn_target.clone() {
        (kind, model, Some(text.trim().to_string()))
    } else {
        let mut parts = text.splitn(2, char::is_whitespace);
        let head = parts.next().unwrap_or("");
        let (kind_str, model) = match head.split_once(':') {
            Some((k, m)) if !m.is_empty() => (k, Some(m.to_string())),
            _ => (head, None),
        };
        let prompt = parts.next().map(|s| s.trim().to_string());
        let Some(kind) = AgentKind::parse(kind_str) else {
            app.status = format!("未知的 agent：{kind_str}（opencode/claude/codex/antigravity）");
            return Ok(());
        };
        (kind, model, prompt)
    };
    let prompt = prompt.filter(|p| !p.is_empty());
    let root = app.search_root();
    let (prompt, attachments) = match prompt
        .as_deref()
        .map(|p| crate::attach::extract_attachments(p, &root))
    {
        Some(Ok((body, atts))) => (Some(body), atts),
        Some(Err(e)) => {
            app.status = format!("附件錯誤：{e}");
            app.mode = Mode::Spawn;
            app.input = text.to_string();
            app.input_cursor = app.input.chars().count();
            return Ok(());
        }
        None => (None, Vec::new()),
    };
    // bypass 需要再按一次 Enter 確認。
    if app.spawn_permission == Some(PermissionPolicy::Bypass) && !app.spawn_bypass_armed {
        app.spawn_bypass_armed = true;
        app.mode = Mode::Spawn;
        app.input = text.to_string();
        app.input_cursor = app.input.chars().count();
        app.status = "⚠ bypass 會跳過所有權限檢查；再按一次 Enter 確認".into();
        return Ok(());
    }
    app.spawn_target = None;
    let effort = app.spawn_effort.take();
    app.spawn_variants.clear();
    send(
        writer,
        &Request::Spawn(SpawnRequest {
            kind: kind.clone(),
            cwd: None,
            label: None,
            role: Role::Worker,
            prompt,
            worktree: app.spawn_worktree,
            transport: Transport::Auto,
            permission: app.spawn_permission,
            parent: None,
            model,
            effort,
            attachments,
            inherit_from: None,
            args: vec![],
        }),
    )?;
    app.status = format!("已要求啟動 {kind}");
    Ok(())
}

pub(crate) fn respond_permission(
    app: &mut App,
    writer: &mut ConnStream,
    decision: Decision,
) -> Result<()> {
    let Some(id) = app.selected_id() else {
        return Ok(());
    };
    let Some(req) = app.selected_permission() else {
        app.status = "此 session 沒有待審批請求".into();
        return Ok(());
    };
    send(
        writer,
        &Request::Permission {
            session: id,
            req,
            decision,
        },
    )?;
    app.status = match decision {
        Decision::Allow => "已允許".into(),
        Decision::AllowAlways => "已一律允許".into(),
        Decision::Deny => "已拒絕".into(),
    };
    Ok(())
}
