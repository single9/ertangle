//! TUI 命令面板、跳轉、啟動與核准回覆。

use anyhow::{bail, Result};
use ertangle_core::{AgentKind, Decision, MsgKind, PermissionPolicy, Role, Target};
use ertangle_protocol::{MergeStrategy, Request, SpawnRequest, Transport};
use tr::tr;

use crate::app::{
    toggle_ended, toggle_scope, toggle_thoughts, toggle_tools, App, CenterView, LayoutMode, Mode,
};
use crate::clipboard::copy_main_pane;
use crate::hosts::HostRouter;
use crate::{request_selected_diff, send};

/// 在輸入框支援 `@session 訊息`、`#channel 訊息`、`/command` 或直接送 prompt 給當前 session。
pub(crate) fn submit_prompt_or_route(
    app: &mut App,
    writer: &mut HostRouter,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if let Some(cmd) = trimmed.strip_prefix('/') {
        return execute_command(app, writer, cmd);
    }
    // `!<cmd>` 在選取 session 的工作目錄執行 shell 命令，不送給 agent。
    if let Some(cmd) = trimmed.strip_prefix('!') {
        return exec_shell_command(app, writer, cmd);
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
            app.status = tr!("已傳送訊息至 {target_str}", target_str = target_str);
            return Ok(());
        }
    }
    if let Some(id) = app.selected_id() {
        let (body, attachments) =
            match crate::attach::extract_attachments(trimmed, &app.search_root()) {
                Ok(r) => r,
                Err(e) => {
                    app.status = tr!("附件錯誤：{e}", e = e);
                    return Ok(());
                }
            };
        let session_opt = app
            .state
            .sessions
            .get(&id)
            .or_else(|| app.state.archived_sessions.get(&id))
            .or_else(|| {
                app.hosts.get(&app.selected_host()).and_then(|h| {
                    h.state
                        .sessions
                        .get(&id)
                        .or_else(|| h.state.archived_sessions.get(&id))
                })
            })
            .cloned();
        if let Some(s) = session_opt {
            if matches!(
                s.status,
                ertangle_core::SessionStatus::Exited
                    | ertangle_core::SessionStatus::Failed
                    | ertangle_core::SessionStatus::Sleeping
            ) {
                let resumable = s.native_id.is_some();
                crate::menu::start_confirm_resume_prompt(
                    app,
                    id,
                    s.label,
                    s.status,
                    resumable,
                    body,
                    attachments,
                );
                return Ok(());
            }
        }
        let n = attachments.len();
        send(
            writer,
            &Request::Prompt {
                session: id,
                text: body,
                attachments,
                queue: false,
            },
        )?;
        app.status = if n > 0 {
            tr!("已送出 prompt (含 {n} 個附件)", n = n)
        } else {
            tr!("已送出 prompt")
        };
    } else {
        app.status = tr!("沒有選取的 session");
    }
    Ok(())
}

/// 在選取 session 的工作目錄執行 shell 命令 (T74)。
pub(crate) fn exec_shell_command(
    app: &mut App,
    writer: &mut HostRouter,
    command: &str,
) -> Result<()> {
    let command = command.trim();
    if command.is_empty() {
        app.status = tr!("命令不可為空");
        return Ok(());
    }
    let Some(session) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return Ok(());
    };
    send(
        writer,
        &Request::Exec {
            session,
            command: command.to_string(),
        },
    )?;
    app.status = tr!("已送出 Shell 命令：! {command}", command = command);
    Ok(())
}

/// 把一則 prompt 排進選取 session 的佇列 (交由 daemon 管理)；session 已閒置就直接送。
/// 只處理一般 prompt (含 `@檔案` 附件)；`/命令`、`@peer`、`#channel` 不排隊。
pub(crate) fn queue_prompt(app: &mut App, writer: &mut HostRouter, text: &str) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let routed = trimmed.starts_with('/')
        || trimmed.starts_with('#')
        || (trimmed.starts_with('@')
            && !crate::attach::leads_with_file(trimmed, &app.search_root()));
    let Some(id) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return Ok(());
    };
    if routed {
        return submit_prompt_or_route(app, writer, text);
    }
    if app.state.sessions.get(&id).is_some_and(|s| s.over_budget) {
        app.status = tr!("已超過成本上限，先用 :budget <金額> 調高");
        return Ok(());
    }
    let (body, attachments) = match crate::attach::extract_attachments(trimmed, &app.search_root())
    {
        Ok(r) => r,
        Err(e) => {
            app.status = tr!("附件錯誤：{e}", e = e);
            return Ok(());
        }
    };
    let n_att = attachments.len();
    send(
        writer,
        &Request::Prompt {
            session: id.clone(),
            text: body,
            attachments,
            queue: true,
        },
    )?;
    let idle_and_empty = app
        .state
        .sessions
        .get(&id)
        .is_some_and(|s| s.status == ertangle_core::SessionStatus::Idle)
        && app.state.queues.get(&id).is_none_or(|q| q.is_empty());
    if idle_and_empty {
        app.status = if n_att > 0 {
            tr!("已送出 prompt (含 {n_att} 個附件)", n_att = n_att)
        } else {
            tr!("已送出 prompt")
        };
    } else {
        let q = app.state.queues.entry(id).or_default();
        if q.len() < 20 {
            q.push(trimmed.to_string());
        }
        app.status = tr!("已排隊 (共 {n} 則，閒下來自動送出)", n = q.len());
    }
    Ok(())
}

/// `:queue [文字|clear]`、`:unqueue`。
fn queue_command(app: &mut App, writer: &mut HostRouter, rest: &str) -> Result<()> {
    let Some(id) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return Ok(());
    };
    match rest.trim() {
        "" => {
            let items = app.state.queues.get(&id).cloned().unwrap_or_default();
            app.status = if items.is_empty() {
                tr!("排隊為空 (:queue <文字> 排入，或在輸入框按 Alt-Enter)")
            } else {
                let list: Vec<String> = items
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("{}.{}", i + 1, t.chars().take(24).collect::<String>()))
                    .collect();
                tr!(
                    "排隊 {n} 則：{list}",
                    n = items.len(),
                    list = list.join(" | ")
                )
            };
            Ok(())
        }
        "clear" => {
            let n = app.state.queues.remove(&id).map_or(0, |q| q.len());
            send(writer, &Request::QueueClear { session: id })?;
            app.status = tr!("已清空 {n} 則排隊 prompt", n = n);
            Ok(())
        }
        text => queue_prompt(app, writer, text),
    }
}

/// `:command` 命令：`:unqueue`。
fn unqueue(app: &mut App, writer: &mut HostRouter) -> Result<()> {
    let Some(id) = app.selected_id() else {
        return Ok(());
    };
    let popped = {
        let item = app.state.queues.get_mut(&id).and_then(|q| q.pop());
        if app.state.queues.get(&id).is_some_and(|q| q.is_empty()) {
            app.state.queues.remove(&id);
        }
        item
    };
    match popped {
        Some(text) => {
            send(writer, &Request::QueuePop { session: id })?;
            // 取回輸入框方便修改後重新排入。
            app.input_cursor = text.chars().count();
            app.input = text;
            app.mode = Mode::Prompt;
            app.status = tr!("已取消最後一則排隊，內容放回輸入框");
        }
        None => {
            app.status = tr!("沒有排隊中的 prompt");
        }
    }
    Ok(())
}

