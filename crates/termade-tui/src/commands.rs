//! TUI 命令面板、跳轉、啟動與核准回覆。

use anyhow::{bail, Result};
use termade_core::{AgentKind, Decision, MsgKind, PermissionPolicy, Role, Target};
use termade_protocol::{Request, SpawnRequest, Transport};
use tr::tr;

use crate::app::{toggle_scope, toggle_thoughts, toggle_tools, App, CenterView, LayoutMode, Mode};
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
        .is_some_and(|s| s.status == termade_core::SessionStatus::Idle)
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

/// `:` 命令列可用的命令：(名稱, 用法與說明)，供提示與 Tab 補全。
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    (
        "spawn",
        termade_core::tr_noop!("[agent[:model] prompt]  新增 agent；不帶參數開選擇器"),
    ),
    ("new", termade_core::tr_noop!("開選擇器：agent → model → prompt")),
    ("help", termade_core::tr_noop!("說明面板：所有按鍵與命令")),
    ("quota", termade_core::tr_noop!("顯示各 agent 帳號的額度與重置時間")),
    (
        "budget",
        termade_core::tr_noop!("<金額>  設定或調高選取 session 的成本上限 (美元)"),
    ),
    (
        "handoff",
        termade_core::tr_noop!("[agent[:model]] [說明]  把選取 session 的工作交接給另一個 agent"),
    ),
    (
        "resume",
        termade_core::tr_noop!("[prompt]  接續選取的已結束 session (接回原對話，session 不變)"),
    ),
    (
        "model",
        termade_core::tr_noop!("[名稱]  切換選取 session 的 model (下一回合起生效)；不帶參數列出目前與可選項"),
    ),
    (
        "effort",
        termade_core::tr_noop!("[low|medium|high|xhigh|max]  切換選取 session 的推理強度；不帶參數顯示目前值"),
    ),
    ("settings", termade_core::tr_noop!("開啟設定面板：用清單切換預設 model、effort、權限、agent 建立 session 的防呆")),
    (
        "set",
        termade_core::tr_noop!("不帶參數開設定面板；[--repo] <agent|permission|model|effort> <值>  直接寫入 config.toml (空值＝刪除)"),
    ),
    ("kill", termade_core::tr_noop!("結束選取的 session")),
    ("cancel", termade_core::tr_noop!("中斷選取 session 的目前回合")),
    ("sleep", termade_core::tr_noop!("休眠選取的 session")),
    ("wake", termade_core::tr_noop!("喚醒選取的 session")),
    ("rm", termade_core::tr_noop!("[session] [--force]  移除已結束的 session")),
    ("clean", termade_core::tr_noop!("清除所有已結束 (且沒有分支)的 session")),
    ("merge", termade_core::tr_noop!("[session] [--dry-run]  合併 worktree 回主工作區")),
    ("diff", termade_core::tr_noop!("顯示選取 session 的 diff")),
    ("gc", termade_core::tr_noop!("[session]  清理已合併的 worktree")),
    ("worktrees", termade_core::tr_noop!("顯示 worktree 清單")),
    ("overview", termade_core::tr_noop!("回到 transcript 視圖")),
    (
        "find",
        termade_core::tr_noop!("[文字]  在逐字稿或 diff 裡搜尋畫面上顯示的內容 (同 f)"),
    ),
    ("layout", termade_core::tr_noop!("[three|two|tabs]  切換版面")),
    ("raw", termade_core::tr_noop!("原始文字／Markdown 切換")),
    ("thoughts", termade_core::tr_noop!("顯示／隱藏思考")),
    ("tools", termade_core::tr_noop!("展開／摺疊工具呼叫")),
    ("scope", termade_core::tr_noop!("[here|all]  只看本專案／顯示全部專案的 session")),
    (
        "notify",
        termade_core::tr_noop!("[on|off|test]  開關通知 (回合結束／等待核准／失敗／額度用完)；不帶參數顯示狀態"),
    ),
    (
        "queue",
        termade_core::tr_noop!("[文字|clear]  排隊 prompt，session 閒下來自動送；不帶參數列出"),
    ),
    ("unqueue", termade_core::tr_noop!("取消最後一則排隊並放回輸入框")),
    ("web", termade_core::tr_noop!("[on|off|status|qr|<addr>]  設定與啟動 Web 版")),
    ("remote", termade_core::tr_noop!("顯示 Web 版網址與連線資訊")),
    ("bus", termade_core::tr_noop!("顯示／隱藏 Bus 浮層")),
    ("task", termade_core::tr_noop!("<標題>  新增任務")),
    ("tasks", termade_core::tr_noop!("任務視圖：依狀態分組，Enter 跳到負責人")),
    (
        "task-status",
        termade_core::tr_noop!("<todo|doing|blocked|review|done>  改任務視圖選取任務的狀態"),
    ),
    ("broadcast", termade_core::tr_noop!("<訊息>  廣播給所有 session")),
    ("copy", termade_core::tr_noop!("複製目前畫面內容")),
    ("quit", termade_core::tr_noop!("離開 (保留 daemon)")),
    (
        "stop",
        termade_core::tr_noop!("結束本專案所有執行中的 session (不動 daemon 與其他專案)"),
    ),
    (
        "shutdown",
        termade_core::tr_noop!("關閉共用 daemon (結束所有專案的 session)並離開；有 session 在跑時需 :shutdown! 確認"),
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
    let mut parts = raw.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match verb.as_str() {
        "help" | "h" | "?" => crate::help::open_help(app),
        "find" => crate::search::open(app, (!rest.is_empty()).then_some(rest)),
        "quota" => {
            let now = termade_core::now_millis();
            app.status = if app.state.quotas.is_empty() {
                tr!("尚無額度資料 (agent 執行過一個回合後才會有)")
            } else {
                app.state
                    .quotas
                    .values()
                    .map(|q| q.summary(now))
                    .collect::<Vec<_>>()
                    .join("   |   ")
            };
        }
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
                limit = termade_core::format_usd_limit(budget_usd)
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
                },
            )?;
            app.status = tr!("已要求交接給 {kind}", kind = kind);
        }
        "spawn" if rest.is_empty() => crate::menu::start_spawn_picker(app),
        "new" => crate::menu::start_spawn_picker(app),
        "spawn" => submit_spawn(app, writer, rest)?,
        "kill" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Kill { session: id })?;
                app.status = tr!("已送出結束");
            }
        }
        "resume" => {
            if let Some(id) = app.selected_id() {
                let prompt = Some(rest.to_string()).filter(|p| !p.is_empty());
                send(
                    writer,
                    &Request::Resume {
                        session: id,
                        prompt,
                    },
                )?;
                app.status = tr!("已要求接續");
            }
        }
        "cancel" => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Cancel { session: id })?;
                app.status = tr!("已送出中斷");
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
        "bus" => {
            app.bus_overlay = !app.bus_overlay;
            app.status = if app.bus_overlay {
                tr!("Bus：顯示")
            } else {
                tr!("Bus：隱藏")
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
        "tasks" => {
            app.view = CenterView::Tasks;
            app.selection = None;
        }
        "task-status" => crate::tasks::set_selected_status(app, writer, rest)?,
        "thoughts" | "think" => toggle_thoughts(app),
        "tools" => toggle_tools(app),
        "scope" | "project" => match rest {
            "here" | "this" => toggle_scope(app, Some(true)),
            "all" => toggle_scope(app, Some(false)),
            "" => toggle_scope(app, None),
            _ => app.status = tr!("用法：:scope [here|all]"),
        },
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
                    label: "termade".into(),
                    text: tr!("termade：測試通知"),
                });
                app.status = tr!("已送出測試通知；{state}", state = app.notify.describe());
            }
            _ => app.status = tr!("用法：:notify [on|off|test]"),
        },
        "queue" => queue_command(app, writer, rest)?,
        "unqueue" => unqueue(app, writer)?,
        "remote" | "web" => {
            let cmd_arg = rest.trim();
            let config_p = termade_daemon::config_dir().join("config.toml");
            let cfg = termade_daemon::config::Config::load_file(&config_p);
            if cmd_arg == "off" {
                send(
                    writer,
                    &Request::WebControl {
                        enabled: Some(false),
                        addr: None,
                    },
                )?;
                let _ = termade_daemon::config::Config::set_toml(
                    &config_p,
                    &["web"],
                    "enabled",
                    Some(termade_daemon::config::TomlValue::Boolean(false)),
                );
                let _ = termade_daemon::config::Config::set_toml(&config_p, &["web"], "addr", None);
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
                let _ = termade_daemon::config::Config::set_toml(
                    &config_p,
                    &["web"],
                    "enabled",
                    Some(termade_daemon::config::TomlValue::Boolean(true)),
                );
                if let Ok(port) = addr.parse::<u16>() {
                    let _ = termade_daemon::config::Config::set_toml(
                        &config_p,
                        &["web"],
                        "port",
                        Some(termade_daemon::config::TomlValue::Integer(port as i64)),
                    );
                } else {
                    let _ = termade_daemon::config::Config::set_toml(
                        &config_p,
                        &["web"],
                        "addr",
                        Some(termade_daemon::config::TomlValue::String(addr)),
                    );
                }
                app.status = tr!("正在啟動 Web 版…");
                return Ok(());
            }

            let token_path = termade_daemon::default_token_path();
            let token = if token_path.exists() {
                termade_daemon::read_token_file(&token_path).ok()
            } else {
                termade_daemon::get_client_token()
            };
            let tailscale_ip = termade_daemon::detect_tailscale_ip()
                .ok()
                .map(|ip| ip.to_string());
            let token_hint = token.as_deref().unwrap_or("");
            let web_port = cfg.web.resolved_port();
            let url = if let Some(ip) = &tailscale_ip {
                format!("http://{ip}:{web_port}/#token={token_hint}")
            } else {
                format!("http://127.0.0.1:{web_port}/#token={token_hint}")
            };

            if cmd_arg == "qr" || cmd_arg == "show" {
                if let Some(qr) = termade_daemon::web::qr::QrCode::encode(&url) {
                    let rendered = qr.render_terminal();
                    let lines: Vec<String> = rendered.lines().map(|s| s.to_string()).collect();
                    app.qr_modal = Some(crate::app::QrModal {
                        title: tr!("Web 版 QR Code"),
                        url: url.clone(),
                        qr_lines: lines,
                    });
                    app.mode = crate::app::Mode::Qr;
                    return Ok(());
                }
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
        "shutdown" | "shutdown!" => {
            // daemon 是所有專案共用的：關掉它會結束每個專案的 agent。有人在跑就先確認。
            let live: Vec<&termade_core::Session> = app.state.live_sessions().collect();
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
        .map(|(n, d)| (n.to_string(), termade_core::tr_dyn!(d)))
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
            kind: kind.clone(),
            cwd: std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string()),
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
            budget_usd: None,
        }),
    )?;
    app.status = tr!("已要求啟動 {kind}", kind = kind);
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
                choices = termade_adapter::EFFORT_LEVELS.join(", ")
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
        cwd.join(".termade").join("config.toml")
    } else {
        termade_daemon::config_dir().join("config.toml")
    };
    let Some(key) = words.first().copied() else {
        let cfg = termade_daemon::config::Config::load(&termade_daemon::config_dir(), &cwd);
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
            termade_daemon::config::Config::set_value(&path, &["defaults"], key, &value)
        }
        "model" | "effort" => {
            let Some(agent) = app.selected_kind_id() else {
                app.status = tr!("model／effort 預設是針對某個 agent；先選取一個 session");
                return;
            };
            if key == "effort" && !value.is_empty() {
                if let Err(e) = termade_adapter::check_effort(&value) {
                    app.status = e.to_string();
                    return;
                }
            }
            termade_daemon::config::Config::set_value(&path, &["agents", &agent], key, &value)
        }
        "web" => {
            if value.is_empty() || value == "off" {
                let _ = termade_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "enabled",
                    Some(termade_daemon::config::TomlValue::Boolean(false)),
                );
                termade_daemon::config::Config::set_toml(&path, &["web"], "addr", None)
            } else if let Ok(port) = value.parse::<u16>() {
                let _ = termade_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "enabled",
                    Some(termade_daemon::config::TomlValue::Boolean(true)),
                );
                termade_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "port",
                    Some(termade_daemon::config::TomlValue::Integer(port as i64)),
                )
            } else {
                let _ = termade_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "enabled",
                    Some(termade_daemon::config::TomlValue::Boolean(true)),
                );
                termade_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "addr",
                    Some(termade_daemon::config::TomlValue::String(value.clone())),
                )
            }
        }
        "web-port" | "web_port" => {
            if value.is_empty() {
                termade_daemon::config::Config::set_toml(&path, &["web"], "port", None)
            } else if let Ok(port) = value.parse::<u16>() {
                termade_daemon::config::Config::set_toml(
                    &path,
                    &["web"],
                    "port",
                    Some(termade_daemon::config::TomlValue::Integer(port as i64)),
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

/// 命令第二個字以後的候選：` (目前正在打的字, [(值, 說明)])`。只列出以該字開頭的。
/// 支援 `:set`(key 與各 key 的值)、`:model`、`:effort`。
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
    let models = |kind: &Option<termade_core::AgentKind>| -> Vec<(String, String)> {
        kind.as_ref()
            .map(crate::menu::model_choices)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(label, v)| (v, label))
            .collect()
    };
    let efforts = || -> Vec<(String, String)> {
        termade_adapter::EFFORT_LEVELS
            .iter()
            .map(|l| (l.to_string(), tr!("推理強度")))
            .collect()
    };
    let list: Vec<(String, String)> = match verb {
        "model" if words.is_empty() => models(&kind),
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
                        v.push(("--repo".into(), tr!("寫到 .termade/config.toml")));
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
        _ => return None,
    };
    let out: Vec<(String, String)> = list
        .into_iter()
        .filter(|(v, _)| v.starts_with(cur))
        .collect();
    Some((cur.to_string(), out))
}
