//! `ade-tui` — Ratatui 前端。
//!
//! 它是 daemon 的一個 client：接收事件流、送出請求。因為狀態都在 daemon，
//! 這裡只保留「渲染所需」的鏡像狀態，關掉重開不會影響任何 agent。

use ade_core::{AgentKind, Decision, MsgKind, Role, SessionId, State, Target};
use ade_daemon::Client;
use ade_protocol::{Frame, Request, SpawnRequest, Transport};
use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame as TuiFrame, Terminal};
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[derive(PartialEq, Eq)]
enum Mode {
    Normal,
    Prompt,
    Spawn,
    Message,
}

struct App {
    state: State,
    selected: usize,
    mode: Mode,
    input: String,
    status: String,
    spawn_worktree: bool,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        App {
            state: State::new(),
            selected: 0,
            mode: Mode::Normal,
            input: String::new(),
            status: "已連線 daemon".into(),
            spawn_worktree: false,
            should_quit: false,
        }
    }

    fn session_ids(&self) -> Vec<SessionId> {
        self.state
            .sessions_sorted()
            .iter()
            .map(|s| s.id.clone())
            .collect()
    }

    fn selected_id(&self) -> Option<SessionId> {
        self.session_ids().get(self.selected).cloned()
    }

    /// 回傳目前選取 session 尚未回覆的審批請求 id。
    fn selected_permission(&self) -> Option<String> {
        let id = self.selected_id()?;
        self.state
            .pending_permissions
            .values()
            .find(|p| p.session == id)
            .map(|p| p.req.clone())
    }

    fn clamp_selection(&mut self) {
        let n = self.session_ids().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }
}

/// 啟動 TUI（會先確保 daemon 在跑）。
pub fn run() -> Result<()> {
    ade_daemon::ensure_daemon()?;

    let mut client = Client::connect()?;
    client.send(&Request::Hello {
        client: "tui".into(),
        since: None,
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
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn send(writer: &mut UnixStream, req: &Request) -> Result<()> {
    ade_daemon::send_request(writer, req)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    writer: &mut UnixStream,
    rx: Receiver<Frame>,
) -> Result<()> {
    let mut app = App::new();

    loop {
        // 吸收 daemon 傳來的 frames。
        while let Ok(frame) = rx.try_recv() {
            apply_frame(&mut app, frame);
        }

        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(60))? {
            if let CtEvent::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, writer, key)?;
                }
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
        Frame::Ready { pid, version } => {
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

fn handle_key(app: &mut App, writer: &mut UnixStream, key: KeyEvent) -> Result<()> {
    if app.mode != Mode::Normal {
        return handle_input_mode(app, writer, key);
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.selected + 1 < app.session_ids().len() {
                app.selected += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.selected = app.selected.saturating_sub(1);
        }
        KeyCode::Enter | KeyCode::Char('i') => {
            app.mode = Mode::Prompt;
            app.input.clear();
        }
        KeyCode::Char('n') => {
            app.mode = Mode::Spawn;
            app.input.clear();
            app.spawn_worktree = false;
            app.status = "輸入：<agent> <prompt>（Ctrl-W 切換 worktree）".into();
        }
        KeyCode::Char('m') => {
            app.mode = Mode::Message;
            app.input.clear();
            app.status = "輸入要送給選取 session 的訊息".into();
        }
        KeyCode::Char('a') => respond_permission(app, writer, Decision::Allow)?,
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
            app.status = "已要求快照".into();
        }
        _ => {}
    }
    Ok(())
}

fn handle_input_mode(app: &mut App, writer: &mut UnixStream, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input.clear();
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.spawn_worktree = !app.spawn_worktree;
            app.status = format!(
                "worktree = {}",
                if app.spawn_worktree { "on" } else { "off" }
            );
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Enter => {
            let text = std::mem::take(&mut app.input);
            match app.mode {
                Mode::Prompt => {
                    if let Some(id) = app.selected_id() {
                        send(writer, &Request::Prompt { session: id, text })?;
                        app.status = "已送出 prompt".into();
                    } else {
                        app.status = "沒有選取的 session".into();
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
                            },
                        )?;
                        app.status = "已送出訊息".into();
                    }
                }
                Mode::Normal => {}
            }
            app.mode = Mode::Normal;
        }
        _ => {}
    }
    Ok(())
}

fn submit_spawn(app: &mut App, writer: &mut UnixStream, text: &str) -> Result<()> {
    let mut parts = text.splitn(2, char::is_whitespace);
    let kind_str = parts.next().unwrap_or("");
    let prompt = parts.next().map(|s| s.trim().to_string());

    let Some(kind) = AgentKind::parse(kind_str) else {
        app.status = format!("未知的 agent：{kind_str}（opencode/claude/codex/antigravity）");
        return Ok(());
    };
    let prompt = prompt.filter(|p| !p.is_empty());
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
            permission: Default::default(),
            parent: None,
        }),
    )?;
    app.status = format!("已要求啟動 {kind}");
    Ok(())
}