/// `:` 命令列可用的命令：(名稱, 用法與說明)，供提示與 Tab 自動完成。
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    (
        "spawn",
        ertangle_core::tr_noop!("[agent[:model] prompt]  新增 agent；不帶參數開選擇器"),
    ),
    ("new", ertangle_core::tr_noop!("開選擇器：agent → model → prompt")),
    (
        "playbook",
        ertangle_core::tr_noop!("[名稱 [目標需求]]  啟動多 Agent 團隊範本；不帶參數開選擇器"),
    ),
    (
        "export",
        ertangle_core::tr_noop!("[路徑]  匯出目前 session 樹為 Markdown，寫檔並複製到剪貼簿"),
    ),
    ("help", ertangle_core::tr_noop!("說明面板：所有按鍵與命令")),
    (
        "exec",
        ertangle_core::tr_noop!("<命令>  在選取 session 的工作目錄執行 shell 命令 (亦可在輸入框打 !<命令>)"),
    ),
    ("quota", ertangle_core::tr_noop!("開啟額度面板：已安裝 agent 的各視窗用量與重置時間")),
    (
        "budget",
        ertangle_core::tr_noop!("<金額>  設定或調高選取 session 的成本上限 (美元)"),
    ),
    (
        "handoff",
        ertangle_core::tr_noop!("[agent[:model]] [說明]  把選取 session 的工作交接給另一個 agent"),
    ),
    (
        "resume",
        ertangle_core::tr_noop!("[prompt]  不帶參數開選擇器 (Tab 切換專案範圍)；有 prompt 時接續選取的 session"),
    ),
    (
        "clear",
        ertangle_core::tr_noop!("[force]  重設選取 session 的上下文 (同一個 id、worktree 與任務，agent 以全新對話重啟)"),
    ),
    (
        "model",
        ertangle_core::tr_noop!("[名稱]  切換選取 session 的 model (下一回合起生效)；不帶參數列出目前與可選項"),
    ),
    (
        "effort",
        ertangle_core::tr_noop!("[low|medium|high|xhigh|max]  切換選取 session 的推理強度；不帶參數顯示目前值"),
    ),
    ("settings", ertangle_core::tr_noop!("開啟設定面板：用清單切換預設 model、effort、權限、agent 建立 session 的防呆")),
    (
        "set",
        ertangle_core::tr_noop!("不帶參數開設定面板；[--repo] <agent|permission|model|effort> <值>  直接寫入 config.toml (空值＝刪除)"),
    ),
    ("kill", ertangle_core::tr_noop!("結束選取的 session")),
    ("cancel", ertangle_core::tr_noop!("中斷選取 session 的目前回合")),
    ("sleep", ertangle_core::tr_noop!("休眠選取的 session")),
    ("wake", ertangle_core::tr_noop!("喚醒選取的 session")),
    ("rm", ertangle_core::tr_noop!("[session] [--force]  移除已結束的 session")),
    (
        "clean",
        ertangle_core::tr_noop!(
            "[--force]  清除所有已結束的 session (預設保留未合併或有變更的 worktree)"
        ),
    ),
    ("merge", ertangle_core::tr_noop!("[session] [--dry-run] [--resolve]  合併 worktree 回主工作區")),
    ("diff", ertangle_core::tr_noop!("顯示選取 session 的 diff")),
    (
        "discard",
        ertangle_core::tr_noop!("(Diff 視圖) 退回游標所在的 hunk 或整個檔案；:discard! 直接執行"),
    ),
    ("gc", ertangle_core::tr_noop!("[session]  清理已合併的 worktree")),
    (
        "prune",
        ertangle_core::tr_noop!("封存已結束且已合併 (或沒有 worktree) 的 session；未合併的保留"),
    ),
    ("ended", ertangle_core::tr_noop!("[show|hide]  展開／摺疊側欄的已結束群組")),
    ("worktrees", ertangle_core::tr_noop!("顯示 worktree 清單")),
    ("overview", ertangle_core::tr_noop!("回到 transcript 視圖")),
    (
        "find",
        ertangle_core::tr_noop!("[文字]  在逐字稿或 diff 裡搜尋畫面上顯示的內容 (同 f)"),
    ),
    ("layout", ertangle_core::tr_noop!("[three|two|tabs]  切換版面")),
    ("raw", ertangle_core::tr_noop!("原始文字／Markdown 切換")),
    ("thoughts", ertangle_core::tr_noop!("顯示／隱藏思考")),
    ("tools", ertangle_core::tr_noop!("展開／摺疊工具呼叫")),
    ("scope", ertangle_core::tr_noop!("[here|all]  只看本專案／顯示全部專案 (session、Bus、任務)")),
    (
        "project",
        ertangle_core::tr_noop!("[here|all|browse|open <路徑>]  切換專案範圍或開啟專案 (browse 瀏覽資料夾；open 省略路徑開專案清單)"),
    ),
    (
        "skill",
        ertangle_core::tr_noop!("[install|uninstall|show] [claude|agents|codex] [global]  安裝 ertangle-ade skill"),
    ),
    (
        "notify",
        ertangle_core::tr_noop!("[on|off|test]  開關通知 (回合結束／等待核准／失敗／額度用完)；不帶參數顯示狀態"),
    ),
    (
        "queue",
        ertangle_core::tr_noop!("[文字|clear]  排隊 prompt，session 閒下來自動送；不帶參數列出"),
    ),
    ("unqueue", ertangle_core::tr_noop!("取消最後一則排隊並放回輸入框")),
    ("web", ertangle_core::tr_noop!("[on|off|status|qr|<addr>]  設定與啟動 Web 版")),
    ("remote", ertangle_core::tr_noop!("顯示 Web 版網址與連線資訊")),
    ("bus", ertangle_core::tr_noop!("顯示／隱藏 Bus 浮層")),
    ("task", ertangle_core::tr_noop!("<標題>  新增任務")),
    ("tasks", ertangle_core::tr_noop!("任務視圖：依狀態分組，Enter 跳到負責人")),
    ("flow", ertangle_core::tr_noop!("流程儀表板：session 節點與訊息流向 (同 F)")),
    (
        "task-status",
        ertangle_core::tr_noop!("<todo|doing|blocked|review|done>  改任務視圖選取任務的狀態"),
    ),
    ("broadcast", ertangle_core::tr_noop!("<訊息>  廣播給所有 session")),
    (
        "copy",
        ertangle_core::tr_noop!("[編號]  複製主要區塊；Transcript 可指定第幾則訊息 (預設最新)"),
    ),
    ("quit", ertangle_core::tr_noop!("離開 (保留 daemon)")),
    (
        "stop",
        ertangle_core::tr_noop!("結束本專案所有執行中的 session (不動 daemon 與其他專案)"),
    ),
    (
        "shutdown",
        ertangle_core::tr_noop!("關閉共用 daemon (結束所有專案的 session)並離開；有 session 在跑時需 :shutdown! 確認"),
    ),
    (
        "restart",
        ertangle_core::tr_noop!("重啟共用 daemon (換上新執行檔)；執行中的 session 自動接續，TUI 會自動重連；有 session 在跑時需 :restart! 確認"),
    ),
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

/// `:clean` 的候選 session：目前檢視範圍 (專案與主機篩選) 內所有已結束的 session。
pub(crate) fn clean_candidates(app: &App) -> Vec<String> {
    // Ended sessions count even while the sidebar group is collapsed.
    app.visible_sessions_with(true)
        .into_iter()
        .filter_map(|(host, id)| {
            let state = app.hosts.get(&host).map_or(&app.state, |h| &h.state);
            state
                .sessions
                .get(&id)
                .filter(|s| !s.status.is_live())
                .map(|_| id)
        })
        .collect()
}

