//! 鍵盤輸入、命令列編輯與剪貼簿貼上。

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ertangle_core::{Decision, MsgKind, PermissionPolicy, Role, SessionStatus, Target};
use ertangle_protocol::Request;
use tr::tr;

use crate::app::{
    scroll_transcript, scroll_transcript_lines, toggle_thoughts, toggle_tools, App, CenterView,
    Mode,
};
use crate::clipboard::copy_main_pane;
use crate::commands::{
    discard_diff, execute_command, execute_jump, respond_permission, submit_prompt_or_route,
    submit_spawn,
};
use crate::diff::{jump_file, jump_hunk, toggle_collapse_current_file};
use crate::hosts::HostRouter;
use crate::{request_selected_diff, send, set_mouse_capture};

pub(crate) fn handle_key(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    match app.mode {
        Mode::Normal => {}
        Mode::Menu => return crate::menu::handle_menu_key(app, writer, key),
        Mode::Help => {
            crate::help::handle_help_key(app, key);
            return Ok(());
        }
        Mode::Settings => {
            crate::settings::handle_key(app, Some(writer), key);
            return Ok(());
        }
        Mode::Ps => return crate::ps::handle_key(app, Some(writer), key),
        Mode::Quota => {
            crate::quota::handle_key(app, key);
            return Ok(());
        }
        Mode::Qr => {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char(' ') => {
                    app.mode = Mode::Normal;
                    app.qr_modal = None;
                }
                _ => {}
            }
            return Ok(());
        }
        Mode::Pick => return crate::menu::handle_pick_key(app, writer, key),
        Mode::Search => {
            crate::search::handle_search_key(app, key);
            return Ok(());
        }
        _ => return handle_input_mode(app, writer, key),
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && handle_ctrl_key(app, key)? {
        return Ok(());
    }

    handle_normal_key(app, writer, key)
}

fn handle_ctrl_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Char('c') => {
            app.should_quit = true;
            Ok(true)
        }
        KeyCode::Char('p') => {
            app.mode = Mode::Command;
            app.input.clear();
            app.input_cursor = 0;
            app.status =
                tr!("命令：spawn / merge / diff / gc / sleep / wake / task / broadcast / copy / shutdown");
            Ok(true)
        }
        KeyCode::Char('k') => {
            app.mode = Mode::Jump;
            app.input.clear();
            app.input_cursor = 0;
            app.status = tr!("輸入關鍵字跳轉至匹配的 session (label / id / branch)");
            Ok(true)
        }
        KeyCode::Char('d') => {
            scroll_transcript(app, 1);
            Ok(true)
        }
        KeyCode::Char('u') => {
            scroll_transcript(app, -1);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Handles keys that only apply inside the Flow view; returns true when consumed.
fn handle_flow_key(app: &mut App, key: KeyEvent) -> bool {
    let step = |app: &mut App, delta: isize| {
        let n = crate::flow::build_flow_model(app, ertangle_core::now_millis())
            .nodes
            .len();
        let cur = app.flow_cursor.min(n.saturating_sub(1));
        app.flow_cursor = cur.saturating_add_signed(delta).min(n.saturating_sub(1));
    };
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => step(app, 1),
        KeyCode::Char('k') | KeyCode::Up => step(app, -1),
        KeyCode::Enter => {
            let model = crate::flow::build_flow_model(app, ertangle_core::now_millis());
            let idx = app.flow_cursor.min(model.nodes.len().saturating_sub(1));
            if let Some(node) = model.nodes.get(idx) {
                app.select_or_pend(&node.id);
            }
            app.view = CenterView::Transcript;
            app.selection = None;
        }
        KeyCode::Char('l') => app.flow_list = !app.flow_list,
        KeyCode::Esc => app.view = app.flow_return,
        KeyCode::Tab => {
            app.selection = None;
            app.view = CenterView::Transcript;
        }
        _ => return false,
    }
    true
}

