#![expect(unused_imports, reason = "內部模組共用之跨模組 re-export 項目")]
//! `ertangle-tui` — 全終端機非同步多 agent 儀表板。
//!
//! 提供三欄式 (或依視窗寬度降級為雙欄／分頁)互動介面：
//! * 左側：所有 session 的清單、狀態符號與未讀標示；
//! * 中央：逐字稿 (支援 Markdown 渲染)、worktree diff、worktrees 清單或任務視圖；
//! * 右側：匯流排訊息、任務看板與待核准清單。
//!
//! 逐字稿區支援 Markdown 渲染 (見 [`markdown`])、翻頁捲動與「跟隨最新」，
//! 並可用 `t` 在渲染後與原始文字之間切換。

mod app;
mod attach;
mod clipboard;
mod commands;
mod diff;
mod draw;
mod flow;
mod help;
mod history;
mod input;
mod inputbox;
mod markdown;
mod menu;
mod mouse;
mod notify;
mod project;
mod ps;
mod quota;
mod search;
mod settings;
mod tasks;
mod term_reply;

pub(crate) use app::*;
pub(crate) use clipboard::*;
pub(crate) use commands::*;
pub(crate) use diff::*;
pub(crate) use draw::*;
pub(crate) use input::*;
pub(crate) use mouse::*;

pub(crate) use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
pub(crate) use ertangle_core::{
    now_millis, AgentKind, Artifact, BusMessage, Decision, MsgKind, PermissionPolicy, Role,
    Session, SessionId, SessionStatus, State, Target, WorktreeInfo,
};
pub(crate) use ratatui::layout::{Constraint, Direction, Layout, Rect};
pub(crate) use ratatui::style::{Color, Modifier, Style};
pub(crate) use ratatui::text::{Line, Span};
pub(crate) use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
pub(crate) use ratatui::{Frame as TuiFrame, Terminal};
pub(crate) use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as CtEvent, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ertangle_daemon::Client;
use ertangle_protocol::Request;
use ratatui::backend::CrosstermBackend;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tr::tr;

pub(crate) mod hosts;
pub(crate) use hosts::*;

/// 啟動 TUI (會先確保 daemon 在跑)。
pub fn run(initial_project: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(ref p) = initial_project {
        if !p.exists() || !p.is_dir() {
            anyhow::bail!("{}: {}", tr!("專案目錄不存在或不是目錄"), p.display());
        }
    }
    ertangle_daemon::ensure_daemon()?;

    let mut client = Client::connect()?;
    let client_token = ertangle_daemon::get_client_token();
    client.send(&Request::Hello {
        client: "tui".into(),
        since: None,
        token: client_token,
        epoch: None,
    })?;
    let writer = client.try_clone_stream()?;
    let mut router = HostRouter::single(writer);

    let (tx, rx) = mpsc::channel::<HostEvent>();
    let running = Arc::new(AtomicBool::new(true));

    // 啟動本機連線執行緒
    let local_seq = Arc::new(AtomicU64::new(0));
    let local_epoch = Arc::new(Mutex::new(None));
    let mut host_sync = HashMap::new();
    host_sync.insert(
        "local".to_string(),
        (local_seq.clone(), local_epoch.clone()),
    );

    spawn_host_thread(
        "local".into(),
        Some(client),
        HostConnectSpec::Local,
        tx.clone(),
        local_seq,
        local_epoch,
        running.clone(),
    );

    // 讀取設定檔並為每個啟用的遠端主機啟動獨立連線執行緒
    let global_dir = ertangle_daemon::config_dir();
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = ertangle_daemon::config::Config::load(&global_dir, &cwd);

    for (name, r) in &cfg.remotes {
        if !r.enabled {
            continue;
        }
        let seq = Arc::new(AtomicU64::new(0));
        let epoch = Arc::new(Mutex::new(None));
        host_sync.insert(name.clone(), (seq.clone(), epoch.clone()));

        spawn_host_thread(
            name.clone(),
            None,
            HostConnectSpec::Tcp {
                addr: r.addr.clone(),
                token: r.resolve_token(),
            },
            tx.clone(),
            seq,
            epoch,
            running.clone(),
        );
    }

    let mut terminal = setup_terminal()?;
    let result = event_loop(&mut terminal, &mut router, rx, host_sync, initial_project);
    running.store(false, Ordering::Relaxed);
    restore_terminal(&mut terminal)?;
    result
}