pub(crate) fn execute_command(app: &mut App, writer: &mut HostRouter, raw: &str) -> Result<()> {
    // `//名稱 …`：強制轉發給 agent (即使跟 TUI 命令同名)。
    if let Some(native) = raw.trim().strip_prefix('/').filter(|r| r.starts_with('/')) {
        let native = native.trim_start_matches('/');
        let mut p = native.splitn(2, char::is_whitespace);
        let verb = p.next().unwrap_or("").to_string();
        let rest = p.next().unwrap_or("").trim().to_string();
        return forward_native(app, writer, &verb, &rest);
    }
    let raw = raw.trim().trim_start_matches('/');
    // `:!<cmd>` 是 shell 命令的簡寫 (大小寫必須保留，所以在轉小寫前處理)。
    if let Some(cmd) = raw.strip_prefix('!') {
        return exec_shell_command(app, writer, cmd.trim());
    }
    let mut parts = raw.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match verb.as_str() {
        "exec" | "sh" => exec_shell_command(app, writer, rest)?,
        "help" | "h" | "?" => crate::help::open_help(app),
        "find" => crate::search::open(app, (!rest.is_empty()).then_some(rest)),
        "quota" => crate::quota::open(app),
        "model" | "effort" => session_config_command(app, writer, &verb, rest)?,
        "settings" => crate::settings::open(app),
        "set" if rest.is_empty() => crate::settings::open(app),
        "set" => set_default_command(app, rest),
        "budget" => {
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            let (target, amount_str) = match tokens.as_slice() {
                [amt] => (app.selected_id(), *amt),
                [ses, amt] => (Some((*ses).to_string()), *amt),
                _ => {
                    app.status = tr!("用法：:budget <金額> (或 :budget <session> <金額>)");
                    return Ok(());
                }
            };
            let Some(session) = target else {
                app.status = tr!("沒有選取的 session");
                return Ok(());
            };
            let clean = amount_str.trim_start_matches('$');
            let Ok(budget_usd) = clean.parse::<f64>() else {
                app.status = tr!("無效的金額：{amount_str}", amount_str = amount_str);
                return Ok(());
            };
            send(
                writer,
                &Request::SessionBudget {
                    session,
                    budget_usd,
                },
            )?;
            app.status = tr!(
                "已設定成本上限為 {limit}",
                limit = ertangle_core::format_usd_limit(budget_usd)
            );
        }
        "handoff" => {
            let Some(src) = app.selected_id() else {
                app.status = tr!("沒有選取的 session");
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
                app.status = tr!("未知的 agent：{kind_str}", kind_str = kind_str);
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
                    role: None,
                    parent: None,
                    force: false,
                },
            )?;
            app.status = tr!("已要求交接給 {kind}", kind = kind);
        }
        "spawn" if rest.is_empty() => crate::menu::start_spawn_picker(app),
        "new" => crate::menu::start_spawn_picker(app),
        "spawn" => submit_spawn(app, writer, rest)?,
        "playbook" if rest.is_empty() => crate::menu::start_playbook_picker(app),
        "playbook" => {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").to_string();
            let goal = parts.next().unwrap_or("").trim().to_string();
            if goal.is_empty() {
                app.input = format!("playbook {name} ");
                app.input_cursor = app.input.chars().count();
                app.mode = Mode::Command;
                app.status = tr!("輸入目標需求後按 Enter 啟動 playbook {name}", name = name);
            } else {
                launch_playbook(app, writer, &name, &goal)?;
            }
        }
        "export" => export_session(app, (!rest.is_empty()).then_some(rest)),
        "kill" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Kill { session: id })?;
                app.status = tr!("已送出結束");
            }
        }
        "resume" => {
            resume_command(app, writer, rest)?;
        }
        "clear" => {
            if let Some(id) = app.selected_id() {
                let force = rest.trim() == "force";
                send(writer, &Request::Clear { session: id, force })?;
                app.status = tr!("已要求重設上下文");
            } else {
                app.status = tr!("沒有選取的 session");
            }
        }
        "cancel" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Cancel { session: id })?;
                app.status = tr!("已送出中斷");
            }
        }
        "ps" => crate::ps::open(app),
        "bg-kill" => {
            let Some(id) = app.selected_id() else {
                app.status = tr!("沒有選取的 session");
                return Ok(());
            };
            if rest.is_empty() {
                app.status = tr!("用法：:bg-kill <PID> (或 :bg-kill --all)");
                return Ok(());
            }
            if rest == "--all" || rest == "-a" || rest == "all" {
                send(
                    writer,
                    &Request::KillBgProc {
                        session: id.clone(),
                        pid: None,
                        force: false,
                    },
                )?;
                app.status = tr!("已要求清理 {session} 底下所有背景處理程序", session = id);
            } else if let Ok(pid) = rest.parse::<u32>() {
                send(
                    writer,
                    &Request::KillBgProc {
                        session: id,
                        pid: Some(pid),
                        force: false,
                    },
                )?;
                app.status = tr!("已請求終止處理程序 PID {pid}", pid = pid);
            } else {
                app.status = tr!("無效的 PID：{rest}", rest = rest);
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
                app.status = tr!("沒有選取的 session");
            }
        }
        "clean" => {
            // 清除目前檢視範圍內所有已結束的 session (含 worktree)；預設安全模式，
            // 未合併或還有未 commit 變更的 worktree 由 daemon 保留，--force 才會強制刪除。
            let force = rest.split_whitespace().any(|w| w == "--force" || w == "-f");
            let ids = clean_candidates(app);
            if ids.is_empty() {
                app.status = tr!("沒有可清除的已結束 session");
                return Ok(());
            }
            let n = ids.len();
            for session in ids {
                send(writer, &Request::SessionRemove { session, force })?;
            }
            app.status = tr!("已要求清除 {n} 個已結束的 session", n = n);
        }
        "raw" => {
            app.raw_md = !app.raw_md;
            app.md_cache = None;
            app.selection = None;
            app.status = if app.raw_md {
                tr!("逐字稿顯示：原始文字")
            } else {
                tr!("逐字稿顯示：Markdown 渲染")
            };
        }
        "bus" => app.toggle_bus(),
        "grid" => crate::project::toggle_grid(app),
        "merge" => {
            let parsed = parse_merge_args(rest);
            let target = parsed.target.or_else(|| app.selected_id());
            if let Some(s) = target {
                send(
                    writer,
                    &Request::WorktreeMerge {
                        session: s,
                        dry_run: parsed.dry_run,
                        resolve: parsed.resolve,
                        strategy: parsed.strategy,
                    },
                )?;
            }
        }
        "diff" => {
            app.view = CenterView::Diff;
            app.selection = None;
            request_selected_diff(app, writer)?;
        }
        "discard" | "discard!" => {
            let force = verb == "discard!"
                || rest == "!"
                || rest.split_whitespace().any(|w| w == "--force" || w == "-f");
            discard_diff(app, writer, force)?;
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
        "tasks" => {
            app.view = CenterView::Tasks;
            app.selection = None;
        }
        "flow" => crate::app::toggle_flow(app),
        "task-status" => crate::tasks::set_selected_status(app, writer, rest)?,
        "thoughts" | "think" => toggle_thoughts(app),
        "tools" => toggle_tools(app),
        "skill" => skill_command(app, rest)?,
        "scope" => match rest {
            "here" | "this" => toggle_scope(app, Some(true)),
            "all" => toggle_scope(app, Some(false)),
            "" => toggle_scope(app, None),
            _ => app.status = tr!("用法：:scope [here|all]"),
        },
        "project" => project_command(app, rest)?,
        "notify" => match rest {
            "on" | "off" => {
                app.notify.enabled = rest == "on";
                app.status = app.notify.describe();
            }
            "" => app.status = app.notify.describe(),
            "test" => {
                app.notify.pending.push(crate::notify::Notice {
                    kind: crate::notify::NoticeKind::Done,
                    session: app.selected_id().unwrap_or_default(),
                    label: "ertangle".into(),
                    text: tr!("ertangle：測試通知"),
                });
                app.status = tr!("已送出測試通知；{state}", state = app.notify.describe());
            }
            _ => app.status = tr!("用法：:notify [on|off|test]"),
        },
        "queue" => queue_command(app, writer, rest)?,
        "unqueue" => unqueue(app, writer)?,
        "remote" | "web" => {
            let cmd_arg = rest.trim();
            let config_p = ertangle_daemon::config_dir().join("config.toml");
            let cfg = ertangle_daemon::config::Config::load_file(&config_p);
            if cmd_arg == "off" {
                send(
                    writer,
                    &Request::WebControl {
                        enabled: Some(false),
                        addr: None,
                    },
                )?;
                let _ = ertangle_daemon::config::Config::set_toml(
                    &config_p,
                    &["web"],
                    "enabled",
                    Some(ertangle_daemon::config::TomlValue::Boolean(false)),
                );
                let _ =
                    ertangle_daemon::config::Config::set_toml(&config_p, &["web"], "addr", None);
                app.status = tr!("已關閉 Web 版並儲存至設定檔");
                return Ok(());
            }

            let is_start = cmd_arg == "on"
                || cmd_arg.starts_with("on ")
                || (!cmd_arg.is_empty() && !matches!(cmd_arg, "qr" | "show" | "status"));

            if is_start {
                let spec = if cmd_arg == "on" {
                    ""
                } else if let Some(s) = cmd_arg.strip_prefix("on ") {
                    s.trim()
                } else {
                    cmd_arg
                };
                let addr = if spec.is_empty() {
                    cfg.web.resolved_addr()
                } else {
                    spec.to_string()
                };
                send(
                    writer,
                    &Request::WebControl {
                        enabled: Some(true),
                        addr: Some(addr.clone()),
                    },
                )?;
                let _ = ertangle_daemon::config::Config::set_toml(
                    &config_p,
                    &["web"],
                    "enabled",
                    Some(ertangle_daemon::config::TomlValue::Boolean(true)),
                );
                if let Ok(port) = addr.parse::<u16>() {
                    let _ = ertangle_daemon::config::Config::set_toml(
                        &config_p,
                        &["web"],
                        "port",
                        Some(ertangle_daemon::config::TomlValue::Integer(port as i64)),
                    );
                } else {
                    let _ = ertangle_daemon::config::Config::set_toml(
                        &config_p,
                        &["web"],
                        "addr",
                        Some(ertangle_daemon::config::TomlValue::String(addr)),
                    );
                }
                app.status = tr!("正在啟動 Web 版…");
                return Ok(());
            }

            let token_path = ertangle_daemon::default_token_path();
            let token = if token_path.exists() {
                ertangle_daemon::read_token_file(&token_path).ok()
            } else {
                ertangle_daemon::get_client_token()
            };
            let tailscale_ip = ertangle_daemon::detect_tailscale_ip()
                .ok()
                .map(|ip| ip.to_string());
            let token_hint = token.as_deref().unwrap_or("");
            let web_port = cfg.web.resolved_port();
            let host = tailscale_ip.as_deref().unwrap_or("127.0.0.1");
            let mut url = format!("http://{host}:{web_port}/?token={token_hint}");
            if app.scope_here && !app.home_project.is_empty() {
                url.push_str("&project=");
                url.push_str(&percent_encode(&app.home_project));
            }

            // 狀態 / QR 一律以彈出視窗顯示網址與 QR Code。
            if let Some(qr) = ertangle_daemon::web::qr::QrCode::encode(&url) {
                app.qr_modal = Some(crate::app::QrModal {
                    title: tr!("Web 版 QR Code"),
                    url: url.clone(),
                    qr_lines: qr.render_lines(),
                });
                app.mode = crate::app::Mode::Qr;
                return Ok(());
            }

            app.status = tr!(
                "Web 版網址：{url} (輸入 :web qr 開啟 QR Code，或 :web on|off 控制)",
                url = url
            );
        }
        "host" => {
            let target = rest.trim();
            if target.is_empty() || target == "all" || target == "*" {
                app.host_filter = None;
                app.status = tr!("主機篩選：全部主機");
            } else if app.hosts.contains_key(target) {
                app.host_filter = Some(target.to_string());
                app.status = tr!("主機篩選：{target}", target = target);
            } else {
                let avail = app.hosts.keys().cloned().collect::<Vec<_>>().join(", ");
                bail!(
                    "{}",
                    tr!(
                        "未知的主機：{target}，可用主機：{avail}",
                        target = target,
                        avail = avail
                    )
                );
            }
            app.clamp_selection();
            writer.set_active(&app.active_host);
        }
        "layout" => {
            if rest.is_empty() {
                app.cycle_layout();
            } else if let Some(mode) = LayoutMode::parse(rest) {
                app.set_layout(mode);
            } else {
                app.status = tr!("用法：/layout three|two|tabs");
            }
        }
        "prune" => {
            let project = app.scope_here.then(|| app.home_project.clone());
            send(writer, &Request::Prune { project })?;
        }
        "ended" => match rest {
            "show" => toggle_ended(app, Some(true)),
            "hide" => toggle_ended(app, Some(false)),
            "" => toggle_ended(app, None),
            _ => app.status = tr!("用法：:ended [show|hide]"),
        },
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
        "copy" | "yank" => {
            let msg = if rest.is_empty() {
                None
            } else {
                match rest.trim().parse::<usize>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        app.status = tr!("用法：copy [訊息編號]",);
                        return Ok(());
                    }
                }
            };
            copy_main_pane(app, msg);
        }
        "shutdown" | "shutdown!" => {
            // daemon 是所有專案共用的：關掉它會結束每個專案的 agent。有人在跑就先確認。
            let live: Vec<&ertangle_core::Session> = app.state.live_sessions().collect();
            let others = live
                .iter()
                .filter(|s| s.project_root() != app.home_project)
                .count();
            if verb == "shutdown" && !live.is_empty() {
                app.status = tr!(
                    "shutdown 會關掉共用的 daemon，結束 {n} 個執行中的 session (其他專案 {others} 個)。確定請輸入 :shutdown!；只想結束本專案請用 :stop；只離開 TUI 用 :quit",
                    n = live.len(),
                    others = others
                );
            } else {
                send(writer, &Request::Shutdown)?;
                app.should_quit = true;
            }
        }
        "restart" | "restart!" => {
            // 重啟會中斷每個專案正在跑的回合 (新 daemon 再自動接續)，有人在跑就先確認。
            let live = app.state.live_sessions().count();
            if verb == "restart" && live > 0 {
                app.status = tr!(
                    "restart 會重啟共用的 daemon，中斷 {n} 個執行中的 session 並自動接續。確定請輸入 :restart!",
                    n = live
                );
            } else {
                send(writer, &Request::Restart)?;
                app.status = tr!("已要求 daemon 重啟，等待重連…");
            }
        }
        "stop" => {
            // 只結束本專案執行中的 session，不動 daemon 與其他專案。
            let ids: Vec<String> = app
                .state
                .live_sessions()
                .filter(|s| s.project_root() == app.home_project)
                .map(|s| s.id.clone())
                .collect();
            for id in &ids {
                send(
                    writer,
                    &Request::Kill {
                        session: id.clone(),
                    },
                )?;
            }
            app.status = tr!("已要求結束本專案 {n} 個執行中的 session", n = ids.len());
        }
        "q" | "quit" => {
            app.should_quit = true;
        }
        other => forward_native(app, writer, other, rest)?,
    }
    Ok(())
}