fn handle_normal_key(app: &mut App, writer: &mut HostRouter, key: KeyEvent) -> Result<()> {
    if app.view == CenterView::Flow && handle_flow_key(app, key) {
        return Ok(());
    }
    if app.sidebar_focus.is_some() {
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => {
                app.activate_sidebar_focus();
                app.sync_active_host_from_selection();
                writer.set_active(&app.active_host);
                if app.view == CenterView::Diff {
                    request_selected_diff(app, writer)?;
                }
                return Ok(());
            }
            KeyCode::Left | KeyCode::Char('h') => {
                app.collapse_sidebar_focus();
                return Ok(());
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Char('q') => {
            app.should_quit = true;
        }
        KeyCode::Char('Q') => execute_command(app, writer, "shutdown")?,
        KeyCode::Char('H') => {
            app.cycle_host_filter();
            writer.set_active(&app.active_host);
        }
        KeyCode::Tab => {
            app.selection = None;
            app.view = match app.view {
                CenterView::Transcript => {
                    request_selected_diff(app, writer)?;
                    CenterView::Diff
                }
                CenterView::Diff => {
                    send(writer, &Request::WorktreeList)?;
                    CenterView::Worktrees
                }
                CenterView::Worktrees => CenterView::Tasks,
                CenterView::Tasks | CenterView::Grid | CenterView::Flow => CenterView::Transcript,
            };
        }
        KeyCode::Char('g') => {
            app.selection = None;
            if app.view == CenterView::Diff {
                app.view = CenterView::Transcript;
            } else {
                app.view = CenterView::Diff;
                request_selected_diff(app, writer)?;
            }
        }
        KeyCode::Down => {
            if app.sidebar_focus.is_some() || app.session_ids().is_empty() {
                app.move_selection(1);
                app.sync_active_host_from_selection();
                writer.set_active(&app.active_host);
            } else {
                match app.view {
                    CenterView::Diff => {
                        app.diff_discard_armed = None;
                        if app.diff_cursor + 1 < app.diff_lines.len() {
                            app.diff_cursor += 1;
                        }
                    }
                    CenterView::Transcript => scroll_transcript_lines(app, 1),
                    CenterView::Tasks => crate::tasks::move_task_cursor(app, 1),
                    CenterView::Worktrees | CenterView::Grid | CenterView::Flow => {}
                }
            }
        }
        KeyCode::Up => {
            if app.sidebar_focus.is_some() || app.session_ids().is_empty() {
                app.move_selection(-1);
                app.sync_active_host_from_selection();
                writer.set_active(&app.active_host);
            } else {
                match app.view {
                    CenterView::Diff => {
                        app.diff_discard_armed = None;
                        app.diff_cursor = app.diff_cursor.saturating_sub(1);
                    }
                    CenterView::Transcript => scroll_transcript_lines(app, -1),
                    CenterView::Tasks => crate::tasks::move_task_cursor(app, -1),
                    CenterView::Worktrees | CenterView::Grid | CenterView::Flow => {}
                }
            }
        }
        KeyCode::Char('j') => {
            if app.view == CenterView::Diff {
                app.diff_discard_armed = None;
                if app.diff_cursor + 1 < app.diff_lines.len() {
                    app.diff_cursor += 1;
                }
            } else if app.view == CenterView::Tasks {
                crate::tasks::move_task_cursor(app, 1);
            } else {
                app.move_selection(1);
                app.sync_active_host_from_selection();
                writer.set_active(&app.active_host);
            }
        }
        KeyCode::Char('k') => {
            if app.view == CenterView::Diff {
                app.diff_discard_armed = None;
                app.diff_cursor = app.diff_cursor.saturating_sub(1);
            } else if app.view == CenterView::Tasks {
                crate::tasks::move_task_cursor(app, -1);
            } else {
                app.move_selection(-1);
                app.sync_active_host_from_selection();
                writer.set_active(&app.active_host);
            }
        }
        KeyCode::Char(']') => {
            if app.selected + 1 < app.session_ids().len() {
                app.selected += 1;
                app.sync_active_host_from_selection();
                writer.set_active(&app.active_host);
                if app.view == CenterView::Diff {
                    request_selected_diff(app, writer)?;
                }
            }
        }
        KeyCode::Char('[') => {
            app.selected = app.selected.saturating_sub(1);
            app.sync_active_host_from_selection();
            writer.set_active(&app.active_host);
            if app.view == CenterView::Diff {
                request_selected_diff(app, writer)?;
            }
        }
        KeyCode::Char('c') if app.view == CenterView::Diff => {
            app.mode = Mode::Review;
            app.input.clear();
            app.input_cursor = 0;
            let loc = app.current_diff_location();
            app.status = tr!("對 `{loc}` 留下審查註解 (Enter 送回 agent)", loc = loc);
        }
        KeyCode::Char('M') => {
            if let Some(id) = app.selected_id() {
                send(
                    writer,
                    &Request::WorktreeMerge {
                        session: id,
                        dry_run: false,
                        resolve: false,
                        strategy: None,
                    },
                )?;
            }
        }
        KeyCode::Char('G') => {
            send(
                writer,
                &Request::WorktreeGc {
                    session: None,
                    force: false,
                },
            )?;
        }
        KeyCode::Char('z') => {
            if let Some(id) = app.selected_id() {
                let is_sleeping = app
                    .state
                    .sessions
                    .get(&id)
                    .is_some_and(|s| s.status == SessionStatus::Sleeping);
                if is_sleeping {
                    send(writer, &Request::Wake { session: id })?;
                } else {
                    send(writer, &Request::Sleep { session: id })?;
                }
            }
        }
        KeyCode::Char(' ') => crate::menu::open_menu(app),
        KeyCode::Char('?') => crate::help::open_help(app),
        KeyCode::Char('/') | KeyCode::Char(':') => {
            app.mode = Mode::Command;
            app.input.clear();
            app.input_cursor = 0;
            app.status = tr!("命令 (Tab 自動完成)：spawn rm clean kill merge diff gc task …");
        }
        KeyCode::Char('!') => {
            app.mode = Mode::Prompt;
            app.input = "!".into();
            app.input_cursor = 1;
            app.status = tr!("在選取 session 的工作目錄執行 shell 命令 (!<命令>)");
        }
        KeyCode::Enter if app.view == CenterView::Grid => app.view = CenterView::Transcript,
        KeyCode::Enter if app.view == CenterView::Tasks => crate::tasks::jump_to_assignee(app),
        KeyCode::Enter | KeyCode::Char('v') if app.view == CenterView::Diff => {
            toggle_collapse_current_file(app)
        }
        KeyCode::Char('X') if app.view == CenterView::Diff => discard_diff(app, writer, false)?,
        KeyCode::Enter | KeyCode::Char('i') => {
            if let Some(q) = app.selected_question() {
                crate::menu::start_question_picker(app, &q);
            } else {
                app.start_prompt();
            }
        }
        KeyCode::Char('w') => {
            app.jump_to_pending();
        }
        KeyCode::Char('n') => crate::menu::start_spawn_picker(app),
        KeyCode::Char('f') => crate::search::open(app, None),
        KeyCode::Char('m') => {
            app.mode = Mode::Message;
            app.input.clear();
            app.input_cursor = 0;
            app.status = tr!("輸入要送給選取 session 的訊息");
        }
        KeyCode::Char('a') => respond_permission(app, writer, Decision::Allow)?,
        KeyCode::Char('A') => respond_permission(app, writer, Decision::AllowAlways)?,
        KeyCode::Char('d') => respond_permission(app, writer, Decision::Deny)?,
        KeyCode::Char('x') => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Cancel { session: id })?;
                app.status = tr!("已送出中斷");
            }
        }
        KeyCode::Char('K') => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Kill { session: id })?;
                app.status = tr!("已送出結束");
            }
        }
        KeyCode::Char('r') => {
            send(writer, &Request::Snapshot)?;
            if app.view == CenterView::Diff {
                request_selected_diff(app, writer)?;
            } else if app.view == CenterView::Worktrees {
                send(writer, &Request::WorktreeList)?;
            }
            app.status = tr!("已要求重整");
        }
        KeyCode::Char('t') => {
            app.raw_md = !app.raw_md;
            app.md_cache = None;
            app.selection = None;
            app.status = if app.raw_md {
                tr!("逐字稿顯示：原始文字")
            } else {
                tr!("逐字稿顯示：Markdown 渲染")
            };
        }
        KeyCode::F(2) => {
            app.mouse_capture = !app.mouse_capture;
            set_mouse_capture(app.mouse_capture)?;
            app.status = if app.mouse_capture {
                tr!("滑鼠擷取：開 (可拖曳選取中央區塊並自動複製；Shift+拖曳可原生選取)")
            } else {
                tr!("滑鼠擷取：關 (可直接拖曳選取複製；滾輪用終端機 alternate scroll)")
            };
        }
        KeyCode::Char('T') => toggle_thoughts(app),
        KeyCode::Char('P') => crate::app::toggle_scope(app, None),
        KeyCode::Char('E') => crate::app::toggle_ended(app, None),
        KeyCode::Char('O') => toggle_tools(app),
        KeyCode::Char('L') => app.cycle_layout(),
        KeyCode::Char('b') => app.toggle_bus(),
        KeyCode::Char('F') => crate::app::toggle_flow(app),
        KeyCode::Char('V') => crate::project::toggle_grid(app),
        KeyCode::Esc => {
            app.selection = None;
            app.sidebar_focus = None;
            app.bus_overlay = false;
            app.diff_discard_armed = None;
            app.status = tr!("已清除選取");
        }
        KeyCode::Char('Y') => copy_main_pane(app, None),
        KeyCode::PageUp => scroll_transcript(app, -1),
        KeyCode::PageDown => scroll_transcript(app, 1),
        KeyCode::Home => {
            if app.view == CenterView::Diff {
                app.diff_discard_armed = None;
                app.diff_cursor = 0;
            } else if app.view == CenterView::Tasks {
                app.task_cursor = 0;
            } else {
                app.transcript_scroll = Some(0);
                app.status = tr!("逐字稿：已到最前");
            }
        }
        KeyCode::End => {
            if app.view == CenterView::Diff {
                app.diff_discard_armed = None;
                app.diff_cursor = app.diff_lines.len().saturating_sub(1);
            } else if app.view == CenterView::Tasks {
                crate::tasks::move_task_cursor(app, i32::MAX);
            } else {
                app.transcript_scroll = None;
                app.status = tr!("逐字稿：跟隨最新");
            }
        }
        KeyCode::Char('{') => jump_hunk(app, false),
        KeyCode::Char('}') => jump_hunk(app, true),
        KeyCode::Char('(') | KeyCode::Char('<') => jump_file(app, false),
        KeyCode::Char(')') | KeyCode::Char('>') => jump_file(app, true),
        // `Normal` 模式誤打字：直接提示要先進輸入模式，而不是靜靜地沒有反應。
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                && !c.is_control() =>
        {
            app.nudge_input();
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn handle_input_mode(
    app: &mut App,
    writer: &mut HostRouter,
    key: KeyEvent,
) -> Result<()> {
    // 任何按鍵都會讓滑鼠選取失效 (字元索引已經改變)。
    app.input_sel = None;
    // `@` 檔案建議：查詢改變就把選取索引歸零；上下鍵／Ctrl-N／Ctrl-P 在建議間移動，Tab 採用。
    if let Some((start, cands)) = at_candidates(app) {
        let tok = at_token_text(app);
        if app.at_sel.0 != tok {
            app.at_sel = (tok, 0);
        }
        let n = cands.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Down => {
                app.at_sel.1 = (app.at_sel.1 + 1) % n;
                return Ok(());
            }
            KeyCode::Up => {
                app.at_sel.1 = (app.at_sel.1 + n - 1) % n;
                return Ok(());
            }
            KeyCode::Char('n') if ctrl => {
                app.at_sel.1 = (app.at_sel.1 + 1) % n;
                return Ok(());
            }
            KeyCode::Char('p') if ctrl => {
                app.at_sel.1 = (app.at_sel.1 + n - 1) % n;
                return Ok(());
            }
            KeyCode::Tab => {
                accept_at_candidate(app, start, &cands[app.at_sel.1.min(n - 1)]);
                return Ok(());
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.prompt_history.reset();
            app.command_history.reset();
            app.input.clear();
            app.input_cursor = 0;
            app.spawn_target = None;
            app.spawn_effort = None;
            app.spawn_variants.clear();
            app.pending_prompt_draft = None;
            app.pending_spawn_attachments.clear();
        }
        KeyCode::Char('e')
            if key.modifiers.contains(KeyModifiers::CONTROL) && app.mode == Mode::Spawn =>
        {
            app.spawn_permission = match app.spawn_permission {
                None => Some(PermissionPolicy::Ask),
                Some(PermissionPolicy::Ask) => Some(PermissionPolicy::Auto),
                Some(PermissionPolicy::Auto) => Some(PermissionPolicy::Bypass),
                Some(PermissionPolicy::Bypass) => None,
            };
            app.spawn_bypass_armed = false;
            app.status = tr!(
                "權限 = {permission}",
                permission = app
                    .spawn_permission
                    .map_or_else(|| tr!("預設(config)"), |p| p.id().to_string())
            );
        }
        KeyCode::Char('f')
            if key.modifiers.contains(KeyModifiers::CONTROL) && app.mode == Mode::Spawn =>
        {
            // 預設不指定 effort；Ctrl-F 依序切換 預設 → 各等級 → 預設。
            let levels: Vec<String> = if app.spawn_variants.is_empty() {
                ertangle_adapter::EFFORT_LEVELS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                ertangle_adapter::EFFORT_LEVELS
                    .iter()
                    .filter(|l| app.spawn_variants.iter().any(|v| v == *l))
                    .map(|s| s.to_string())
                    .collect()
            };
            app.spawn_effort = match &app.spawn_effort {
                None => levels.first().cloned(),
                Some(cur) => levels
                    .iter()
                    .position(|l| l == cur)
                    .and_then(|i| levels.get(i + 1).cloned()),
            };
            app.status = tr!(
                "effort = {effort}",
                effort = app.spawn_effort.clone().unwrap_or_else(|| tr!("預設"))
            );
        }
        KeyCode::Char('r')
            if key.modifiers.contains(KeyModifiers::CONTROL) && app.mode == Mode::Spawn =>
        {
            app.spawn_role = match app.spawn_role {
                Role::Worker => Role::Coordinator,
                Role::Coordinator => Role::Reviewer,
                _ => Role::Worker,
            };
            app.status = tr!("角色 = {role}", role = app.spawn_role.id());
        }
        KeyCode::Tab if app.mode == Mode::Command => complete_command(app),
        // 輸入框以 `/` 開頭時送出會當成命令 (`submit_prompt_or_route`)，Tab 也照命令自動完成。
        KeyCode::Tab if app.mode == Mode::Prompt && app.input.trim_start().starts_with('/') => {
            complete_command(app)
        }
        KeyCode::Tab if matches!(app.mode, Mode::Prompt | Mode::Spawn | Mode::Message) => {
            complete_attachment_path(app)
        }
        KeyCode::Char('v')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(app.mode, Mode::Prompt | Mode::Spawn | Mode::Message) =>
        {
            match crate::attach::clipboard_image() {
                Ok(path) => {
                    insert_str_input(app, &format!("@{} ", path.display()));
                    app.status = tr!("已貼上剪貼簿圖片");
                }
                Err(e) => app.status = e,
            }
        }
        KeyCode::Up | KeyCode::Down if matches!(app.mode, Mode::Prompt | Mode::Command) => {
            let up = key.code == KeyCode::Up;
            if !move_cursor_row(app, up) {
                recall_history(app, up);
            }
        }
        KeyCode::Enter
            if is_multiline(app.mode)
                && (key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::CONTROL)) =>
        {
            insert_input(app, '\n');
        }
        KeyCode::Char('j')
            if key.modifiers.contains(KeyModifiers::CONTROL) && is_multiline(app.mode) =>
        {
            insert_input(app, '\n');
        }
        KeyCode::Char(c @ ('p' | 'n'))
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(app.mode, Mode::Prompt | Mode::Command) =>
        {
            recall_history(app, c == 'p');
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::SUPER) => {
            delete_to_line_start(app);
        }
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::ALT)
                || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            delete_word_back(app);
        }
        KeyCode::Backspace => backspace_input(app),
        KeyCode::Delete
            if key.modifiers.contains(KeyModifiers::ALT)
                || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            delete_word_forward(app);
        }
        KeyCode::Delete => delete_input(app),
        KeyCode::Left if key.modifiers.contains(KeyModifiers::SUPER) => {
            app.input_cursor = crate::inputbox::line_bounds(&app.input, app.input_cursor).0;
        }
        KeyCode::Left
            if key.modifiers.contains(KeyModifiers::ALT)
                || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            move_word_left(app);
        }
        KeyCode::Left => app.input_cursor = app.input_cursor.saturating_sub(1),
        KeyCode::Right if key.modifiers.contains(KeyModifiers::SUPER) => {
            app.input_cursor = crate::inputbox::line_bounds(&app.input, app.input_cursor).1;
        }
        KeyCode::Right
            if key.modifiers.contains(KeyModifiers::ALT)
                || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            move_word_right(app);
        }
        KeyCode::Right => {
            let n = app.input.chars().count();
            app.input_cursor = (app.input_cursor + 1).min(n);
        }
        KeyCode::Home => {
            app.input_cursor = crate::inputbox::line_bounds(&app.input, app.input_cursor).0
        }
        KeyCode::End => {
            app.input_cursor = crate::inputbox::line_bounds(&app.input, app.input_cursor).1
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input_cursor = crate::inputbox::line_bounds(&app.input, app.input_cursor).0;
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input_cursor = crate::inputbox::line_bounds(&app.input, app.input_cursor).1;
        }
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
            move_word_left(app);
        }
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
            move_word_right(app);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
            delete_word_forward(app);
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            delete_word_back(app);
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            delete_to_line_end(app);
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            delete_to_line_start(app);
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            insert_input(app, c);
        }
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            app.input_cursor = 0;
            let mode = app.mode;
            app.mode = Mode::Normal;
            let queue = key.modifiers.contains(KeyModifiers::ALT);
            match mode {
                Mode::Prompt => {
                    app.prompt_history.push(&text);
                    if queue {
                        crate::commands::queue_prompt(app, writer, &text)?;
                    } else {
                        submit_prompt_or_route(app, writer, &text)?;
                    }
                }
                Mode::Spawn => submit_spawn(app, writer, &text)?,
                Mode::Message => {
                    if let Some(id) = app.selected_id() {
                        send(
                            writer,
                            &Request::BusSend {
                                from: None,
                                to: Target::Direct(id),
                                kind: MsgKind::Chat,
                                body: text,
                                reply_to: None,
                            },
                        )?;
                        app.status = tr!("已送出訊息");
                    }
                }
                Mode::Review => {
                    if let Some(id) = app.selected_id() {
                        let location = app.current_diff_location();
                        send(
                            writer,
                            &Request::ReviewComment {
                                session: id,
                                location,
                                comment: text,
                            },
                        )?;
                    }
                }
                Mode::Command => {
                    app.command_history.push(&text);
                    execute_command(app, writer, &text)?;
                }
                Mode::Jump => execute_jump(app, writer, &text)?,
                Mode::Normal
                | Mode::Menu
                | Mode::Pick
                | Mode::Help
                | Mode::Search
                | Mode::Settings
                | Mode::Qr
                | Mode::Quota
                | Mode::Ps => {}
            }
        }
        _ => {}
    }
    Ok(())
}

