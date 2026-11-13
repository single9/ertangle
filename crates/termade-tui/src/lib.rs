//! `termade-tui` — 全終端非同步多 agent 儀表板。
//!
//! 提供三欄式（或依視窗寬度降級為雙欄／分頁）互動介面：
//! * 左側：所有 session 的清單、狀態符號與未讀標示；
//! * 中央：逐字稿（支援 Markdown 渲染）、worktree diff 或 worktrees 清單；
//! * 右側：匯流排訊息、任務看板與待審批清單。
//!
//! 逐字稿區支援 Markdown 渲染（見 [`markdown`]）、翻頁捲動與「跟隨最新」，
//! 並可用 `t` 在渲染後與原始文字之間切換。

mod app;
mod attach;
mod clipboard;
mod commands;
mod diff;
mod draw;
mod help;
mod history;
mod input;
mod inputbox;
mod markdown;
mod menu;
mod mouse;

#[allow(unused_imports)]
pub(crate) use app::*;
#[allow(unused_imports)]
pub(crate) use clipboard::*;
#[allow(unused_imports)]
pub(crate) use commands::*;
#[allow(unused_imports)]
pub(crate) use diff::*;
#[allow(unused_imports)]
pub(crate) use draw::*;
#[allow(unused_imports)]
pub(crate) use input::*;
#[allow(unused_imports)]
pub(crate) use mouse::*;

#[allow(unused_imports)]
pub(crate) use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
#[allow(unused_imports)]
pub(crate) use ratatui::layout::{Constraint, Direction, Layout, Rect};
#[allow(unused_imports)]
pub(crate) use ratatui::style::{Color, Modifier, Style};
#[allow(unused_imports)]
pub(crate) use ratatui::text::{Line, Span};
#[allow(unused_imports)]
pub(crate) use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
#[allow(unused_imports)]
pub(crate) use ratatui::{Frame as TuiFrame, Terminal};
#[allow(unused_imports)]
pub(crate) use termade_core::{
    now_millis, AgentKind, Artifact, BusMessage, Decision, MsgKind, PermissionPolicy, Role,
    Session, SessionId, SessionStatus, State, Target, WorktreeInfo,
};
#[allow(unused_imports)]
pub(crate) use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CtEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use termade_daemon::{Client, ConnStream};
use termade_protocol::{Frame, Request};

/// 啟動 TUI（會先確保 daemon 在跑）。
pub fn run() -> Result<()> {
    termade_daemon::ensure_daemon()?;

    let mut client = Client::connect()?;
    client.send(&Request::Hello {
        client: "tui".into(),
        since: None,
        token: std::env::var("TERMADE_TOKEN").ok(),
        epoch: None,
    })?;
    let mut writer = client.try_clone_stream()?;

    let (tx, rx) = mpsc::channel::<Frame>();
    std::thread::spawn(move || {
        while let Ok(frame) = client.recv() {
            if tx.send(frame).is_err() {
                break;
            }
        }
    });

    let mut terminal = setup_terminal()?;
    let result = event_loop(&mut terminal, &mut writer, rx);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // 預設擷取滑鼠：應用內可拖曳選取中央區塊並用 OSC 52 複製，滾輪也能捲動。
    // 需要終端原生選取時按 `F2` 關閉擷取（Shift+拖曳在多數終端也可直接繞過）。
    // 另外保留 DECSET 1007：關閉擷取後滾輪仍會變成 Up/Down 來捲動逐字稿。
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    write!(stdout, "\x1b[?1007h")?;
    stdout.flush()?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let backend = terminal.backend_mut();
    write!(backend, "\x1b[?1007l")?;
    execute!(
        backend,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    terminal.show_cursor()?;
    Ok(())
}

/// 切換完整滑鼠擷取（滾輪事件與點選 session）；關閉時回到原生選取。
pub(crate) fn set_mouse_capture(on: bool) -> Result<()> {
    let mut out = io::stdout();
    if on {
        execute!(out, EnableMouseCapture)?;
    } else {
        execute!(out, DisableMouseCapture)?;
    }
    Ok(())
}

pub(crate) fn send(writer: &mut ConnStream, req: &Request) -> Result<()> {
    termade_daemon::send_request(writer, req)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    writer: &mut ConnStream,
    rx: Receiver<Frame>,
) -> Result<()> {
    let mut app = App::new();
    app.prompt_history = history::History::load(Some(history::history_path("prompt")));
    app.command_history = history::History::load(Some(history::history_path("command")));
    let mut dirty = true;
    let mut last_draw = std::time::Instant::now();

    loop {
        loop {
            match rx.try_recv() {
                Ok(frame) => {
                    apply_frame(&mut app, frame);
                    dirty = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if !app.disconnected {
                        app.disconnected = true;
                        app.status =
                            "daemon 連線已中斷（可能被關閉了）。畫面是最後的狀態，按 q 離開後重新啟動".into();
                        dirty = true;
                    }
                    break;
                }
            }
        }

        let before = app.status.clone();
        commands::flush_queues(&mut app, writer)?;
        dirty |= app.status != before;

        dirty |= app.rebuild_pending;
        // 只在有狀態或輸入變動時重繪，閒置時不佔 CPU。
        // 每秒至少重繪一次，讓時間／狀態類顯示保持更新。
        if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
            last_draw = std::time::Instant::now();
            terminal.draw(|f| draw(f, &mut app))?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(60))? {
            dirty = true;
            match event::read()? {
                CtEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Err(e) = handle_key(&mut app, writer, key) {
                        // 連線已斷時，送出請求失敗不該讓整個 TUI 崩掉。
                        if app.disconnected {
                            app.status = "daemon 已斷線，無法送出；按 q 離開".into();
                        } else {
                            return Err(e);
                        }
                    }
                }
                CtEvent::Mouse(mouse) => {
                    if let Err(e) = handle_mouse(&mut app, writer, mouse) {
                        if !app.disconnected {
                            return Err(e);
                        }
                    }
                }
                CtEvent::Paste(text) => handle_paste(&mut app, &text),
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn apply_frame(app: &mut App, frame: Frame) {
    match frame {
        Frame::Ready { pid, version, .. } => {
            app.status = format!("daemon pid={pid} v{version}");
        }
        Frame::Snapshot { state } => {
            app.state = *state;
            app.clamp_selection();
        }
        Frame::Record { record } => {
            app.state.apply(&record);
            app.clamp_selection();
        }
        Frame::Messages { .. } => {}
        Frame::Diff { session, diff, .. } => {
            app.diff_session = Some(session);
            app.diff_lines = diff.lines().map(|l| l.to_string()).collect();
            app.diff_gutter = build_gutter(&app.diff_lines);
            let (added, removed) = diff_stat(&app.diff_lines);
            app.diff_added = added;
            app.diff_removed = removed;
            app.diff_cursor = 0;
        }
        Frame::Worktrees { worktrees } => {
            app.worktrees = worktrees;
        }
        Frame::Ok { note } => {
            if let Some(n) = note {
                app.status = n;
            }
        }
        Frame::Error { message } => {
            app.status = format!("錯誤：{message}");
        }
    }
}

pub(crate) fn request_selected_diff(app: &mut App, writer: &mut ConnStream) -> Result<()> {
    if let Some(id) = app.selected_id() {
        send(writer, &Request::WorktreeDiff { session: id })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