fn project_command(app: &mut App, rest: &str) -> Result<()> {
    let rest = rest.trim();
    if rest.is_empty() {
        toggle_scope(app, None);
    } else if rest == "here" || rest == "this" {
        toggle_scope(app, Some(true));
    } else if rest == "all" {
        toggle_scope(app, Some(false));
    } else if let Some(path_arg) = rest
        .strip_prefix("open")
        .or_else(|| rest.strip_prefix("set"))
    {
        let path_str = path_arg.trim();
        if path_str.is_empty() {
            crate::menu::start_project_picker(app);
        } else if let Err(err) = app.set_active_project(std::path::Path::new(path_str)) {
            app.status = err;
        }
    } else if rest == "browse" || rest == "folder" {
        crate::menu::start_folder_browser(app);
    } else if rest == "pick" || rest == "select" || rest == "list" {
        crate::menu::start_project_picker(app);
    } else {
        app.status = tr!("用法：:project [here|all|browse|open <路徑>]");
    }
    Ok(())
}

/// 處理 `:resume`：不帶參數 (或 `--list`) 時開啟接續選擇器；
/// 帶參數時若首個詞對應已結束或已封存的 session id／label／native_id，則以該 session 為目標，其餘作為 prompt。
fn resume_command(app: &mut App, writer: &mut HostRouter, rest: &str) -> Result<()> {
    let rest = rest.trim();
    if rest.is_empty() || rest == "--list" || rest == "-l" || rest == "--pick" {
        crate::menu::start_resume_picker(app);
        return Ok(());
    }
    let resolve_known = |token: &str| -> Option<String> {
        for h in app.hosts.values() {
            if let Some((s, is_arch)) = h.state.resolve_resumable_session(token) {
                if is_arch || !s.status.is_live() {
                    return Some(s.id);
                }
            }
        }
        if let Some((s, is_arch)) = app.state.resolve_resumable_session(token) {
            if is_arch || !s.status.is_live() {
                return Some(s.id);
            }
        }
        None
    };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let after = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let (target, prompt) = if let Some(sid) = resolve_known(first) {
        (Some(sid), after)
    } else if first.starts_with("ses_") {
        (Some(first.to_string()), after)
    } else {
        (app.selected_id(), Some(rest.to_string()))
    };
    if let Some(session) = target {
        app.select_or_pend(&session);
        send(writer, &Request::Resume { session, prompt })?;
        app.status = tr!("已要求接續");
    } else {
        app.status = tr!("沒有選取的 session");
    }
    Ok(())
}

