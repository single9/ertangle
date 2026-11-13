//! 鍵盤輸入、命令列編輯與剪貼簿貼上。

use ade_core::{Decision, MsgKind, PermissionPolicy, SessionStatus, Target};
use ade_daemon::ConnStream;
use ade_protocol::Request;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{
    scroll_transcript, scroll_transcript_lines, toggle_thoughts, App, CenterView, Mode,
};
use crate::clipboard::copy_main_pane;
use crate::commands::{
    execute_command, execute_jump, respond_permission, submit_prompt_or_route, submit_spawn,
};
use crate::diff::jump_hunk;
use crate::{request_selected_diff, send, set_mouse_capture};

pub(crate) fn handle_key(app: &mut App, writer: &mut ConnStream, key: KeyEvent) -> Result<()> {
    match app.mode {
        Mode::Normal => {}
        Mode::Menu => return crate::menu::handle_menu_key(app, writer, key),
        Mode::Pick => return crate::menu::handle_pick_key(app, key),
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
                "命令：spawn / merge / diff / gc / sleep / wake / task / broadcast / copy / shutdown"
                    .into();
            Ok(true)
        }
        KeyCode::Char('k') => {
            app.mode = Mode::Jump;
            app.input.clear();
            app.input_cursor = 0;
            app.status = "輸入關鍵字跳轉至匹配的 session（label / id / branch）".into();
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

fn handle_normal_key(app: &mut App, writer: &mut ConnStream, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('Q') => {
            send(writer, &Request::Shutdown)?;
            app.status = "已關閉 daemon（所有 agent 已結束）".into();
            app.should_quit = true;
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
                CenterView::Worktrees => CenterView::Transcript,
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
        KeyCode::Down => match app.view {
            CenterView::Diff => {
                if app.diff_cursor + 1 < app.diff_lines.len() {
                    app.diff_cursor += 1;
                }
            }
            CenterView::Transcript => scroll_transcript_lines(app, 1),
            CenterView::Worktrees => {}
        },
        KeyCode::Up => match app.view {
            CenterView::Diff => app.diff_cursor = app.diff_cursor.saturating_sub(1),
            CenterView::Transcript => scroll_transcript_lines(app, -1),
            CenterView::Worktrees => {}
        },
        KeyCode::Char('j') => {
            if app.view == CenterView::Diff {
                if app.diff_cursor + 1 < app.diff_lines.len() {
                    app.diff_cursor += 1;
                }
            } else if app.selected + 1 < app.session_ids().len() {
                app.selected += 1;
            }
        }
        KeyCode::Char('k') => {
            if app.view == CenterView::Diff {
                app.diff_cursor = app.diff_cursor.saturating_sub(1);
            } else {
                app.selected = app.selected.saturating_sub(1);
            }
        }
        KeyCode::Char(']') => {
            if app.selected + 1 < app.session_ids().len() {
                app.selected += 1;
                if app.view == CenterView::Diff {
                    request_selected_diff(app, writer)?;
                }
            }
        }
        KeyCode::Char('[') => {
            app.selected = app.selected.saturating_sub(1);
            if app.view == CenterView::Diff {
                request_selected_diff(app, writer)?;
            }
        }
        KeyCode::Char('c') if app.view == CenterView::Diff => {
            app.mode = Mode::Review;
            app.input.clear();
            app.input_cursor = 0;
            let loc = app.current_diff_location();
            app.status = format!("對 `{loc}` 留下審查註解（Enter 送回 agent）");
        }
        KeyCode::Char('M') => {
            if let Some(id) = app.selected_id() {
                send(
                    writer,
                    &Request::WorktreeMerge {
                        session: id,
                        dry_run: false,
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
        KeyCode::Char('/') | KeyCode::Char(':') => {
            app.mode = Mode::Command;
            app.input.clear();
            app.input_cursor = 0;
            app.status = "命令（Tab 補全）：spawn rm clean kill merge diff gc task …".into();
        }
        KeyCode::Enter | KeyCode::Char('i') => {
            app.mode = Mode::Prompt;
            app.input.clear();
            app.input_cursor = 0;
        }
        KeyCode::Char('n') => crate::menu::start_spawn_picker(app),
        KeyCode::Char('m') => {
            app.mode = Mode::Message;
            app.input.clear();
            app.input_cursor = 0;
            app.status = "輸入要送給選取 session 的訊息".into();
        }
        KeyCode::Char('a') => respond_permission(app, writer, Decision::Allow)?,
        KeyCode::Char('A') => respond_permission(app, writer, Decision::AllowAlways)?,
        KeyCode::Char('d') => respond_permission(app, writer, Decision::Deny)?,
        KeyCode::Char('x') => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Cancel { session: id })?;
                app.status = "已送出中斷".into();
            }
        }
        KeyCode::Char('K') => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::Kill { session: id })?;
                app.status = "已送出結束".into();
            }
        }
        KeyCode::Char('r') => {
            send(writer, &Request::Snapshot)?;
            if app.view == CenterView::Diff {
                request_selected_diff(app, writer)?;
            } else if app.view == CenterView::Worktrees {
                send(writer, &Request::WorktreeList)?;
            }
            app.status = "已要求重整".into();
        }
        KeyCode::Char('t') => {
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
        KeyCode::F(2) => {
            app.mouse_capture = !app.mouse_capture;
            set_mouse_capture(app.mouse_capture)?;
            app.status = if app.mouse_capture {
                "滑鼠擷取：開（可拖曳選取中央區塊並自動複製；Shift+拖曳可原生選取）".into()
            } else {
                "滑鼠擷取：關（可直接拖曳選取複製；滾輪用終端 alternate scroll）".into()
            };
        }
        KeyCode::Char('T') => toggle_thoughts(app),
        KeyCode::Char('L') => app.cycle_layout(),
        KeyCode::Char('b') => {
            app.bus_overlay = !app.bus_overlay;
            app.status = if app.bus_overlay {
                "Bus：顯示（b 或 Esc 關閉；僅在兩欄／分頁模式有浮層）".into()
            } else {
                "Bus：隱藏".into()
            };
        }
        KeyCode::Esc => {
            app.selection = None;
            app.bus_overlay = false;
            app.status = "已清除選取".into();
        }
        KeyCode::Char('Y') => copy_main_pane(app),
        KeyCode::PageUp => scroll_transcript(app, -1),
        KeyCode::PageDown => scroll_transcript(app, 1),
        KeyCode::Home => {
            if app.view == CenterView::Diff {
                app.diff_cursor = 0;
            } else {
                app.transcript_scroll = Some(0);
                app.status = "逐字稿：已到最前".into();
            }
        }
        KeyCode::End => {
            if app.view == CenterView::Diff {
                app.diff_cursor = app.diff_lines.len().saturating_sub(1);
            } else {
                app.transcript_scroll = None;
                app.status = "逐字稿：跟隨最新".into();
            }
        }
        KeyCode::Char('{') => jump_hunk(app, false),
        KeyCode::Char('}') => jump_hunk(app, true),
        _ => {}
    }
    Ok(())
}

pub(crate) fn handle_input_mode(
    app: &mut App,
    writer: &mut ConnStream,
    key: KeyEvent,
) -> Result<()> {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input.clear();
            app.input_cursor = 0;
            app.spawn_target = None;
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.spawn_worktree = !app.spawn_worktree;
            app.status = format!(
                "worktree = {}",
                if app.spawn_worktree { "on" } else { "off" }
            );
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.spawn_permission = match app.spawn_permission {
                None => Some(PermissionPolicy::Ask),
                Some(PermissionPolicy::Ask) => Some(PermissionPolicy::Auto),
                Some(PermissionPolicy::Auto) => Some(PermissionPolicy::Bypass),
                Some(PermissionPolicy::Bypass) => None,
            };
            app.spawn_bypass_armed = false;
            app.status = format!(
                "權限 = {}",
                app.spawn_permission.map_or("預設(config)", |p| p.id())
            );
        }
        KeyCode::Tab if app.mode == Mode::Command => complete_command(app),
        KeyCode::Backspace => backspace_input(app),
        KeyCode::Delete => delete_input(app),
        KeyCode::Left => app.input_cursor = app.input_cursor.saturating_sub(1),
        KeyCode::Right => {
            let n = app.input.chars().count();
            app.input_cursor = (app.input_cursor + 1).min(n);
        }
        KeyCode::Home => app.input_cursor = 0,
        KeyCode::End => app.input_cursor = app.input.chars().count(),
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input_cursor = 0;
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input_cursor = app.input.chars().count();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let b = byte_index(&app.input, app.input_cursor);
            app.input.truncate(b);
            app.input_cursor = app.input.chars().count();
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            insert_input(app, c);
        }
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            app.input_cursor = 0;
            let mode = app.mode;
            app.mode = Mode::Normal;
            match mode {
                Mode::Prompt => submit_prompt_or_route(app, writer, &text)?,
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
                        app.status = "已送出訊息".into();
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
                Mode::Command => execute_command(app, writer, &text)?,
                Mode::Jump => execute_jump(app, writer, &text)?,
                Mode::Normal | Mode::Menu | Mode::Pick => {}
            }
        }
        _ => {}
    }
    Ok(())
}

