//! `termade-tui` — 全終端機非同步多 agent 儀表板。
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
mod help;
mod history;
mod input;
mod inputbox;
mod markdown;
mod menu;
mod mouse;
mod notify;
mod search;
mod settings;
mod tasks;

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
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as CtEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use termade_daemon::Client;
use termade_protocol::Request;
use tr::tr;

pub(crate) mod hosts;
pub(crate) use hosts::*;

/// 啟動 TUI (會先確保 daemon 在跑)。
pub fn run() -> Result<()> {
    termade_daemon::ensure_daemon()?;

    let mut client = Client::connect()?;
    let client_token = termade_daemon::get_client_token();
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
    let global_dir = termade_daemon::config_dir();
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = termade_daemon::config::Config::load(&global_dir, &cwd);

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
    let result = event_loop(&mut terminal, &mut router, rx, host_sync);
    running.store(false, Ordering::Relaxed);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // 預設擷取滑鼠：應用內可拖曳選取中央區塊並用 OSC 52 複製，滾輪也能捲動。
    // 需要終端機原生選取時按 `F2` 關閉擷取 (Shift+拖曳在多數終端機也可直接繞過)。
    // 另外保留 DECSET 1007：關閉擷取後滾輪仍會變成 Up/Down 來捲動逐字稿。
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        // 焦點回報：有焦點且正在看的 session 不必通知。
        EnableFocusChange
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
) -> Result<()> {
    let mut app = App::new();
    app.prompt_history = history::History::load(Some(history::history_path("prompt")));
    app.command_history = history::History::load(Some(history::history_path("command")));
    let mut dirty = true;
    let mut last_draw = std::time::Instant::now();

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
                                *epoch_arc.lock().unwrap() = h.daemon_epoch.clone();
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
                            app.status = tr!("連線中斷中，正在重連；請稍候");
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
                CtEvent::FocusGained => app.notify.focused = true,
                CtEvent::FocusLost => app.notify.focused = false,
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

pub(crate) fn request_selected_diff(app: &mut App, writer: &mut HostRouter) -> Result<()> {
    if let Some(id) = app.selected_id() {
        send(writer, &Request::WorktreeDiff { session: id })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