/// TUI 不認得的 `/指令` 是 agent 的原生斜線指令：原樣轉發給選取的 session。
/// `/` 開頭的名稱若跟 TUI 命令同名 (如 `/model`)，TUI 的優先；要送給 agent 就打 `//名稱`。
fn forward_native(app: &mut App, writer: &mut HostRouter, verb: &str, rest: &str) -> Result<()> {
    let Some(id) = app.selected_id() else {
        app.status = tr!(
            "未知的命令：/{verb} (沒有選取的 session 可轉發)",
            verb = verb
        );
        return Ok(());
    };
    let known = app
        .state
        .sessions
        .get(&id)
        .map(|s| s.slash_commands.iter().any(|c| c == verb));
    let text = if rest.is_empty() {
        format!("/{verb}")
    } else {
        format!("/{verb} {rest}")
    };
    send(
        writer,
        &Request::Prompt {
            session: id,
            text,
            attachments: vec![],
            queue: false,
        },
    )?;
    app.status = match known {
        Some(false) => tr!(
            "已轉發 /{verb} 給 agent (它沒有回報這個指令，可能被當成一般文字)",
            verb = verb
        ),
        _ => tr!("已轉發原生指令 /{verb} 給 agent", verb = verb),
    };
    Ok(())
}

/// 輸入框提示：TUI 命令加上選取 session 的原生斜線指令。
pub(crate) fn hints_with_native(app: &App) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = command_hints(&app.input)
        .into_iter()
        .map(|(n, d)| (n.to_string(), ertangle_core::tr_dyn!(d)))
        .collect();
    // 已打完命令名稱：用參數候選取代用法行 (用法留在 `:help`)。
    if let Some((_, cands)) = arg_candidates(app, &app.input) {
        if !cands.is_empty() {
            return cands;
        }
    }
    let input = app.input.trim_start().trim_start_matches(['/', ':']);
    if input.contains(char::is_whitespace) {
        return out;
    }
    let native = app
        .selected_id()
        .and_then(|id| app.state.sessions.get(&id))
        .map(|s| s.slash_commands.as_slice())
        .unwrap_or(&[]);
    for c in native {
        if c.starts_with(input) && !out.iter().any(|(n, _)| n == c) {
            out.push((c.clone(), tr!("(agent 原生指令，原樣轉發)")));
        }
    }
    out
}

/// agent 的可執行檔是否存在於 PATH (啟動前的檢查，避免留下建立到一半的團隊)。
fn agent_available(kind: &AgentKind) -> bool {
    let (bin, _) = kind.pty_command();
    if bin.contains('/') {
        return std::path::Path::new(bin).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
}

/// 展開並啟動一個多 Agent 團隊。主 session 先建立，其餘成員用主 session 的標籤當 `parent`
/// (daemon 會按請求順序處理，因此標籤在子 session 建立前已可解析)。
pub(crate) fn launch_playbook(
    app: &mut App,
    writer: &mut HostRouter,
    name: &str,
    goal: &str,
) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
    let Ok(mut spawns) = cfg.playbook_spawns(name, goal) else {
        app.status = tr!("未知的 playbook：{name}", name = name);
        return Ok(());
    };
    if spawns.is_empty() {
        app.status = tr!("playbook {name} 沒有成員", name = name);
        return Ok(());
    }
    for kind in spawns.iter().filter_map(|req| req.kind.as_ref()) {
        if !agent_available(kind) {
            app.status = tr!(
                "agent {kind} 未安裝；無法啟動 playbook {name}",
                kind = kind.id(),
                name = name
            );
            return Ok(());
        }
    }
    let main_label = spawns[0].label.clone().unwrap_or_default();
    let count = spawns.len();
    for (i, mut req) in spawns.drain(..).enumerate() {
        if i > 0 {
            req.parent = Some(main_label.clone());
        }
        send(writer, &Request::Spawn(req))?;
    }
    app.status = tr!(
        "已從 playbook {name} 啟動 {count} 個 session",
        name = name,
        count = count
    );
    Ok(())
}

/// `:export [path]`：把目前選取的 session 樹匯出成 Markdown，寫檔並透過 OSC 52 複製到剪貼簿。
pub(crate) fn export_session(app: &mut App, path: Option<&str>) {
    let Some(id) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return;
    };
    let diff = if app.view == CenterView::Diff
        && app.diff_session.as_deref() == Some(id.as_str())
        && !app.diff_raw_lines.is_empty()
    {
        Some(app.diff_raw_lines.join("\n"))
    } else {
        None
    };
    let md = match app.state.export_markdown(&id, true, diff.as_deref()) {
        Ok(md) => md,
        Err(e) => {
            app.status = e;
            return;
        }
    };
    let target = match path {
        Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p.trim()),
        _ => {
            let label = app
                .state
                .sessions
                .get(&id)
                .map(|s| {
                    s.label
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                                c
                            } else {
                                '-'
                            }
                        })
                        .collect::<String>()
                })
                .unwrap_or_else(|| "session".to_string());
            let ts = ertangle_core::now_millis() / 1000;
            std::path::Path::new(".ertangle")
                .join("exports")
                .join(format!("{label}-{ts}.md"))
        }
    };
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            app.status = tr!("建立匯出目錄失敗：{e}", e = e);
            return;
        }
    }
    if let Err(e) = std::fs::write(&target, &md) {
        app.status = tr!("寫入匯出檔失敗：{e}", e = e);
        return;
    }
    let path = target.display().to_string();
    match crate::clipboard::write_clipboard(&md) {
        Ok(backend) => {
            app.status = tr!(
                "已匯出至 {path} 並複製到剪貼簿 ({backend})",
                path = path,
                backend = backend
            );
        }
        Err(e) => {
            app.status = tr!("已匯出至 {path} (剪貼簿失敗：{e})", path = path, e = e);
        }
    }
}

pub(crate) fn execute_jump(app: &mut App, writer: &mut HostRouter, query: &str) -> Result<()> {
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
        app.status = tr!("已跳轉至 {label} ({id})", label = s.label, id = s.id);
        if app.view == CenterView::Diff {
            request_selected_diff(app, writer)?;
        }
    } else {
        app.status = tr!("找不到符合 `{query}` 的 session", query = query);
    }
    Ok(())
}

fn extract_named_flag(text: &str, flag: &str) -> (Option<String>, String) {
    for (idx, _) in text.match_indices(flag) {
        let before_ok = idx == 0 || text[..idx].ends_with(char::is_whitespace);
        let after_idx = idx + flag.len();
        let after_ok =
            after_idx == text.len() || text[after_idx..].starts_with(char::is_whitespace);
        if before_ok && after_ok {
            let after = &text[after_idx..];
            let mut parts = after.split_whitespace();
            if let Some(val) = parts.next() {
                let before = &text[..idx];
                if let Some(val_start) = after.find(val) {
                    let remainder = &after[val_start + val.len()..];
                    let rest_text = format!("{}{}", before, remainder).trim().to_string();
                    return (Some(val.to_string()), rest_text);
                }
            }
        }
    }
    (None, text.to_string())
}