fn respond_permission(app: &mut App, writer: &mut UnixStream, decision: Decision) -> Result<()> {
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

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

fn draw(f: &mut TuiFrame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 標題
            Constraint::Min(6),    // 主區
            Constraint::Length(3), // 輸入
            Constraint::Length(1), // 狀態
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30),
            Constraint::Min(20),
            Constraint::Length(36),
        ])
        .split(chunks[1]);

    draw_sessions(f, cols[0], app);
    draw_transcript(f, cols[1], app);
    draw_bus(f, cols[2], app);
    draw_input(f, chunks[2], app);
    draw_status(f, chunks[3], app);
}

fn draw_header(f: &mut TuiFrame, area: Rect, app: &App) {
    let live = app.state.live_sessions().count();
    let text = Line::from(vec![
        Span::styled(
            " ade ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  sessions: {} live / {} total   tasks: {}",
            live,
            app.state.sessions.len(),
            app.state.tasks.len()
        )),
    ]);
    f.render_widget(Paragraph::new(text), area);
}

fn draw_sessions(f: &mut TuiFrame, area: Rect, app: &App) {
    let ids = app.session_ids();
    let items: Vec<ListItem> = ids
        .iter()
        .filter_map(|id| app.state.sessions.get(id))
        .map(|s| {
            let color = match s.status {
                ade_core::SessionStatus::Failed => Color::Red,
                ade_core::SessionStatus::WaitingApproval => Color::Yellow,
                ade_core::SessionStatus::Busy => Color::Green,
                ade_core::SessionStatus::Idle => Color::Gray,
                _ => Color::DarkGray,
            };
            let branch = s.branch.as_deref().unwrap_or("");
            let line = Line::from(vec![
                Span::styled(format!("{} ", s.status.glyph()), Style::default().fg(color)),
                Span::raw(s.label.clone()),
                Span::styled(
                    format!(
                        "  [{}]{}",
                        s.kind.id(),
                        if branch.is_empty() {
                            String::new()
                        } else {
                            format!(" @{branch}")
                        }
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut list_state = ListState::default();
    if !ids.is_empty() {
        list_state.select(Some(app.selected.min(ids.len() - 1)));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Sessions "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut list_state);
}

fn draw_transcript(f: &mut TuiFrame, area: Rect, app: &App) {
    let title = match app.selected_id() {
        Some(id) => {
            let s = &app.state.sessions[&id];
            format!(
                " {} · {} · {} ",
                s.label,
                s.kind.id(),
                if s.transport.is_empty() {
                    "-"
                } else {
                    &s.transport
                }
            )
        }
        None => " (尚無 session：按 n 建立) ".into(),
    };

    let mut lines: Vec<Line> = Vec::new();
    if let Some(id) = app.selected_id() {
        if let Some(buf) = app.state.transcripts.get(&id) {
            let h = area.height.saturating_sub(2) as usize;
            let start = buf.len().saturating_sub(h.max(1));
            for l in buf.iter().skip(start) {
                lines.push(Line::raw(l.clone()));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "這裡會顯示 agent 的輸出。按 i 輸入 prompt、n 建立 session。",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_bus(f: &mut TuiFrame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    // 待審批請求置頂。
    for p in app.state.pending_permissions.values() {
        lines.push(Line::from(vec![
            Span::styled(
                "⏸ 審批 ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{}：{}", p.title, truncate(&p.detail, 40))),
        ]));
    }
    if !app.state.pending_permissions.is_empty() {
        lines.push(Line::styled(
            "  [a] 允許  [d] 拒絕",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let h = area.height.saturating_sub(2) as usize;
    let msgs = &app.state.messages;
    let start = msgs.len().saturating_sub(h);
    for m in msgs.iter().skip(start) {
        let to = match &m.to {
            Target::Direct(s) => format!("@{s}"),
            Target::Channel(c) => format!("#{c}"),
            Target::Broadcast => "*".into(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}→{} ", short(&m.from), short(&to)),
                Style::default().fg(Color::Magenta),
            ),
            Span::raw(truncate(&m.body, 60)),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "agent 之間的訊息會出現在這裡。",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Bus / Tasks "),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_input(f: &mut TuiFrame, area: Rect, app: &App) {
    let title = match app.mode {
        Mode::Normal => " 輸入（i=prompt, n=spawn, m=message） ",
        Mode::Prompt => " prompt → 選取的 session（Enter 送出, Esc 取消） ",
        Mode::Spawn => " spawn：<agent> <prompt>（Ctrl-W 切換 worktree, Enter 送出） ",
        Mode::Message => " message → 選取的 session（Enter 送出） ",
    };
    let style = if app.mode == Mode::Normal {
        Style::default()
    } else {
        Style::default().fg(Color::Cyan)
    };
    let para = Paragraph::new(app.input.clone())
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, area);
    if app.mode != Mode::Normal {
        let _ = io::stdout().flush();
    }
}

fn draw_status(f: &mut TuiFrame, area: Rect, app: &App) {
    let text = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw("  q 離開（daemon 續跑）  j/k 選擇  a/d 審批  x 中斷  K 結束  r 重整"),
    ]);
    f.render_widget(Paragraph::new(text), area);
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn short(id: &str) -> String {
    id.rsplit('_')
        .next()
        .unwrap_or(id)
        .chars()
        .take(6)
        .collect()
}