/// `ertangle - <專案根目錄名稱>`；取不到名稱時只用 `ertangle`。
fn window_title(app: &App) -> String {
    let name = std::path::Path::new(&app.home_project)
        .file_name()
        .map(|n| n.to_string_lossy().replace(|c: char| c.is_control(), ""))
        .unwrap_or_default();
    if name.is_empty() {
        "ertangle".to_string()
    } else {
        format!("ertangle - {name}")
    }
}

/// 標題前綴：本專案有待核准顯示 `⏸ `，否則有執行中顯示 `● `，全閒置則為空。
fn status_prefix(app: &App) -> &'static str {
    use ertangle_core::SessionStatus as S;
    let mut busy = false;
    for sess in app.state.sessions.values() {
        if sess.project_root() != app.home_project {
            continue;
        }
        match sess.status {
            S::WaitingApproval => return "⏸ ",
            S::Busy | S::Starting => busy = true,
            _ => {}
        }
    }
    if busy {
        "● "
    } else {
        ""
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // 預設擷取滑鼠：應用內可拖曳選取中央區塊並用 OSC 52 複製，滾輪也能捲動。
    // 需要終端機原生選取時按 `F2` 關閉擷取 (Shift+拖曳在多數終端機也可直接繞過)。
    // 另外保留 DECSET 1007：關閉擷取後滾輪仍會變成 Up/Down 來捲動逐字稿。
    // 直接送出 Kitty keyboard enhancement (`\x1b[>1u`) 而不先查詢 `supports_keyboard_enhancement()`：
    // 不支援的終端機會靜默忽略該 CSI 序列；查詢反而會在 SSH / tmux 等環境因延遲回傳
    // Device Attributes (`\x1b[>64;...c`) 並漏進輸入框。
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        EnableMouseCapture,
        // 焦點回報：有焦點且正在看的 session 不必通知。
        EnableFocusChange
    )?;
    write!(stdout, "\x1b[?1007h")?;
    // 視窗標題帶上專案名，在有 window manager 的環境可辨識是哪個專案。
    // 先 push (XTWINOPS 22) 原標題，離開時 pop 還原。
    write!(stdout, "\x1b[22;0t\x1b]0;ertangle\x07")?;
    stdout.flush()?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let backend = terminal.backend_mut();
    write!(backend, "\x1b[?1007l\x1b[23;0t")?;
    execute!(
        backend,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture,
        DisableFocusChange,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    terminal.show_cursor()?;
    Ok(())
}

/// 切換完整滑鼠擷取 (滾輪事件與點選 session)；關閉時回到原生選取。
pub(crate) fn set_mouse_capture(on: bool) -> Result<()> {
    let mut out = io::stdout();
    if on {
        execute!(out, EnableMouseCapture)?;
    } else {
        execute!(out, DisableMouseCapture)?;
    }
    Ok(())
}

pub(crate) fn send(writer: &mut HostRouter, req: &Request) -> Result<()> {
    writer.send(req)
}