/// 目前 `@token` 的內容 (不含 `@`)。
fn at_token_text(app: &App) -> String {
    crate::attach::current_at_token(&app.input, app.input_cursor)
        .map(|(_, t)| t)
        .unwrap_or_default()
}

/// 在 Prompt／Spawn／Message 輸入 `@xxx` 時的專案內模糊搜尋建議 (相對路徑或 `~`、`/` 開頭的字面路徑不搜尋)。
pub(crate) fn at_candidates(app: &App) -> Option<(usize, Vec<String>)> {
    if !matches!(app.mode, Mode::Prompt | Mode::Spawn | Mode::Message) {
        return None;
    }
    let (start, tok) = crate::attach::current_at_token(&app.input, app.input_cursor)?;
    if tok.is_empty() || tok.starts_with(['/', '~', '"']) {
        return None;
    }
    let c = crate::attach::suggestions(&app.search_root(), &tok, 8);
    (!c.is_empty()).then_some((start, c))
}

/// 用選取的建議取代游標前的 `@token` (路徑含空白時加引號)，後面補一個空白。
fn accept_at_candidate(app: &mut App, start: usize, path: &str) {
    let rep = if path.contains(char::is_whitespace) {
        format!("@\"{path}\" ")
    } else {
        format!("@{path} ")
    };
    let s = byte_index(&app.input, start);
    let e = byte_index(&app.input, app.input_cursor);
    app.input.replace_range(s..e, &rep);
    app.input_cursor = start + rep.chars().count();
    app.status = tr!("已選 {path}", path = path);
}