pub(crate) fn submit_spawn(app: &mut App, writer: &mut HostRouter, text: &str) -> Result<()> {
    // 支援指定主機：--host <名稱>
    let (target_host, clean_text) = if let Some(idx) = text.find("--host") {
        let after = &text[idx + "--host".len()..];
        let mut parts = after.split_whitespace();
        if let Some(h) = parts.next() {
            let before = &text[..idx];
            let remainder = parts.collect::<Vec<_>>().join(" ");
            let rest_text = format!("{} {}", before.trim(), remainder.trim())
                .trim()
                .to_string();
            (Some(h.to_string()), rest_text)
        } else {
            (None, text.to_string())
        }
    } else {
        (None, text.to_string())
    };
    if let Some(h) = &target_host {
        writer.set_active(h);
    }
    // 支援指定角色：--role <角色> 或 -r <角色>
    let (target_role, clean_text) = {
        let (r, t) = extract_named_flag(&clean_text, "--role");
        if r.is_some() {
            (r, t)
        } else {
            extract_named_flag(&clean_text, "-r")
        }
    };
    let role = match target_role {
        Some(r) => Role::parse(&r),
        None => app.spawn_role.clone(),
    };
    let text = clean_text.as_str();
    // 由選擇器選好 agent／model 時，輸入框的內容就是 prompt；
    // 否則維持舊格式 `<agent[:model]> <prompt>`(`:spawn claude:opus …`)。
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
            app.status = tr!(
                "未知的 agent：{kind_str} (opencode/claude/codex/antigravity)",
                kind_str = kind_str
            );
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
            app.status = tr!("附件錯誤：{e}", e = e);
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
        app.status = tr!("⚠ bypass 會跳過所有權限檢查；再按一次 Enter 確認");
        return Ok(());
    }
    app.spawn_target = None;
    let effort = app.spawn_effort.take();
    app.spawn_variants.clear();
    send(
        writer,
        &Request::Spawn(SpawnRequest {
            kind: Some(kind.clone()),
            cwd: Some(app.active_project.to_string_lossy().to_string()),
            label: None,
            role: Some(role),
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
            budget_usd: None,
        }),
    )?;
    app.status = tr!("已要求啟動 {kind}", kind = kind);
    Ok(())
}

/// 在 Diff 視圖退回目前游標所在的 Hunk 或整個檔案 (`force = false` 時需二次確認)。
pub(crate) fn discard_diff(app: &mut App, writer: &mut HostRouter, force: bool) -> Result<()> {
    if app.view != CenterView::Diff {
        app.status = tr!("請先切換到 Diff 視圖再執行退回");
        return Ok(());
    }
    let Some(session) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return Ok(());
    };
    let Some((file, patch)) = crate::diff::extract_current_hunk_patch(app) else {
        app.status = tr!("目前位置沒有可退回的變更");
        return Ok(());
    };
    let target = (session.clone(), file.clone(), patch.clone());
    if force || app.diff_discard_armed.as_ref() == Some(&target) {
        app.diff_discard_armed = None;
        send(
            writer,
            &Request::WorktreeDiscard {
                session,
                file,
                patch,
            },
        )?;
    } else {
        app.diff_discard_armed = Some(target);
        app.status = if patch.is_some() {
            tr!(
                "⚠ 將退回 {file} 的目前 hunk；再按一次 X 或輸入 :discard! 確認",
                file = file
            )
        } else {
            tr!(
                "⚠ 將還原整個檔案 {file} 的變更；再按一次 X 或輸入 :discard! 確認",
                file = file
            )
        };
    }
    Ok(())
}

pub(crate) fn respond_permission(
    app: &mut App,
    writer: &mut HostRouter,
    decision: Decision,
) -> Result<()> {
    let Some(id) = app.selected_id() else {
        return Ok(());
    };
    let Some(req) = app.selected_permission() else {
        app.status = tr!("此 session 沒有待核准請求");
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
        Decision::Allow => tr!("已允許"),
        Decision::AllowAlways => tr!("已一律允許"),
        Decision::Deny => tr!("已拒絕"),
    };
    Ok(())
}

/// `:model [名稱]`、`:effort [等級]`：對選取的 session 執行期切換；不帶參數只顯示。
fn session_config_command(
    app: &mut App,
    writer: &mut HostRouter,
    verb: &str,
    rest: &str,
) -> Result<()> {
    let Some(id) = app.selected_id() else {
        app.status = tr!("沒有選取的 session");
        return Ok(());
    };
    let Some(sess) = app.state.sessions.get(&id).cloned() else {
        return Ok(());
    };
    if rest.is_empty() {
        app.status = if verb == "model" {
            let choices: Vec<String> = crate::menu::model_choices(&sess.kind)
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(_, v)| v)
                .collect();
            tr!(
                "model：{current}　可選：{choices}",
                current = sess.model.clone().unwrap_or_else(|| tr!("(未回報)")),
                choices = if choices.is_empty() {
                    tr!("(用 :model <名稱> 直接指定)")
                } else {
                    choices.join(", ")
                }
            )
        } else {
            tr!(
                "effort：{current}　可選：{choices}",
                current = sess.effort.clone().unwrap_or_else(|| tr!("(預設)")),
                choices = ertangle_adapter::EFFORT_LEVELS.join(", ")
            )
        };
        return Ok(());
    }
    let value = rest.to_string();
    let (model, effort) = if verb == "model" {
        (Some(value.clone()), None)
    } else {
        (None, Some(value.clone()))
    };
    send(
        writer,
        &Request::SessionConfig {
            session: id,
            model,
            effort,
        },
    )?;
    app.status = tr!(
        "切換 {verb} → {value} (下一回合起生效)",
        verb = verb,
        value = value
    );
    Ok(())
}

/// `:set [--repo] <key> <值>`：把預設值寫進 config.toml。只影響之後新建立的 session，
/// 不動已在跑的；要改執行中的 session 用 `:model`／`:effort`。
fn set_default_command(app: &mut App, rest: &str) {
    let mut words: Vec<&str> = rest.split_whitespace().collect();
    let repo = if let Some(i) = words.iter().position(|w| *w == "--repo") {
        words.remove(i);
        true
    } else {
        false
    };
    let cwd = app
        .selected_cwd()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let path = if repo {
        cwd.join(".ertangle").join("config.toml")
    } else {
        ertangle_daemon::config_dir().join("config.toml")
    };
    let Some(key) = words.first().copied() else {
        let cfg = ertangle_daemon::config::Config::load(&ertangle_daemon::config_dir(), &cwd);
        let agent = app.selected_kind_id();
        let a = agent.as_deref().and_then(|k| cfg.agents.get(k));
        app.status = tr!(
            "預設：agent={agent} permission={permission} model={model} effort={effort} ({target})",
            agent = cfg.defaults.agent.as_deref().unwrap_or("-"),
            permission = cfg.defaults.permission.as_deref().unwrap_or("-"),
            model = a.and_then(|a| a.model.as_deref()).unwrap_or("-"),
            effort = a.and_then(|a| a.effort.as_deref()).unwrap_or("-"),
            target = agent
                .clone()
                .unwrap_or_else(|| tr!("未選 session，model／effort 不顯示")),
        );
        return;
    };
    let value = words.get(1..).unwrap_or(&[]).join(" ");
    let result = match key {
        "agent" | "permission" => {
            if !value.is_empty()
                && key == "permission"
                && !["ask", "auto", "bypass"].contains(&value.as_str())
            {
                app.status = tr!("permission 只能是 ask、auto、bypass");
                return;
            }
            ertangle_daemon::config::Config::set_value(&path, &["defaults"], key, &value)
        }
        "model" | "effort" => {
            let Some(agent) = app.selected_kind_id() else {
                app.status = tr!("model／effort 預設是針對某個 agent；先選取一個 session");
                return;
            };
            if key == "effort" && !value.is_empty() {
                if let Err(e) = ertangle_adapter::check_effort(&value) {
                    app.status = e.to_string();
                    return;
                }
            }
            ertangle_daemon::config::Config::set_value(&path, &["agents", &agent], key, &value)
        }
        "web" => {
            if value.is_empty() || value == "off" {
                let _ = ertangle_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "enabled",
                    Some(ertangle_daemon::config::TomlValue::Boolean(false)),
                );
                ertangle_daemon::config::Config::set_toml(&path, &["web"], "addr", None)
            } else if let Ok(port) = value.parse::<u16>() {
                let _ = ertangle_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "enabled",
                    Some(ertangle_daemon::config::TomlValue::Boolean(true)),
                );
                ertangle_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "port",
                    Some(ertangle_daemon::config::TomlValue::Integer(port as i64)),
                )
            } else {
                let _ = ertangle_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "enabled",
                    Some(ertangle_daemon::config::TomlValue::Boolean(true)),
                );
                ertangle_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "addr",
                    Some(ertangle_daemon::config::TomlValue::String(value.clone())),
                )
            }
        }
        "web-port" | "web_port" => {
            if value.is_empty() {
                ertangle_daemon::config::Config::set_toml(&path, &["web"], "port", None)
            } else if let Ok(port) = value.parse::<u16>() {
                ertangle_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "port",
                    Some(ertangle_daemon::config::TomlValue::Integer(port as i64)),
                )
            } else {
                app.status = tr!("無效的 port 設定：{value}", value = value);
                return;
            }
        }
        _ => {
            app.status = tr!("可設定：agent、permission、model、effort、web、web-port");
            return;
        }
    };
    app.status = match result {
        Ok(()) if value.is_empty() => tr!(
            "已移除 {key} 預設 ({path})",
            key = key,
            path = path.display()
        ),
        Ok(()) => tr!(
            "已將預設 {key} 設為 {value} ({path})，新建立的 session 起生效",
            key = key,
            value = value,
            path = path.display()
        ),
        Err(e) => tr!("寫入設定失敗：{e}", e = e),
    };
}