type HostSyncMap = HashMap<String, (Arc<AtomicU64>, Arc<Mutex<Option<String>>>)>;

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    writer: &mut HostRouter,
    rx: Receiver<HostEvent>,
    host_sync: HostSyncMap,
    initial_project: Option<std::path::PathBuf>,
) -> Result<()> {
    let mut app = App::with_project(initial_project);
    app.prompt_history = history::History::load(Some(history::history_path("prompt")));
    app.command_history = history::History::load(Some(history::history_path("command")));
    let mut dirty = true;
    let mut last_draw = std::time::Instant::now();
    let mut last_title = String::new();

    loop {
        loop {
            match rx.try_recv() {
                Ok(event) => match event {
                    HostEvent::Frame { host, frame } => {
                        app.apply_host_frame(&host, frame);
                        writer.update_from_app(&app);
                        if let Some((seq_arc, epoch_arc)) = host_sync.get(&host) {
                            if let Some(h) = app.hosts.get(&host) {
                                seq_arc.store(h.state.seq, Ordering::Relaxed);
                                *epoch_arc.lock().unwrap_or_else(|e| e.into_inner()) =
                                    h.daemon_epoch.clone();
                            }
                        }
                        dirty = true;
                    }
                    HostEvent::Disconnected { host } => {
                        app.set_host_status(&host, HostStatus::Reconnecting(tr!("重連中")));
                        if host == app.active_host {
                            app.disconnected = true;
                            app.status = tr!("與 {host} 連線中斷，正在重連…", host = host);
                        }
                        dirty = true;
                    }
                    HostEvent::Reconnected {
                        host,
                        writer: new_stream,
                    } => {
                        writer.set_writer(&host, new_stream);
                        app.set_host_status(&host, HostStatus::Connected);
                        if host == app.active_host {
                            app.disconnected = false;
                            app.status = tr!("已重新連線至 {host}", host = host);
                        }
                        dirty = true;
                    }
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if !app.disconnected {
                        app.disconnected = true;
                        app.status =
                            tr!("daemon 連線已中斷 (可能被關閉了)。畫面是最後的狀態，按 q 離開後重新啟動");
                        dirty = true;
                    }
                    break;
                }
            }
        }

        // 通知在兩次繪製之間直接寫 stdout (與 OSC 52 複製相同的做法)。
        if !app.notify.pending.is_empty() {
            if let Some(err) = app.notify.flush(&mut io::stdout().lock()) {
                app.status = err;
            }
        }

        let title = format!("{}{}", status_prefix(&app), window_title(&app));
        if title != last_title {
            // 更新終端機標題，終端機若不支援或輸出關閉則忽略
            let _ = write!(io::stdout(), "\x1b]0;{title}\x07");
            let _ = io::stdout().flush();
            last_title = title;
        }

        let was_picking = app.picker.is_some();
        dirty |= crate::menu::reconcile_question_picker(&mut app);
        app.auto_open_question();
        dirty |= app.picker.is_some() != was_picking;
        dirty |= app.rebuild_pending;
        // Redraw only on state/input changes so idling costs no CPU. While any session
        // is busy, redraw every 120ms to animate the thinking spinner.
        let any_busy = app
            .state
            .sessions
            .values()
            .any(|s| s.status == SessionStatus::Busy);
        // T77: a message packet is travelling across the Flow graph; animate it smoothly.
        let flow_motion = (app.view == crate::app::CenterView::Flow || app.bus_visible)
            && !app.flow_list
            && app.state.messages.last().is_some_and(|m| {
                ertangle_core::now_millis().saturating_sub(m.ts) < crate::flow::FLOW_PACKET_MS
            });
        let redraw_interval = if flow_motion {
            Duration::from_millis(60)
        } else if any_busy {
            Duration::from_millis(120)
        } else {
            Duration::from_secs(1)
        };
        if dirty || last_draw.elapsed() >= redraw_interval {
            last_draw = std::time::Instant::now();
            terminal.draw(|f| draw(f, &mut app))?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(60))? {
            dirty = true;
            // 把同一次讀取已到的事件一起拿出來，才能辨識並丟掉漏進來的終端機回應。
            let mut batch = vec![event::read()?];
            while batch.len() < 256 && event::poll(Duration::ZERO)? {
                batch.push(event::read()?);
            }
            for ev in term_reply::strip_terminal_replies(batch) {
                handle_event(&mut app, writer, ev)?;
                if app.should_quit {
                    break;
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// 處理一個 crossterm 事件。
fn handle_event(app: &mut App, writer: &mut HostRouter, ev: CtEvent) -> Result<()> {
    match ev {
        CtEvent::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            if let Err(e) = handle_key(app, writer, key) {
                // 連線已斷時，送出請求失敗不該讓整個 TUI 崩掉。
                if app.disconnected {
                    app.status = tr!("連線中斷中，正在重連；請稍候");
                } else {
                    return Err(e);
                }
            }
        }
        CtEvent::Mouse(mouse) => {
            if let Err(e) = handle_mouse(app, writer, mouse) {
                if !app.disconnected {
                    return Err(e);
                }
            }
        }
        CtEvent::Paste(text) => handle_paste(app, &text),
        CtEvent::FocusGained => app.notify.focused = true,
        CtEvent::FocusLost => app.notify.focused = false,
        _ => {}
    }
    Ok(())
}

pub(crate) fn request_selected_diff(app: &mut App, writer: &mut HostRouter) -> Result<()> {
    if let Some(id) = app.selected_id() {
        send(
            writer,
            &Request::WorktreeDiff {
                session: id,
                stat_only: false,
                paths: Vec::new(),
                max_lines: None,
                fold_generated: false,
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