/// 游標前的 `@路徑` 做 Tab 自動完成：唯一符合就補完，多個則補到共同前綴並列出候選。
fn complete_attachment_path(app: &mut App) {
    let chars: Vec<char> = app.input.chars().collect();
    let cur = app.input_cursor.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let Some(at) = before.rfind('@') else { return };
    if at > 0 && !before[..at].ends_with(char::is_whitespace) {
        return;
    }
    let prefix = &before[at + 1..];
    if prefix.contains(char::is_whitespace) {
        return;
    }
    let cands = crate::attach::complete_path(prefix);
    if cands.is_empty() {
        app.status = tr!("沒有符合的路徑");
        return;
    }
    let common = crate::attach::common_prefix(&cands);
    let new_prefix = if cands.len() == 1 {
        cands[0].clone()
    } else {
        common
    };
    if new_prefix.len() > prefix.len() {
        let start = byte_index(&app.input, before[..at + 1].chars().count());
        let end = byte_index(&app.input, cur);
        app.input.replace_range(start..end, &new_prefix);
        app.input_cursor = before[..at + 1].chars().count() + new_prefix.chars().count();
    }
    if cands.len() > 1 {
        let shown: Vec<&str> = cands.iter().take(8).map(String::as_str).collect();
        app.status = tr!(
            "候選：{list}",
            list = format!(
                "{}{}",
                shown.join("  "),
                if cands.len() > 8 { " …" } else { "" }
            )
        );
    }
}