/// 命令的子命令／旗標候選。提示浮層與 Tab 自動完成共用同一份資料，
/// 說明用 `tr_noop!` 標記，讓 `make i18n-extract` 能收集到翻譯。
const SUBCOMMANDS: &[(&str, &[(&str, &str)])] = &[
    (
        "project",
        &[
            ("here", ertangle_core::tr_noop!("只看本專案")),
            ("all", ertangle_core::tr_noop!("顯示所有專案")),
            ("browse", ertangle_core::tr_noop!("瀏覽資料夾，開啟新專案")),
            (
                "open",
                ertangle_core::tr_noop!("開啟或切換專案；省略路徑開專案清單"),
            ),
            ("set", ertangle_core::tr_noop!("同 open")),
            ("pick", ertangle_core::tr_noop!("開啟專案選擇器")),
        ],
    ),
    (
        "scope",
        &[
            ("here", ertangle_core::tr_noop!("只看本專案")),
            ("all", ertangle_core::tr_noop!("顯示所有專案")),
        ],
    ),
    (
        "layout",
        &[
            (
                "three",
                ertangle_core::tr_noop!("三欄：Sessions／中央／Project"),
            ),
            ("two", ertangle_core::tr_noop!("兩欄")),
            ("tabs", ertangle_core::tr_noop!("分頁")),
        ],
    ),
    (
        "notify",
        &[
            ("on", ertangle_core::tr_noop!("開啟通知")),
            ("off", ertangle_core::tr_noop!("關閉通知")),
            ("test", ertangle_core::tr_noop!("送出測試通知")),
        ],
    ),
    (
        "web",
        &[
            ("on", ertangle_core::tr_noop!("啟動 Web 版")),
            ("off", ertangle_core::tr_noop!("關閉 Web 版")),
            ("status", ertangle_core::tr_noop!("顯示 Web 版狀態")),
            ("qr", ertangle_core::tr_noop!("顯示網址與 QR Code")),
        ],
    ),
    (
        "ended",
        &[
            ("show", ertangle_core::tr_noop!("展開已結束的 session 群組")),
            ("hide", ertangle_core::tr_noop!("摺疊已結束的 session 群組")),
        ],
    ),
    (
        "queue",
        &[("clear", ertangle_core::tr_noop!("清空待送的 prompt 佇列"))],
    ),
    (
        "task-status",
        &[
            ("todo", ertangle_core::tr_noop!("待處理")),
            ("doing", ertangle_core::tr_noop!("進行中")),
            ("blocked", ertangle_core::tr_noop!("阻塞中")),
            ("review", ertangle_core::tr_noop!("審查中")),
            ("done", ertangle_core::tr_noop!("已完成")),
        ],
    ),
    (
        "spawn",
        &[
            ("opencode", ertangle_core::tr_noop!("OpenCode agent")),
            ("claude", ertangle_core::tr_noop!("Claude Code agent")),
            ("codex", ertangle_core::tr_noop!("Codex agent")),
            ("antigravity", ertangle_core::tr_noop!("Antigravity agent")),
        ],
    ),
    (
        "handoff",
        &[
            ("opencode", ertangle_core::tr_noop!("OpenCode agent")),
            ("claude", ertangle_core::tr_noop!("Claude Code agent")),
            ("codex", ertangle_core::tr_noop!("Codex agent")),
            ("antigravity", ertangle_core::tr_noop!("Antigravity agent")),
        ],
    ),
    (
        "rm",
        &[(
            "--force",
            ertangle_core::tr_noop!("強制移除，不檢查 worktree"),
        )],
    ),
    (
        "clean",
        &[(
            "--force",
            ertangle_core::tr_noop!("連未合併或有變更的 worktree 一起清除"),
        )],
    ),
    (
        "merge",
        &[
            (
                "--dry-run",
                ertangle_core::tr_noop!("只看會合併什麼，不真的合併"),
            ),
            (
                "--resolve",
                ertangle_core::tr_noop!("合併並委派 agent 解衝突"),
            ),
            ("--squash", ertangle_core::tr_noop!("使用 squash 策略")),
        ],
    ),
];

fn subcommand_candidates(verb: &str) -> Vec<(String, String)> {
    SUBCOMMANDS
        .iter()
        .find(|(name, _)| *name == verb)
        .map(|(_, items)| {
            items
                .iter()
                .map(|(v, d)| (v.to_string(), ertangle_core::tr_dyn!(d)))
                .collect()
        })
        .unwrap_or_default()
}

/// 路徑參數的候選 (例如 `:project open <路徑>`)：沿用 `@` 附件完成所用的檔案系統讀取。
fn path_candidates(prefix: &str) -> Vec<(String, String)> {
    crate::attach::complete_path(prefix)
        .into_iter()
        .take(20)
        .map(|p| (p, String::new()))
        .collect()
}

