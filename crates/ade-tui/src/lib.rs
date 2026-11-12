//! `ade-tui` — Ratatui 前端（含 M3 Diff/Review 視圖與 M4 命令面板 / Jump-Anywhere / 成本與休眠控制）。
//!
//! 它是 daemon 的一個 client：接收事件流、送出請求。因為狀態都在 daemon，
//! 這裡只保留「渲染所需」的鏡像狀態，關掉重開不會影響任何 agent。
//!
//! 逐字稿區支援 Markdown 渲染（見 [`markdown`]）、翻頁捲動與「跟隨最新」，
//! 並可用 `t` 在渲染後與原始文字之間切換。

mod markdown;

use ade_core::{
    AgentKind, Decision, MsgKind, Role, SessionId, SessionStatus, State, Target, WorktreeInfo,
};
use ade_daemon::{Client, ConnStream};
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
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::{Frame as TuiFrame, Terminal};
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Normal,
    Prompt,
    Spawn,
    Message,
    /// M3：對選取的 diff 行下審查註解並送回該 agent。
    Review,
    /// M4：命令面板（`/` 或 `Ctrl-P`）。
    Command,
    /// M4：快速跳轉任何 session（`Ctrl-K`）。
    Jump,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CenterView {
    Transcript,
    Diff,
    Worktrees,
}

/// 逐字稿渲染快取：只有來源／寬度／模式改變時才重算 Markdown。
struct MdCache {
    session: SessionId,
    seq: u64,
    raw: bool,
    width: u16,
    lines: Vec<Line<'static>>,
}