/// `:` 命令列的 Tab 自動完成：輸入的第一個字若只符合一個命令就補完；多個則補到共同前綴並列出候選。
#[cfg(test)]
pub(crate) fn complete_command_for_test(app: &mut App) {
    complete_command(app)
}

fn complete_command(app: &mut App) {
    // 和提示浮層 (`hints_with_native`)一致：忽略前導空白與 `/`、`:` 前綴，
    // 候選也包含選取 session 的 agent 原生指令。前綴原樣保留。
    let body = app.input.trim_start().trim_start_matches(['/', ':']);
    if body.contains(char::is_whitespace) {
        complete_arg(app);
        return;
    }
    let lead = app.input[..app.input.len() - body.len()].to_string();
    let cands: Vec<String> = crate::commands::hints_with_native(app)
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| n.starts_with(body))
        .collect();
    match cands.len() {
        0 => {}
        1 => {
            app.input = format!("{lead}{} ", cands[0]);
            app.input_cursor = app.input.chars().count();
        }
        _ => {
            app.input = format!("{lead}{}", crate::attach::common_prefix(&cands));
            app.input_cursor = app.input.chars().count();
            app.status = tr!("候選：{list}", list = cands.join(" "));
        }
    }
}

/// 命令參數的 Tab 自動完成：唯一候選就補完 (後面補空白，若還有下一個參數可接著打)，多個補到共同前綴並列出。
fn complete_arg(app: &mut App) {
    let Some((cur, cands)) = crate::commands::arg_candidates(app, &app.input) else {
        return;
    };
    let names: Vec<&str> = cands.iter().map(|(v, _)| v.as_str()).collect();
    let Some(first) = names.first().copied() else {
        return;
    };
    // 用會檢查字元邊界的 common_prefix，避免中文路徑被從 UTF-8 位元組中間截斷。
    let owned: Vec<String> = names.iter().map(|v| (*v).to_string()).collect();
    let common = crate::attach::common_prefix(&owned);
    let head_len = app.input.len() - cur.len();
    let mut next = app.input[..head_len].to_string();
    if names.len() == 1 {
        next.push_str(first);
        next.push(' ');
    } else {
        next.push_str(&common);
        app.status = format!("候選：{}", names.join(" "));
    }
    app.input = next;
    app.input_cursor = app.input.chars().count();
}