/// `:` 命令列的 Tab 補全：輸入的第一個字若只符合一個命令就補完；多個則補到共同前綴並列出候選。
fn complete_command(app: &mut App) {
    if app.input.contains(char::is_whitespace) {
        return;
    }
    let cands: Vec<&str> = crate::commands::COMMANDS
        .iter()
        .map(|(n, _)| *n)
        .filter(|c| c.starts_with(app.input.as_str()))
        .collect();
    match cands.len() {
        0 => {}
        1 => {
            app.input = format!("{} ", cands[0]);
            app.input_cursor = app.input.chars().count();
        }
        _ => {
            let first = cands[0];
            let mut common = first.len();
            for c in &cands {
                common = common.min(
                    first
                        .bytes()
                        .zip(c.bytes())
                        .take_while(|(a, b)| a == b)
                        .count(),
                );
            }
            app.input = first[..common].to_string();
            app.input_cursor = app.input.chars().count();
            app.status = format!("候選：{}", cands.join(" "));
        }
    }
}

/// 以 Unicode 字元索引取得對應的 byte offset（超出長度時回傳字串長度）。
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

/// 貼上（bracketed paste）：只插入目前輸入框；換行轉成空白以維持單行輸入。
pub(crate) fn handle_paste(app: &mut App, text: &str) {
    let sanitized = text.replace(['\r', '\n'], " ");
    insert_str_input(app, &sanitized);
}