struct App {
    state: State,
    selected: usize,
    view: CenterView,
    mode: Mode,
    input: String,
    status: String,
    spawn_worktree: bool,
    /// M3：目前顯示的 worktree diff 內容與游標行。
    diff_session: Option<SessionId>,
    diff_lines: Vec<String>,
    diff_cursor: usize,
    /// 每行對應的（舊檔行號, 新檔行號）邊欄。
    diff_gutter: Vec<(Option<u64>, Option<u64>)>,
    diff_added: usize,
    diff_removed: usize,
    /// M3：受管 worktrees 快照。
    worktrees: Vec<WorktreeInfo>,
    /// 逐字稿捲動位置（渲染後行索引）；`None` = 跟隨最新輸出。
    transcript_scroll: Option<usize>,
    /// 最近一次繪製時可見的行數與總行數（供翻頁與捲軸使用）。
    transcript_viewport: usize,
    transcript_total: usize,
    /// 逐字稿是否以原始文字（不做 Markdown 渲染）顯示。
    raw_md: bool,
    md_cache: Option<MdCache>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        App {
            state: State::new(),
            selected: 0,
            view: CenterView::Transcript,
            mode: Mode::Normal,
            input: String::new(),
            status: "已連線 daemon".into(),
            spawn_worktree: false,
            diff_session: None,
            diff_lines: Vec::new(),
            diff_cursor: 0,
            diff_gutter: Vec::new(),
            diff_added: 0,
            diff_removed: 0,
            worktrees: Vec::new(),
            transcript_scroll: None,
            transcript_viewport: 0,
            transcript_total: 0,
            raw_md: false,
            md_cache: None,
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

    /// 取得目前 diff 游標所在行的位置描述（`檔案 :: 行內容`）。
    fn current_diff_location(&self) -> String {
        if self.diff_lines.is_empty() {
            return "diff".into();
        }
        let idx = self.diff_cursor.min(self.diff_lines.len() - 1);
        let mut current_file = "unknown";
        for l in &self.diff_lines[..=idx] {
            if let Some(rest) = l.strip_prefix("+++ b/") {
                current_file = rest.trim();
            } else if let Some(rest) = l.strip_prefix("diff --git ") {
                current_file = rest.trim();
            }
        }
        let line_text = self.diff_lines[idx].trim();
        format!("{current_file} (L{}: {})", idx + 1, truncate(line_text, 48))
    }
}

/// 啟動 TUI（會先確保 daemon 在跑）。
pub fn run() -> Result<()> {
    ade_daemon::ensure_daemon()?;

    let mut client = Client::connect()?;
    client.send(&Request::Hello {
        client: "tui".into(),
        since: None,
        token: std::env::var("ADE_TOKEN").ok(),
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

fn send(writer: &mut ConnStream, req: &Request) -> Result<()> {
    ade_daemon::send_request(writer, req)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    writer: &mut ConnStream,
    rx: Receiver<Frame>,
) -> Result<()> {
    let mut app = App::new();

    loop {
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

fn request_selected_diff(app: &mut App, writer: &mut ConnStream) -> Result<()> {
    if let Some(id) = app.selected_id() {
        send(writer, &Request::WorktreeDiff { session: id })?;
    }
    Ok(())
}

fn handle_key(app: &mut App, writer: &mut ConnStream, key: KeyEvent) -> Result<()> {
    if app.mode != Mode::Normal {
        return handle_input_mode(app, writer, key);
    }

    // Ctrl-P：命令面板；Ctrl-K：Jump Anywhere
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                app.should_quit = true;
                return Ok(());
            }
            KeyCode::Char('p') => {
                app.mode = Mode::Command;
                app.input.clear();
                app.status =
                    "命令：spawn / merge / diff / gc / sleep / wake / task / broadcast / shutdown"
                        .into();
                return Ok(());
            }
            KeyCode::Char('k') => {
                app.mode = Mode::Jump;
                app.input.clear();
                app.status = "輸入關鍵字跳轉至匹配的 session（label / id / branch）".into();
                return Ok(());
            }
            KeyCode::Char('d') => {
                scroll_transcript(app, 1);
                return Ok(());
            }
            KeyCode::Char('u') => {
                scroll_transcript(app, -1);
                return Ok(());
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('Q') => {
            send(writer, &Request::Shutdown)?;
            app.status = "已關閉 daemon（所有 agent 已結束）".into();
            app.should_quit = true;
        }
        KeyCode::Tab => {
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
            if app.view == CenterView::Diff {
                app.view = CenterView::Transcript;
            } else {
                app.view = CenterView::Diff;
                request_selected_diff(app, writer)?;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.view == CenterView::Diff {
                if app.diff_cursor + 1 < app.diff_lines.len() {
                    app.diff_cursor += 1;
                }
            } else if app.selected + 1 < app.session_ids().len() {
                app.selected += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
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
            let loc = app.current_diff_location();
            app.status = format!("對 `{loc}` 留下審查註解（Enter 送回 agent）");
        }
        KeyCode::Char('M') => {
            if let Some(id) = app.selected_id() {
                send(writer, &Request::WorktreeMerge { session: id })?;
            }
        }
        KeyCode::Char('G') => {
            send(writer, &Request::WorktreeGc { session: None })?;
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
        KeyCode::Char('/') => {
            app.mode = Mode::Command;
            app.input.clear();
            app.status =
                "命令：spawn / merge / diff / gc / sleep / wake / task / broadcast / shutdown"
                    .into();
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
            app.status = format!(
                "逐字稿顯示：{}",
                if app.raw_md {
                    "原始文字"
                } else {
                    "Markdown 渲染"
                }
            );
        }
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

fn handle_input_mode(app: &mut App, writer: &mut ConnStream, key: KeyEvent) -> Result<()> {
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
                Mode::Normal => {}
            }
        }
        _ => {}
    }
    Ok(())
}

/// 在輸入框支援 `@session 訊息`、`#channel 訊息`、`/command` 或直接送 prompt 給當前 session。
fn submit_prompt_or_route(app: &mut App, writer: &mut ConnStream, text: &str) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if let Some(cmd) = trimmed.strip_prefix('/') {
        return execute_command(app, writer, cmd);
    }
    if trimmed.starts_with('@') || trimmed.starts_with('#') {
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
        send(
            writer,
            &Request::Prompt {
                session: id,
                text: trimmed.to_string(),
            },
        )?;
        app.status = "已送出 prompt".into();
    } else {
        app.status = "沒有選取的 session".into();
    }
    Ok(())
}

fn execute_command(app: &mut App, writer: &mut ConnStream, raw: &str) -> Result<()> {
    let raw = raw.trim().trim_start_matches('/');
    let mut parts = raw.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();

    match verb.as_str() {
        "spawn" => submit_spawn(app, writer, rest)?,
        "merge" => {
            let target = if rest.is_empty() {
                app.selected_id()
            } else {
                Some(rest.to_string())
            };
            if let Some(s) = target {
                send(writer, &Request::WorktreeMerge { session: s })?;
            }
        }
        "diff" => {
            app.view = CenterView::Diff;
            request_selected_diff(app, writer)?;
        }
        "transcript" | "overview" => {
            app.view = CenterView::Transcript;
        }
        "worktrees" | "wt" => {
            app.view = CenterView::Worktrees;
            send(writer, &Request::WorktreeList)?;
        }
        "gc" => {
            let session = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            send(writer, &Request::WorktreeGc { session })?;
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

fn execute_jump(app: &mut App, writer: &mut ConnStream, query: &str) -> Result<()> {
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

fn submit_spawn(app: &mut App, writer: &mut ConnStream, text: &str) -> Result<()> {
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

fn respond_permission(app: &mut App, writer: &mut ConnStream, decision: Decision) -> Result<()> {
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
            Constraint::Length(2), // 標題、頁籤與成本
            Constraint::Min(6),    // 主區
            Constraint::Length(3), // 輸入
            Constraint::Length(1), // 狀態
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);

    // 依終端寬度調整三欄比例：寬螢幕給側欄多一點，窄螢幕留給中央。
    let w = f.area().width;
    let (side_l, side_r) = if w >= 140 {
        (34, 42)
    } else if w >= 110 {
        (30, 36)
    } else if w >= 84 {
        (26, 30)
    } else {
        (22, 24)
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(side_l),
            Constraint::Min(20),
            Constraint::Length(side_r),
        ])
        .split(chunks[1]);

    draw_sessions(f, cols[0], app);
    match app.view {
        CenterView::Transcript => draw_transcript(f, cols[1], app),
        CenterView::Diff => draw_diff(f, cols[1], app),
        CenterView::Worktrees => draw_worktrees(f, cols[1], app),
    }
    draw_bus(f, cols[2], app);
    draw_input(f, chunks[2], app);
    draw_status(f, chunks[3], app);
}

fn tab(label: &str, active: bool) -> Span<'static> {
    if active {
        Span::styled(
            format!(" [{label}] "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!("  {label}  "), Style::default().fg(Color::DarkGray))
    }
}

fn draw_header(f: &mut TuiFrame, area: Rect, app: &App) {
    let live = app.state.live_sessions().count();
    let cost = app.state.total_cost();

    let tabs = Line::from(vec![
        Span::styled(
            " ade ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        tab("Overview", app.view == CenterView::Transcript),
        tab("Diff (g)", app.view == CenterView::Diff),
        tab("Worktrees", app.view == CenterView::Worktrees),
        Span::raw("   "),
        Span::styled(
            format!("{} sessions", live),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!("  · {} tasks", app.state.tasks.len()),
            Style::default().fg(Color::Gray),
        ),
    ]);

    let md_mode = if app.raw_md { "raw" } else { "md" };
    let follow = if app.view == CenterView::Transcript {
        match app.transcript_scroll {
            None => "▼ follow",
            Some(_) => "⏸ paused",
        }
    } else {
        ""
    };
    let stats = Line::from(vec![
        Span::styled("  cost ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("${:.4}", cost.usd),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("  ({} in / {} out)", cost.input_tokens, cost.output_tokens),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("   md:{md_mode}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            if follow.is_empty() {
                String::new()
            } else {
                format!("   {follow}")
            },
            Style::default().fg(if app.transcript_scroll.is_none() {
                Color::Green
            } else {
                Color::Yellow
            }),
        ),
    ]);

    f.render_widget(Paragraph::new(vec![tabs, stats]), area);
}

fn draw_sessions(f: &mut TuiFrame, area: Rect, app: &App) {
    let ids = app.session_ids();
    let items: Vec<ListItem> = ids
        .iter()
        .filter_map(|id| app.state.sessions.get(id))
        .map(|s| {
            let color = match s.status {
                SessionStatus::Failed => Color::Red,
                SessionStatus::WaitingApproval => Color::Yellow,
                SessionStatus::Busy => Color::Green,
                SessionStatus::Sleeping => Color::Blue,
                SessionStatus::Idle => Color::Gray,
                _ => Color::DarkGray,
            };
            let branch = s.branch.as_deref().unwrap_or("");
            let note = s.status_note.as_deref().unwrap_or("");
            let mut spans = vec![
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
            ];
            if !note.is_empty() {
                spans.push(Span::styled(
                    format!(" · {}", truncate(note, 16)),
                    Style::default().fg(Color::Cyan),
                ));
            }
            let line = Line::from(spans);
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

/// 依目前模式（Markdown / 原始）與寬度，把逐字稿建成樣式化且已換行的行。
fn build_transcript_lines(app: &App, id: &SessionId, width: usize) -> Vec<Line<'static>> {
    let buf = match app.state.transcripts.get(id) {
        Some(b) => b,
        None => return Vec::new(),
    };
    let src: Vec<String> = buf.iter().cloned().collect();
    let lines: Vec<Line<'static>> = if app.raw_md {
        src.iter().map(|l| Line::raw(l.clone())).collect()
    } else {
        markdown::Renderer::new().render(&src, width)
    };
    markdown::wrap_lines(lines, width)
}

fn draw_transcript(f: &mut TuiFrame, area: Rect, app: &mut App) {
    let selected = app.selected_id();
    // 保留右側一行給捲軸。
    let inner_w = area.width.saturating_sub(3).max(8) as usize;

    let need_rebuild = match (&app.md_cache, &selected) {
        (Some(c), Some(id)) => {
            c.session != *id
                || c.seq != app.state.seq
                || c.raw != app.raw_md
                || c.width != area.width
        }
        _ => true,
    };
    if need_rebuild {
        match &selected {
            Some(id) => {
                if app.md_cache.as_ref().map(|c| &c.session) != Some(id) {
                    app.transcript_scroll = None; // 換 session 時回到跟隨
                }
                let lines = build_transcript_lines(app, id, inner_w);
                app.md_cache = Some(MdCache {
                    session: id.clone(),
                    seq: app.state.seq,
                    raw: app.raw_md,
                    width: area.width,
                    lines,
                });
            }
            None => app.md_cache = None,
        }
    }

    let viewport = area.height.saturating_sub(2) as usize;
    let total = app.md_cache.as_ref().map_or(0, |c| c.lines.len());
    app.transcript_viewport = viewport;
    app.transcript_total = total;

    let max_off = total.saturating_sub(viewport);
    let off = match app.transcript_scroll {
        Some(o) => {
            let o = o.min(max_off);
            if o >= max_off {
                app.transcript_scroll = None;
            }
            o
        }
        None => max_off,
    };

    let pct = (off * 100)
        .checked_div(max_off)
        .map(|p| p.min(100))
        .unwrap_or(100);
    let title = match &selected {
        Some(id) => {
            let s = &app.state.sessions[id];
            format!(
                " Transcript: {} · {} · {} (${:.4}) · {}% · {} ",
                s.label,
                s.kind.id(),
                if s.transport.is_empty() {
                    "-"
                } else {
                    &s.transport
                },
                s.cost.usd,
                pct,
                if app.raw_md { "raw" } else { "md" },
            )
        }
        None => " (尚無 session：按 n 建立) ".into(),
    };

    let mut visible: Vec<Line> = app
        .md_cache
        .as_ref()
        .map(|c| c.lines.iter().skip(off).take(viewport).cloned().collect())
        .unwrap_or_default();
    if visible.is_empty() {
        visible.push(Line::styled(
            "這裡會顯示 agent 的輸出。按 i 輸入 prompt、n 建立 session、g 切換 Diff 視圖。",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let para = Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, area);

    if total > viewport {
        let mut sb_state = ScrollbarState::new(max_off).position(off);
        let sb_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            sb_area,
            &mut sb_state,
        );
    }
}

fn diff_line_style(l: &str) -> Style {
    if l.starts_with("+++")
        || l.starts_with("---")
        || l.starts_with("diff ")
        || l.starts_with("index ")
    {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if l.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if l.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if l.starts_with('-') {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    }
}

fn draw_diff(f: &mut TuiFrame, area: Rect, app: &App) {
    let title = match app.selected_id() {
        Some(id) => {
            let s = &app.state.sessions[&id];
            format!(
                " Diff: {} ({})  +{} -{}  ·  j/k 選行 · {{/}} hunk · c 註解 · M 合併 · [ ] 換 session ",
                s.label,
                s.branch.as_deref().unwrap_or("main"),
                app.diff_added,
                app.diff_removed,
            )
        }
        None => " Diff ".into(),
    };

    let items: Vec<ListItem> = if app.diff_lines.is_empty() {
        vec![ListItem::new(Line::styled(
            "(尚無 diff 資料，按 r 重新載入)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.diff_lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let (old, new) = app.diff_gutter.get(i).copied().unwrap_or((None, None));
                let g_old = old
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_else(|| "    ".into());
                let g_new = new
                    .map(|n| format!("{n:>4}"))
                    .unwrap_or_else(|| "    ".into());
                let spans = vec![
                    Span::styled(
                        format!("{g_old} "),
                        Style::default().fg(Color::Rgb(150, 100, 100)),
                    ),
                    Span::styled(
                        format!("{g_new} "),
                        Style::default().fg(Color::Rgb(100, 150, 100)),
                    ),
                    Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(l.clone(), diff_line_style(l)),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect()
    };

    let mut list_state = ListState::default();
    if !app.diff_lines.is_empty() {
        list_state.select(Some(app.diff_cursor.min(app.diff_lines.len() - 1)));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut list_state);

    let viewport = area.height.saturating_sub(2) as usize;
    let total = app.diff_lines.len();
    if total > viewport {
        let max_off = total - viewport;
        let pos = app.diff_cursor.saturating_sub(viewport / 2).min(max_off);
        let mut sb_state = ScrollbarState::new(max_off).position(pos);
        let sb_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            sb_area,
            &mut sb_state,
        );
    }
}

fn draw_worktrees(f: &mut TuiFrame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    if app.worktrees.is_empty() {
        lines.push(Line::styled(
            "(目前沒有獨立 worktree；建立 session 時開啟 --worktree 或按 Ctrl-W)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for wt in &app.worktrees {
            let state_tag = if wt.live { "● live" } else { "✓ exited" };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<20} ", wt.label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("@{}  ", wt.branch),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!("{state_tag}  {}  ({})", wt.summary, wt.path)),
            ]));
        }
    }
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Worktrees（M 合併選取 session · G 清理已結束 worktree） "),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_bus(f: &mut TuiFrame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

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
            "  [a] 允許  [A] 一律允許  [d] 拒絕",
            Style::default().fg(Color::DarkGray),
        ));
    }

    if !app.state.tasks.is_empty() {
        for t in app.state.tasks.values() {
            let color = match t.status {
                ade_core::TaskStatus::Done => Color::Green,
                ade_core::TaskStatus::Doing | ade_core::TaskStatus::Review => Color::Cyan,
                ade_core::TaskStatus::Blocked => Color::Red,
                ade_core::TaskStatus::Todo => Color::Gray,
            };
            lines.push(Line::from(vec![
                Span::styled(format!("[{}] ", t.status.id()), Style::default().fg(color)),
                Span::raw(truncate(&t.title, 26)),
            ]));
        }
        lines.push(Line::styled(
            "────────────────────────",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let h = area.height.saturating_sub(2 + lines.len() as u16) as usize;
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
            "agent 之間的訊息與任務看板會出現在這裡。",
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
        Mode::Normal => {
            " 輸入（i=prompt/@peer/#ch, n=spawn, /=命令, Ctrl-K=跳轉, g=Diff, z=休眠, t=原始/渲染, PgUp/PgDn=捲動） "
        }
        Mode::Prompt => " prompt（可輸入 @peer 訊息、#channel 訊息、/命令，Enter 送出） ",
        Mode::Spawn => " spawn：<agent> <prompt>（Ctrl-W 切換 worktree, Enter 送出） ",
        Mode::Message => " message → 選取的 session（Enter 送出） ",
        Mode::Review => " review 註解 → 送回選取的 session 修改（Enter 送出） ",
        Mode::Command => " 命令面板（spawn / merge / diff / gc / sleep / wake / task / shutdown） ",
        Mode::Jump => " Jump Anywhere：輸入關鍵字跳轉 session（Enter 確認） ",
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
    let hints = match app.view {
        CenterView::Transcript => {
            "  q 離開(保留 daemon)  Q 關閉 daemon  Tab 切換  i prompt  n spawn  t 原始/渲染  PgUp/PgDn 捲動  / 命令"
        }
        CenterView::Diff => {
            "  q 離開  Q 關閉 daemon  j/k 選行  {/} hunk  c 審查註解  M 合併  [ ] 換 session  r 重整"
        }
        CenterView::Worktrees => "  q 離開  Q 關閉 daemon  M 合併選取  G 清理已結束  r 重整",
    };
    let text = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Span::raw(hints),
    ]);
    f.render_widget(Paragraph::new(text), area);
}

/// 逐字稿翻頁捲動：`dir < 0` 往上、`dir > 0` 往下；到底時恢復跟隨最新。
fn scroll_transcript(app: &mut App, dir: i32) {
    let page = app.transcript_viewport.max(1);
    let max_off = app.transcript_total.saturating_sub(app.transcript_viewport);
    let cur = match app.transcript_scroll {
        Some(o) => o.min(max_off),
        None => max_off,
    };
    let next = if dir < 0 {
        cur.saturating_sub(page)
    } else {
        (cur + page).min(max_off)
    };
    if next >= max_off {
        app.transcript_scroll = None;
        app.status = "逐字稿：跟隨最新".into();
    } else {
        app.transcript_scroll = Some(next);
        app.status = format!("逐字稿：第 {next} 行（End 回到最新）");
    }
}

/// 在 Diff 視圖跳到上一個／下一個 hunk（`@@` 行）。
fn jump_hunk(app: &mut App, forward: bool) {
    if app.view != CenterView::Diff || app.diff_lines.is_empty() {
        return;
    }
    let n = app.diff_lines.len();
    let cur = app.diff_cursor.min(n - 1);
    let is_hunk = |i: usize| app.diff_lines[i].starts_with("@@");
    if forward {
        if let Some(i) = ((cur + 1)..n).find(|&i| is_hunk(i)) {
            app.diff_cursor = i;
            return;
        }
    } else if let Some(i) = (0..cur).rev().find(|&i| is_hunk(i)) {
        app.diff_cursor = i;
        return;
    }
    app.status = "沒有更多 hunk".into();
}

/// 從 `@@ -a,b +c,d @@` 解析新舊檔起始行號。
fn parse_hunk(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("@@")?;
    let mut old = None;
    let mut new = None;
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix('-') {
            old = v.split(',').next().and_then(|s| s.parse::<u64>().ok());
        } else if let Some(v) = tok.strip_prefix('+') {
            new = v.split(',').next().and_then(|s| s.parse::<u64>().ok());
        }
        if old.is_some() && new.is_some() {
            break;
        }
    }
    Some((old?, new?))
}

/// 為每行 diff 標上（舊檔行號, 新檔行號）；非內容行則為 `None`。
fn build_gutter(lines: &[String]) -> Vec<(Option<u64>, Option<u64>)> {
    let mut out = Vec::with_capacity(lines.len());
    let (mut old, mut new) = (0u64, 0u64);
    for l in lines {
        if l.starts_with("@@") {
            if let Some((o, n)) = parse_hunk(l) {
                old = o;
                new = n;
            }
            out.push((None, None));
        } else if l.starts_with("+++")
            || l.starts_with("---")
            || l.starts_with("diff ")
            || l.starts_with("index ")
            || l.starts_with('\\')
        {
            out.push((None, None));
        } else if l.starts_with('+') {
            out.push((None, Some(new)));
            new += 1;
        } else if l.starts_with('-') {
            out.push((Some(old), None));
            old += 1;
        } else {
            out.push((Some(old), Some(new)));
            old += 1;
            new += 1;
        }
    }
    out
}

/// 統計 diff 的新增／刪除行數。
fn diff_stat(lines: &[String]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for l in lines {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if l.starts_with('+') {
            added += 1;
        } else if l.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ade_core::{Cost, Role, Session};
    use ratatui::backend::TestBackend;
    use std::collections::VecDeque;

    fn sample_app(lines: &[&str]) -> App {
        let mut app = App::new();
        let s = Session {
            id: "ses_test".into(),
            kind: AgentKind::OpenCode,
            label: "worker".into(),
            cwd: "/tmp".into(),
            branch: Some("ade/x".into()),
            role: Role::Worker,
            status: SessionStatus::Busy,
            model: None,
            cost: Cost::default(),
            parent: None,
            created_ms: 1,
            transport: "acp".into(),
            status_note: None,
        };
        app.state.sessions.insert(s.id.clone(), s);
        let deque: VecDeque<String> = lines.iter().map(|l| l.to_string()).collect();
        app.state.transcripts.insert("ses_test".into(), deque);
        app
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn render(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        buffer_text(&term)
    }

    #[test]
    fn transcript_renders_markdown_and_toggles_raw() {
        let mut app = sample_app(&[
            "# Title",
            "a **bold** word",
            "```rust",
            "let x = 1;",
            "```",
            "| a | b |",
            "|---|---|",
            "| 1 | 2 |",
        ]);

        let rendered = render(&mut app);
        assert!(rendered.contains("Title"), "heading text missing");
        assert!(rendered.contains("bold"), "bold text missing");
        assert!(!rendered.contains("**bold**"), "markdown not stripped");
        assert!(rendered.contains("let x = 1;"), "code block missing");
        assert!(rendered.contains("md"), "mode indicator missing");

        app.raw_md = true;
        let raw = render(&mut app);
        assert!(raw.contains("**bold**"), "raw mode should show markup");
    }

    #[test]
    fn diff_view_shows_gutter_and_stats() {
        let mut app = sample_app(&[]);
        app.view = CenterView::Diff;
        app.diff_lines = vec![
            "diff --git a/f b/f".into(),
            "--- a/f".into(),
            "+++ b/f".into(),
            "@@ -1,2 +1,3 @@".into(),
            " ctx".into(),
            "-old".into(),
            "+new".into(),
            "+extra".into(),
        ];
        app.diff_gutter = build_gutter(&app.diff_lines);
        let (added, removed) = diff_stat(&app.diff_lines);
        app.diff_added = added;
        app.diff_removed = removed;
        assert_eq!((added, removed), (2, 1));

        let text = render(&mut app);
        assert!(text.contains("+2 -1"), "diff stat missing");
        assert!(text.contains("new"), "added line missing");
    }

    #[test]
    fn scroll_transcript_pages_and_resumes_follow() {
        let mut app = sample_app(&[]);
        app.transcript_total = 100;
        app.transcript_viewport = 10; // 跟隨時 offset = 90

        scroll_transcript(&mut app, -1);
        assert_eq!(app.transcript_scroll, Some(80));

        scroll_transcript(&mut app, 1);
        assert_eq!(app.transcript_scroll, None, "到底應恢復跟隨");

        app.transcript_scroll = Some(0);
        scroll_transcript(&mut app, -1);
        assert_eq!(app.transcript_scroll, Some(0), "不應低於 0");
    }
}