/// 以 Unicode 字元索引取得對應的 byte offset (超出長度時回傳字串長度)。
pub(crate) fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

pub(crate) fn insert_input(app: &mut App, c: char) {
    let b = byte_index(&app.input, app.input_cursor);
    app.input.insert(b, c);
    app.input_cursor += 1;
}

pub(crate) fn insert_str_input(app: &mut App, s: &str) {
    if s.is_empty() {
        return;
    }
    let b = byte_index(&app.input, app.input_cursor);
    app.input.insert_str(b, s);
    app.input_cursor += s.chars().count();
}

pub(crate) fn backspace_input(app: &mut App) {
    if app.input_cursor == 0 {
        return;
    }
    let b = byte_index(&app.input, app.input_cursor - 1);
    app.input.remove(b);
    app.input_cursor -= 1;
}

pub(crate) fn delete_input(app: &mut App) {
    let n = app.input.chars().count();
    if app.input_cursor >= n {
        return;
    }
    let b = byte_index(&app.input, app.input_cursor);
    app.input.remove(b);
}

/// Previous word boundary (char index) before `cur`: skip spaces/newlines, then the word.
fn prev_word_start(chars: &[char], cur: usize) -> usize {
    let mut i = cur.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// Next word boundary (char index) after `cur`: skip spaces/newlines, then the word.
fn next_word_end(chars: &[char], cur: usize) -> usize {
    let mut i = cur.min(chars.len());
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    i
}

pub(crate) fn move_word_left(app: &mut App) {
    let chars: Vec<char> = app.input.chars().collect();
    app.input_cursor = prev_word_start(&chars, app.input_cursor);
}

pub(crate) fn move_word_right(app: &mut App) {
    let chars: Vec<char> = app.input.chars().collect();
    app.input_cursor = next_word_end(&chars, app.input_cursor);
}

pub(crate) fn delete_word_back(app: &mut App) {
    let chars: Vec<char> = app.input.chars().collect();
    let start = prev_word_start(&chars, app.input_cursor);
    if app.input_cursor > start {
        let b_start = byte_index(&app.input, start);
        let b_end = byte_index(&app.input, app.input_cursor);
        app.input.drain(b_start..b_end);
        app.input_cursor = start;
    }
}

pub(crate) fn delete_word_forward(app: &mut App) {
    let chars: Vec<char> = app.input.chars().collect();
    let end = next_word_end(&chars, app.input_cursor);
    if end > app.input_cursor {
        let b_start = byte_index(&app.input, app.input_cursor);
        let b_end = byte_index(&app.input, end);
        app.input.drain(b_start..b_end);
    }
}

pub(crate) fn delete_to_line_start(app: &mut App) {
    let (start, _) = crate::inputbox::line_bounds(&app.input, app.input_cursor);
    if app.input_cursor > start {
        let b_start = byte_index(&app.input, start);
        let b_end = byte_index(&app.input, app.input_cursor);
        app.input.drain(b_start..b_end);
        app.input_cursor = start;
    }
}

pub(crate) fn delete_to_line_end(app: &mut App) {
    let (_, end) = crate::inputbox::line_bounds(&app.input, app.input_cursor);
    if end > app.input_cursor {
        let b_start = byte_index(&app.input, app.input_cursor);
        let b_end = byte_index(&app.input, end);
        app.input.drain(b_start..b_end);
    } else {
        let total = app.input.chars().count();
        if app.input_cursor < total {
            delete_input(app);
        }
    }
}

/// 貼上 (bracketed paste)：只插入目前輸入框；換行轉成空白以維持單行輸入。
pub(crate) fn handle_paste(app: &mut App, text: &str) {
    if app.mode == Mode::Pick && crate::menu::paste_custom(app, text) {
        return;
    }
    // `Normal` 模式貼上文字時直接切進輸入模式，否則內容會插進看不見的輸入框，
    // 使用者會以為貼上沒反應 (而且下次按 i 會清空)。
    if app.mode == Mode::Normal && !text.trim().is_empty() {
        app.start_prompt();
        app.status = tr!("已切換到輸入模式 (Enter 送出 · Esc 取消)");
    }
    // 拖曳檔案進終端機會貼上它的路徑：轉成 `@path` 附件引用。
    if matches!(app.mode, Mode::Prompt | Mode::Spawn | Mode::Message) {
        if let Some(r) = crate::attach::pasted_file_ref(text) {
            insert_str_input(app, &r);
            app.status = "已加入附件引用 (送出時一併附上)".into();
            return;
        }
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let sanitized = if is_multiline(app.mode) {
        normalized
    } else {
        normalized.replace('\n', " ")
    };
    insert_str_input(app, &sanitized);
    if app.mode == Mode::Search {
        crate::search::query_changed(app);
    }
}

/// ↑／↓ 回顧輸入歷史 (Prompt 與 Command 各有自己的一份)。
pub(crate) fn recall_history(app: &mut App, older: bool) {
    let cur = app.input.clone();
    let hist = if app.mode == Mode::Command {
        &mut app.command_history
    } else {
        &mut app.prompt_history
    };
    let got = if older { hist.prev(&cur) } else { hist.next() };
    if let Some(t) = got {
        app.input_cursor = t.chars().count();
        app.input = t;
    }
}

/// 這些模式的輸入框允許多行 (Ctrl-J／Shift-Enter 換行、貼上保留換行)。
pub(crate) fn is_multiline(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Prompt | Mode::Spawn | Mode::Message | Mode::Review
    )
}

/// 在多行內容裡上下移動游標；已在最上／最下一列 (或只有一列)回傳 `false`，交給歷史處理。
pub(crate) fn move_cursor_row(app: &mut App, up: bool) -> bool {
    if !is_multiline(app.mode) {
        return false;
    }
    let rows = crate::inputbox::rows(&app.input, app.input_inner_w);
    let (r, col) = crate::inputbox::cursor_pos(&app.input, &rows, app.input_cursor);
    let target = if up {
        match r.checked_sub(1) {
            Some(t) => t,
            None => return false,
        }
    } else if r + 1 < rows.len() {
        r + 1
    } else {
        return false;
    };
    app.input_cursor = crate::inputbox::index_at(&app.input, &rows, target, col);
    true
}