/// 命令第二個字以後的候選：` (目前正在打的字, [(值, 說明)])`。只列出以該字開頭的。
/// 支援 `:set`(key 與各 key 的值)、`:model`、`:effort`，以及各命令的子命令／旗標與路徑。
pub(crate) fn arg_candidates(app: &App, input: &str) -> Option<(String, Vec<(String, String)>)> {
    let input = input.trim_start().trim_start_matches(['/', ':']);
    if !input.contains(char::is_whitespace) {
        return None;
    }
    let ends_space = input.ends_with(char::is_whitespace);
    let mut words: Vec<&str> = input.split_whitespace().collect();
    let verb = words.remove(0);
    let cur = if ends_space {
        ""
    } else {
        words.pop().unwrap_or("")
    };
    let kind = app
        .selected_id()
        .and_then(|id| app.state.sessions.get(&id))
        .map(|s| s.kind.clone());
    let models = |kind: &Option<ertangle_core::AgentKind>| -> Vec<(String, String)> {
        kind.as_ref()
            .map(crate::menu::model_choices)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(label, v)| (v, label))
            .collect()
    };
    let efforts = || -> Vec<(String, String)> {
        ertangle_adapter::EFFORT_LEVELS
            .iter()
            .map(|l| (l.to_string(), tr!("推理強度")))
            .collect()
    };
    let list: Vec<(String, String)> = match verb {
        "model" if words.is_empty() => models(&kind),
        "skill" if words.is_empty() => vec![
            (
                "install".into(),
                tr!("安裝到目前專案 (加 global 則裝到家目錄)"),
            ),
            ("uninstall".into(), tr!("移除已安裝的 skill")),
            ("show".into(), tr!("顯示 skill 安裝位置與注入設定")),
        ],
        "skill" if words.len() == 1 => vec![
            ("claude".into(), ".claude/skills".into()),
            ("agents".into(), ".agents/skills".into()),
            ("codex".into(), ".codex/skills".into()),
            ("global".into(), tr!("安裝到家目錄 (所有專案共用)")),
        ],
        "skill" if words.len() == 2 => vec![("global".into(), tr!("安裝到家目錄 (所有專案共用)"))],
        "effort" if words.is_empty() => efforts(),
        "set" => {
            words.retain(|w| *w != "--repo");
            match words.first().copied() {
                None => {
                    let mut v: Vec<(String, String)> = vec![
                        ("agent".into(), tr!("預設的 agent")),
                        ("permission".into(), tr!("預設權限 (ask／auto／bypass)")),
                        ("model".into(), tr!("選取 session 的 agent 的預設 model")),
                        ("effort".into(), tr!("選取 session 的 agent 的預設 effort")),
                        ("web".into(), tr!("Web 版支援 (off／tailscale／位址／port)")),
                        ("web-port".into(), tr!("Web 版自訂連接埠 (例如 7421、8080)")),
                    ];
                    if !input.contains("--repo") {
                        v.push(("--repo".into(), tr!("寫到 .ertangle/config.toml")));
                    }
                    v
                }
                Some("agent") if words.len() == 1 => ["opencode", "claude", "codex", "antigravity"]
                    .iter()
                    .map(|a| (a.to_string(), "agent".to_string()))
                    .collect(),
                Some("permission") if words.len() == 1 => [
                    ("ask", tr!("每個動作都要人同意")),
                    ("auto", tr!("自動核准一般動作")),
                    ("bypass", tr!("完全不問")),
                ]
                .into_iter()
                .map(|(a, d)| (a.to_string(), d))
                .collect(),
                Some("model") if words.len() == 1 => models(&kind),
                Some("effort") if words.len() == 1 => efforts(),
                Some("web") if words.len() == 1 => [
                    ("off", tr!("關閉")),
                    ("tailscale", tr!("Tailscale 自動偵測")),
                    ("127.0.0.1:7421", tr!("本機位址")),
                    ("0.0.0.0:7421", tr!("全網位址")),
                ]
                .into_iter()
                .map(|(a, d)| (a.to_string(), d))
                .collect(),
                Some("web-port") if words.len() == 1 => [
                    ("7421", tr!("預設連接埠")),
                    ("8080", tr!("常用連接埠")),
                    ("8000", tr!("常用連接埠")),
                    ("3000", tr!("常用連接埠")),
                ]
                .into_iter()
                .map(|(a, d)| (a.to_string(), d))
                .collect(),
                _ => Vec::new(),
            }
        }
        "project" if words.len() == 1 => path_candidates(cur),
        "export" => path_candidates(cur),
        _ if words.is_empty() => subcommand_candidates(verb),
        _ => Vec::new(),
    };
    let out: Vec<(String, String)> = list
        .into_iter()
        .filter(|(v, _)| v.starts_with(cur))
        .collect();
    Some((cur.to_string(), out))
}

/// URL query 值的百分比編碼 (只保留 unreserved 字元)。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `:skill [install|uninstall|show] [claude|agents|codex] [global]`：安裝內嵌的 ertangle-ade skill。
fn skill_command(app: &mut App, rest: &str) -> Result<()> {
    let mut words = rest.split_whitespace();
    let action = words.next().unwrap_or("show");
    let mut target = "claude";
    let mut global = false;
    for w in words {
        match w {
            "global" | "--global" => global = true,
            "project" | "--project" => global = false,
            t if ertangle_daemon::skill::target_dir(t).is_some() => target = t,
            _ => bail!(
                "{}",
                tr!("用法：:skill [install|uninstall|show] [claude|agents|codex] [global]")
            ),
        }
    }
    let base = if global {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("找不到家目錄 (HOME)")))?
    } else {
        std::path::PathBuf::from(&app.home_project)
    };
    app.status = skill_action(&base, action, target)?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MergeArgs {
    pub target: Option<String>,
    pub dry_run: bool,
    pub resolve: bool,
    pub strategy: Option<MergeStrategy>,
}

pub(crate) fn parse_merge_args(rest: &str) -> MergeArgs {
    let mut dry_run = false;
    let mut resolve = false;
    let mut strategy = None;
    let mut target = None;

    let mut iter = rest.split_whitespace().peekable();
    while let Some(p) = iter.next() {
        if p == "--dry-run" || p == "-n" || p == "dry-run" {
            dry_run = true;
        } else if p == "--resolve" || p == "-r" || p == "resolve" {
            resolve = true;
        } else if p == "--squash" {
            strategy = Some(MergeStrategy::Squash);
        } else if p == "--rebase" {
            strategy = Some(MergeStrategy::Rebase);
        } else if let Some(val) = p.strip_prefix("--strategy=") {
            strategy = val.parse::<MergeStrategy>().ok();
        } else if p == "--strategy" {
            if let Some(val) = iter.next() {
                strategy = val.parse::<MergeStrategy>().ok();
            }
        } else if target.is_none() {
            target = Some(p.to_string());
        }
    }

    MergeArgs {
        target,
        dry_run,
        resolve,
        strategy,
    }
}

/// 實際的安裝／移除／查詢，回傳狀態列文字 (拆出來方便測試)。
pub(crate) fn skill_action(base: &std::path::Path, action: &str, target: &str) -> Result<String> {
    use ertangle_daemon::skill;
    Ok(match action {
        "install" => {
            let p = skill::install(base, target)?;
            tr!("已安裝 skill：{p}", p = p.display())
        }
        "uninstall" => match skill::uninstall(base, target)? {
            Some(d) => tr!("已移除 skill：{p}", p = d.display()),
            None => tr!("沒有安裝 skill (target {t})", t = target),
        },
        "show" => {
            let path = skill::skill_path(base, target).unwrap_or_default();
            let installed = path.is_file();
            tr!(
                "skill {state}：{p} (注入協作說明可在 :settings 開關)",
                state = if installed {
                    tr!("已安裝")
                } else {
                    tr!("未安裝")
                },
                p = path.display()
            )
        }
        _ => bail!(
            "{}",
            tr!("用法：:skill [install|uninstall|show] [claude|agents|codex] [global]")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_merge_args_defaults() {
        let parsed = parse_merge_args("");
        assert_eq!(
            parsed,
            MergeArgs {
                target: None,
                dry_run: false,
                resolve: false,
                strategy: None,
            }
        );
    }

    #[test]
    fn parse_merge_args_flags_and_target() {
        let parsed = parse_merge_args("ses_1 --dry-run --resolve");
        assert_eq!(
            parsed,
            MergeArgs {
                target: Some("ses_1".into()),
                dry_run: true,
                resolve: true,
                strategy: None,
            }
        );

        let parsed = parse_merge_args("-n -r ses_2");
        assert_eq!(
            parsed,
            MergeArgs {
                target: Some("ses_2".into()),
                dry_run: true,
                resolve: true,
                strategy: None,
            }
        );
    }

    #[test]
    fn parse_merge_args_strategy_options() {
        assert_eq!(
            parse_merge_args("ses_1 --squash").strategy,
            Some(MergeStrategy::Squash)
        );
        assert_eq!(
            parse_merge_args("ses_1 --rebase").strategy,
            Some(MergeStrategy::Rebase)
        );
        assert_eq!(
            parse_merge_args("ses_1 --strategy squash").strategy,
            Some(MergeStrategy::Squash)
        );
        assert_eq!(
            parse_merge_args("ses_1 --strategy=rebase").strategy,
            Some(MergeStrategy::Rebase)
        );
        assert_eq!(
            parse_merge_args("ses_1 --strategy=SQUASH").strategy,
            Some(MergeStrategy::Squash)
        );
        assert_eq!(
            parse_merge_args("ses_1 --strategy merge").strategy,
            Some(MergeStrategy::Merge)
        );
        assert_eq!(parse_merge_args("ses_1 --strategy invalid").strategy, None);
        assert_eq!(parse_merge_args("ses_1 --strategy=invalid").strategy, None);
        assert_eq!(parse_merge_args("ses_1 --strategy").strategy, None);
    }
}
