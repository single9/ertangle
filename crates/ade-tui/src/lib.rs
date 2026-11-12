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
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

/// 串流期間逐字稿最短重算間隔；其餘變動（換 session／寬度／模式）立即重算。
const MD_REBUILD_INTERVAL: Duration = Duration::from_millis(120);

/// 逐字稿渲染快取：只有來源／寬度／模式改變時才重算 Markdown。
struct MdCache {
    session: SessionId,
    /// 該 session 逐字稿的輕量指紋（行數、首行長、末行長）；其他 session 或
    /// 非逐字稿事件造成的 `state.seq` 變動不會讓快取失效。
    stamp: (usize, usize, usize),
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
    /// 輸入游標位置（以 Unicode 字元計，0 = 最前面）。
    input_cursor: usize,
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
    /// 是否顯示 agent 的思考行（`💭`）。預設隱藏，避免長篇思考拖慢渲染。
    show_thoughts: bool,
    md_cache: Option<MdCache>,
    /// 上次重算逐字稿的時間；串流期間用來節流（見 `MD_REBUILD_INTERVAL`）。
    last_rebuild: std::time::Instant,
    /// 因節流而延後的重算，下一輪迴圈需補畫。
    rebuild_pending: bool,
    /// 最近一次繪製時 Sessions 清單的區域（供滑鼠點選）。
    sessions_area: Rect,
    /// Sessions 清單目前捲動的起始索引（滑鼠點選需加此偏移）。
    sessions_offset: usize,
    /// 最近一次繪製時逐字稿的區域、捲動偏移與可用寬度（供滑鼠選取）。
    transcript_area: Rect,
    transcript_offset: usize,
    transcript_inner_w: usize,
    /// 滑鼠拖曳選取範圍：((起始行, 起始欄), (目前行, 目前欄))，
    /// 行是渲染後的絕對行索引，欄是顯示欄位。
    selection: Option<((usize, usize), (usize, usize))>,
    /// 是否擷取滑鼠（預設開啟，支援應用內拖曳選取；`F2` 關閉以使用終端原生選取）。
    mouse_capture: bool,
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
            input_cursor: 0,
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
            show_thoughts: false,
            md_cache: None,
            last_rebuild: std::time::Instant::now(),
            rebuild_pending: false,
            sessions_area: Rect::default(),
            sessions_offset: 0,
            transcript_area: Rect::default(),
            transcript_offset: 0,
            transcript_inner_w: 0,
            selection: None,
            mouse_capture: true,
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
fn set_mouse_capture(on: bool) -> Result<()> {
    let mut out = io::stdout();
    if on {
        execute!(out, EnableMouseCapture)?;
    } else {
        execute!(out, DisableMouseCapture)?;
    }
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
    let mut dirty = true;
    let mut last_draw = std::time::Instant::now();

    loop {
        while let Ok(frame) = rx.try_recv() {
            apply_frame(&mut app, frame);
            dirty = true;
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
                    handle_key(&mut app, writer, key)?;
                }
                CtEvent::Mouse(mouse) => handle_mouse(&mut app, writer, mouse)?,
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
                app.input_cursor = 0;
                app.status =
                    "命令：spawn / merge / diff / gc / sleep / wake / task / broadcast / copy / shutdown"
                        .into();
                return Ok(());
            }
            KeyCode::Char('k') => {
                app.mode = Mode::Jump;
                app.input.clear();
                app.input_cursor = 0;
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
                send(writer, &Request::WorktreeMerge { session: id })?;
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
        KeyCode::Char('/') => {
            app.mode = Mode::Command;
            app.input.clear();
            app.input_cursor = 0;
            app.status =
                "命令：spawn / merge / diff / gc / sleep / wake / task / broadcast / copy / shutdown"
                    .into();
        }
        KeyCode::Enter | KeyCode::Char('i') => {
            app.mode = Mode::Prompt;
            app.input.clear();
            app.input_cursor = 0;
        }
        KeyCode::Char('n') => {
            app.mode = Mode::Spawn;
            app.input.clear();
            app.input_cursor = 0;
            app.spawn_worktree = false;
            app.status = "輸入：<agent> <prompt>（Ctrl-W 切換 worktree）".into();
        }
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
        KeyCode::Esc => {
            app.selection = None;
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

fn handle_input_mode(app: &mut App, writer: &mut ConnStream, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input.clear();
            app.input_cursor = 0;
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.spawn_worktree = !app.spawn_worktree;
            app.status = format!(
                "worktree = {}",
                if app.spawn_worktree { "on" } else { "off" }
            );
        }
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
                Mode::Normal => {}
            }
        }
        _ => {}
    }
    Ok(())
}

/// 以 Unicode 字元索引取得對應的 byte offset（超出長度時回傳字串長度）。
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn insert_input(app: &mut App, c: char) {
    let b = byte_index(&app.input, app.input_cursor);
    app.input.insert(b, c);
    app.input_cursor += 1;
}

fn insert_str_input(app: &mut App, s: &str) {
    if s.is_empty() {
        return;
    }
    let b = byte_index(&app.input, app.input_cursor);
    app.input.insert_str(b, s);
    app.input_cursor += s.chars().count();
}

fn backspace_input(app: &mut App) {
    if app.input_cursor == 0 {
        return;
    }
    let b = byte_index(&app.input, app.input_cursor - 1);
    app.input.remove(b);
    app.input_cursor -= 1;
}

fn delete_input(app: &mut App) {
    let n = app.input.chars().count();
    if app.input_cursor >= n {
        return;
    }
    let b = byte_index(&app.input, app.input_cursor);
    app.input.remove(b);
}

/// 滑鼠事件：滾輪捲動中央視圖、左鍵點 Sessions 清單，或在逐字稿上拖曳選取。
fn handle_mouse(app: &mut App, writer: &mut ConnStream, mouse: MouseEvent) -> Result<()> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if app.view == CenterView::Diff {
                app.diff_cursor = app.diff_cursor.saturating_sub(3);
            } else {
                scroll_transcript_lines(app, -3);
            }
        }
        MouseEventKind::ScrollDown => {
            if app.view == CenterView::Diff {
                let max = app.diff_lines.len().saturating_sub(1);
                app.diff_cursor = (app.diff_cursor + 3).min(max);
            } else {
                scroll_transcript_lines(app, 3);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let area = app.sessions_area;
            let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
            if area.width > 0
                && area.height > 2
                && mouse.row < area.y + area.height - 1
                && area.contains(pos)
            {
                let idx = app.sessions_offset + mouse.row.saturating_sub(area.y + 1) as usize;
                if idx < app.session_ids().len() {
                    app.selected = idx;
                    app.selection = None;
                    if app.view == CenterView::Diff {
                        request_selected_diff(app, writer)?;
                    }
                }
                return Ok(());
            }
            // 逐字稿拖曳選取：只涵蓋中央區塊，不會選到左右側欄。
            if app.view == CenterView::Transcript {
                if let Some(p) = transcript_pos(app, mouse.column, mouse.row, false) {
                    app.selection = Some((p, p));
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.view == CenterView::Transcript => {
            if let (Some(sel), Some(p)) = (
                app.selection,
                transcript_pos(app, mouse.column, mouse.row, true),
            ) {
                app.selection = Some((sel.0, p));
                let ((l0, _), (l1, _)) = normalize_selection((sel.0, p));
                app.status = format!("選取第 {}–{} 行（放開滑鼠複製）", l0 + 1, l1 + 1);
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.view == CenterView::Transcript => {
            if let Some(sel) = app.selection {
                let ((l0, c0), (l1, c1)) = normalize_selection(sel);
                if (l0, c0) == (l1, c1) {
                    app.selection = None;
                } else {
                    let text = selection_text(app);
                    if text.is_empty() {
                        app.status = "選取範圍沒有文字".into();
                    } else {
                        match write_clipboard(&text) {
                            Ok(()) => {
                                let n = text.lines().count().max(1);
                                app.status =
                                    format!("已複製選取 {n} 行（OSC 52；Shift+拖曳可原生選取）");
                            }
                            Err(e) => app.status = format!("複製失敗：{e}"),
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// 把滑鼠座標映射成逐字稿的（絕對渲染行, 顯示欄）；`clamp` 時允許拖到區域外。
fn transcript_pos(app: &App, column: u16, row: u16, clamp: bool) -> Option<(usize, usize)> {
    let area = app.transcript_area;
    if area.width < 4 || area.height < 4 || app.transcript_total == 0 {
        return None;
    }
    let x0 = area.x + 1;
    let y0 = area.y + 1;
    let w = app.transcript_inner_w.max(1) as u16;
    let h = area.height.saturating_sub(2);
    let inside = column >= x0 && column < x0 + w && row >= y0 && row < y0 + h;
    if !inside && !clamp {
        return None;
    }
    let col = column.saturating_sub(x0) as usize;
    let rel_row = row.saturating_sub(y0) as usize;
    let col = col.min(w.saturating_sub(1) as usize);
    let rel_row = rel_row.min(h.saturating_sub(1) as usize);
    let line = app.transcript_offset.saturating_add(rel_row);
    Some((line.min(app.transcript_total - 1), col))
}

/// 讓選取範圍的起點在終點之前（行優先）。
fn normalize_selection(sel: ((usize, usize), (usize, usize))) -> ((usize, usize), (usize, usize)) {
    let (a, b) = sel;
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// 指定行在選取範圍內要標亮的顯示欄區間 `[from, to)`。
fn selection_columns(
    sel: ((usize, usize), (usize, usize)),
    line: usize,
    inner_w: usize,
) -> Option<(usize, usize)> {
    let ((l0, c0), (l1, c1)) = normalize_selection(sel);
    if line < l0 || line > l1 {
        return None;
    }
    let from = if line == l0 { c0 } else { 0 };
    let to = if line == l1 {
        (c1 + 1).min(inner_w)
    } else {
        inner_w
    };
    (to > from).then_some((from, to))
}

/// 依顯示欄位標亮一行中的選取區間（保留原本樣式）。
fn highlight_columns(line: &Line<'static>, from: usize, to: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    for span in &line.spans {
        let mut buf = String::new();
        let mut state: Option<bool> = None;
        for ch in span.content.chars() {
            let w = ch.width().unwrap_or(0);
            let selected = if w == 0 {
                state.unwrap_or(false)
            } else {
                col < to && col + w > from
            };
            if let Some(prev) = state {
                if prev != selected {
                    let style = select_style(span.style, prev);
                    spans.push(Span::styled(std::mem::take(&mut buf), style));
                    state = Some(selected);
                }
            } else {
                state = Some(selected);
            }
            buf.push(ch);
            col += w;
        }
        if let Some(selected) = state {
            spans.push(Span::styled(buf, select_style(span.style, selected)));
        }
    }
    Line::from(spans)
}

fn select_style(base: Style, selected: bool) -> Style {
    if selected {
        base.add_modifier(Modifier::REVERSED)
    } else {
        base
    }
}

/// 依顯示欄位切出字串片段（中文等寬字元以 2 欄計）。
fn slice_display(s: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for ch in s.chars() {
        if col >= to {
            break;
        }
        let w = ch.width().unwrap_or(0);
        if col + w > from {
            out.push(ch);
        }
        col += w;
    }
    out
}

/// 目前滑鼠選取範圍的純文字（只含中央逐字稿，不含側欄與邊框）。
fn selection_text(app: &App) -> String {
    let Some(sel) = app.selection else {
        return String::new();
    };
    let Some(cache) = &app.md_cache else {
        return String::new();
    };
    if cache.lines.is_empty() {
        return String::new();
    }
    let ((l0, c0), (l1, c1)) = normalize_selection(sel);
    let last = l1.min(cache.lines.len() - 1);
    let mut out: Vec<String> = Vec::new();
    for (i, line) in cache.lines.iter().enumerate().take(last + 1).skip(l0) {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let from = if i == l0 { c0 } else { 0 };
        let to = if i == last { c1 + 1 } else { usize::MAX };
        out.push(slice_display(&text, from, to).trim_end().to_string());
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// 貼上（bracketed paste）：只插入目前輸入框；換行轉成空白以維持單行輸入。
fn handle_paste(app: &mut App, text: &str) {
    if app.mode == Mode::Normal {
        return;
    }
    let cleaned: String = text
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    insert_str_input(app, &cleaned);
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
            format!("   think:{}", if app.show_thoughts { "on" } else { "off" }),
            Style::default().fg(if app.show_thoughts {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ),
        Span::styled(
            format!("   mouse:{}", if app.mouse_capture { "on" } else { "off" }),
            Style::default().fg(if app.mouse_capture {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
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

fn draw_sessions(f: &mut TuiFrame, area: Rect, app: &mut App) {
    app.sessions_area = area;
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
    app.sessions_offset = list_state.offset();
}

/// 逐字稿指紋：串流增量會改變末行長度或行數，滿額截斷會改變首行。
fn transcript_stamp(app: &App, id: &SessionId) -> (usize, usize, usize) {
    app.state.transcripts.get(id).map_or((0, 0, 0), |b| {
        (
            b.len(),
            b.front().map_or(0, String::len),
            b.back().map_or(0, String::len),
        )
    })
}

/// 依目前模式（Markdown / 原始）與寬度，把逐字稿建成樣式化且已換行的行。
fn build_transcript_lines(app: &App, id: &SessionId, width: usize) -> Vec<Line<'static>> {
    let buf = match app.state.transcripts.get(id) {
        Some(b) => b,
        None => return Vec::new(),
    };
    let src: Vec<String> = buf
        .iter()
        .filter(|l| app.show_thoughts || !l.starts_with("💭 "))
        .cloned()
        .collect();
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

    let mut need_rebuild = match (&app.md_cache, &selected) {
        (Some(c), Some(id)) => {
            c.session != *id
                || c.raw != app.raw_md
                || c.width != area.width
                || (c.stamp != transcript_stamp(app, id) && {
                    // 只有內容增量：節流，避免串流時每個 delta 都重算整份。
                    let due = app.last_rebuild.elapsed() >= MD_REBUILD_INTERVAL;
                    app.rebuild_pending = !due;
                    due
                })
        }
        _ => true,
    };
    if selected.is_none() && app.md_cache.is_none() {
        need_rebuild = false;
    }
    if need_rebuild {
        match &selected {
            Some(id) => {
                if app.md_cache.as_ref().map(|c| &c.session) != Some(id) {
                    app.transcript_scroll = None; // 換 session 時回到跟隨
                    app.selection = None;
                }
                let lines = build_transcript_lines(app, id, inner_w);
                app.last_rebuild = std::time::Instant::now();
                app.rebuild_pending = false;
                app.md_cache = Some(MdCache {
                    session: id.clone(),
                    stamp: transcript_stamp(app, id),
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
    app.transcript_area = area;
    app.transcript_inner_w = inner_w;

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

    let selection = app.selection;
    let mut visible: Vec<Line> = Vec::new();
    if let Some(c) = &app.md_cache {
        for (idx, line) in c.lines.iter().enumerate().skip(off).take(viewport) {
            let line = match selection.and_then(|sel| selection_columns(sel, idx, inner_w)) {
                Some((from, to)) => highlight_columns(line, from, to),
                None => line.clone(),
            };
            visible.push(line);
        }
    }
    if visible.is_empty() {
        visible.push(Line::styled(
            "這裡會顯示 agent 的輸出。按 i 輸入 prompt、n 建立 session、g 切換 Diff 視圖。",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let para = Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, area);
    app.transcript_offset = off;

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
            " 輸入（i=prompt/@peer/#ch, n=spawn, /=命令, Ctrl-K=跳轉, g=Diff, z=休眠, t=原始/渲染, T=思考, 拖曳=選取複製, PgUp/PgDn=捲動, Y=複製, F2=滑鼠） "
        }
        Mode::Prompt => " prompt（可輸入 @peer 訊息、#channel 訊息、/命令，Enter 送出） ",
        Mode::Spawn => " spawn：<agent> <prompt>（Ctrl-W 切換 worktree, Enter 送出） ",
        Mode::Message => " message → 選取的 session（Enter 送出） ",
        Mode::Review => " review 註解 → 送回選取的 session 修改（Enter 送出） ",
        Mode::Command => " 命令面板（spawn / merge / diff / gc / sleep / wake / task / copy / shutdown） ",
        Mode::Jump => " Jump Anywhere：輸入關鍵字跳轉 session（Enter 確認） ",
    };
    let style = if app.mode == Mode::Normal {
        Style::default()
    } else {
        Style::default().fg(Color::Cyan)
    };

    // 水平捲動：讓游標永遠留在可見範圍內（支援中文等寬字元）。
    let inner_w = area.width.saturating_sub(2) as usize;
    let chars: Vec<char> = app.input.chars().collect();
    let cursor = app.input_cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let mut cursor_col = UnicodeWidthStr::width(before.as_str());
    let mut start = 0usize;
    while inner_w > 0 && cursor_col >= inner_w && start < cursor {
        let w = chars[start].width().unwrap_or(0).max(1);
        cursor_col = cursor_col.saturating_sub(w);
        start += 1;
    }
    let visible: String = chars[start..].iter().collect();

    let para = Paragraph::new(visible)
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, area);

    // 把終端游標放到輸入框內，IME 的 preedit 就會錨定在這裡。
    if area.width > 2 && area.height > 2 {
        let x = area
            .x
            .saturating_add(1)
            .saturating_add(cursor_col as u16)
            .min(area.x + area.width.saturating_sub(2));
        f.set_cursor_position((x, area.y + 1));
    }
}

fn draw_status(f: &mut TuiFrame, area: Rect, app: &App) {
    let hints = match app.view {
        CenterView::Transcript => {
            "  q 離開(保留 daemon)  Q 關閉 daemon  Tab 切換  i prompt  n spawn  t 原始/渲染  T 思考  PgUp/PgDn 捲動  / 命令"
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

/// 取得中央主要區塊的純文字內容（不含 Sessions / Bus 側欄與邊框）。
fn main_pane_text(app: &App) -> String {
    match app.view {
        CenterView::Transcript => app
            .selected_id()
            .and_then(|id| app.state.transcripts.get(&id))
            .map(|buf| {
                buf.iter()
                    .filter(|l| app.show_thoughts || !l.starts_with("💭 "))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        CenterView::Diff => app.diff_lines.join("\n"),
        CenterView::Worktrees => app
            .worktrees
            .iter()
            .map(|w| {
                format!(
                    "{} @{} {} {} ({})",
                    w.label,
                    w.branch,
                    if w.live { "live" } else { "exited" },
                    w.summary,
                    w.path
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// 用 OSC 52 把文字寫進終端剪貼簿（SSH / tmux 下也可用，不需終端原生選取）。
fn write_clipboard(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    write!(out, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    out.flush()
}

/// 最小 base64 編碼器（OSC 52 需要；避免多拉一個依賴）。
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((chunk[0] as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 複製中央主要區塊（`Y` 或 `/copy`），成功後在狀態列回報行數。
fn copy_main_pane(app: &mut App) {
    let text = main_pane_text(app);
    if text.is_empty() {
        app.status = "沒有可複製的內容".into();
        return;
    }
    match write_clipboard(&text) {
        Ok(()) => {
            let lines = text.lines().count().max(1);
            app.status = format!("已複製主要區塊 {lines} 行（OSC 52；失敗時可用 Shift+拖曳選取）");
        }
        Err(e) => app.status = format!("複製失敗：{e}"),
    }
}

/// 切換是否顯示 agent 的思考行（`💭`）。
fn toggle_thoughts(app: &mut App) {
    app.show_thoughts = !app.show_thoughts;
    app.md_cache = None;
    app.selection = None;
    app.status = if app.show_thoughts {
        "思考顯示：開（顯示 💭 行）".into()
    } else {
        "思考顯示：關（已隱藏 💭 行，按 T 顯示）".into()
    };
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

/// 以「行」為單位捲動（滑鼠滾輪用），到底時恢復跟隨最新。
fn scroll_transcript_lines(app: &mut App, delta: i32) {
    let max_off = app.transcript_total.saturating_sub(app.transcript_viewport);
    let cur = app.transcript_scroll.unwrap_or(max_off).min(max_off);
    let next = if delta < 0 {
        cur.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        (cur + delta as usize).min(max_off)
    };
    if next >= max_off {
        app.transcript_scroll = None;
    } else {
        app.transcript_scroll = Some(next);
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
            repo_root: None,
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

    #[test]
    fn scroll_transcript_lines_moves_by_rows() {
        let mut app = sample_app(&[]);
        app.transcript_total = 100;
        app.transcript_viewport = 10;

        scroll_transcript_lines(&mut app, -3);
        assert_eq!(app.transcript_scroll, Some(87));
        scroll_transcript_lines(&mut app, 3);
        assert_eq!(app.transcript_scroll, None, "回到最新應恢復跟隨");
        scroll_transcript_lines(&mut app, -1000);
        assert_eq!(app.transcript_scroll, Some(0), "不應低於 0");
    }

    #[test]
    fn user_prompt_is_echoed_in_transcript() {
        let mut app = sample_app(&[]);
        app.state.apply(&ade_core::Record {
            seq: 1,
            ts: 1,
            event: ade_core::Event::UserPrompt {
                session: "ses_test".into(),
                text: "fix the tests\nsecond line 中文".into(),
            },
        });

        // CJK 是雙寬字元，TestBackend 會在中間留下空白格，因此用 ASCII 檢查版面。
        let text = render(&mut app);
        assert!(text.contains("❯ fix the tests"), "prompt marker missing");
        assert!(text.contains("second line"), "continuation line missing");

        let buf = &app.state.transcripts["ses_test"];
        assert_eq!(buf[0], "❯ fix the tests");
        assert_eq!(buf[1], "  second line 中文");
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode("註".as_bytes()), "6Ki7");
    }

    #[test]
    fn main_pane_text_excludes_sidebars() {
        let mut app = sample_app(&["hello", "world"]);
        assert_eq!(main_pane_text(&app), "hello\nworld");

        app.view = CenterView::Diff;
        app.diff_lines = vec!["+added".into(), "-removed".into()];
        assert_eq!(main_pane_text(&app), "+added\n-removed");
    }

    #[test]
    fn input_editing_respects_unicode_cursor() {
        let mut app = App::new();
        app.mode = Mode::Prompt;
        insert_str_input(&mut app, "你好");
        assert_eq!(app.input, "你好");
        assert_eq!(app.input_cursor, 2);

        app.input_cursor = 1;
        insert_input(&mut app, 'X');
        assert_eq!(app.input, "你X好");
        assert_eq!(app.input_cursor, 2);

        backspace_input(&mut app);
        assert_eq!(app.input, "你好");
        assert_eq!(app.input_cursor, 1);

        delete_input(&mut app);
        assert_eq!(app.input, "你");

        app.input_cursor = 0;
        backspace_input(&mut app);
        assert_eq!(app.input, "你", "最前面按退格不應改動");
    }

    #[test]
    fn input_cursor_is_placed_inside_the_box() {
        let mut app = sample_app(&[]);
        app.mode = Mode::Prompt;
        app.input = "你好".into();
        app.input_cursor = 2;

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let pos = term.get_cursor_position().unwrap();
        // 輸入框在倒數第二列；內框起點為 (1, 37)，游標應落在「你好」之後。
        assert_eq!((pos.x, pos.y), (5, 37));
    }

    #[test]
    fn thinking_lines_are_hidden_by_default() {
        let mut app = sample_app(&["💭 planning", "answer", "💭 more", "done"]);
        let id: SessionId = "ses_test".into();

        let lines = build_transcript_lines(&app, &id, 80);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("planning"), "thought leaked: {text}");
        assert!(!text.contains("more"), "thought leaked: {text}");
        assert!(text.contains("answer"));
        assert!(text.contains("done"));
        assert!(!main_pane_text(&app).contains("planning"));

        app.show_thoughts = true;
        let lines = build_transcript_lines(&app, &id, 80);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("planning"));
        assert!(text.contains("more"));
        assert!(main_pane_text(&app).contains("planning"));
    }

    fn cache_lines(lines: Vec<Line<'static>>) -> MdCache {
        MdCache {
            session: "ses_test".into(),
            stamp: (0, 0, 0),
            raw: false,
            width: 80,
            lines,
        }
    }

    #[test]
    fn selection_text_uses_display_columns() {
        let mut app = sample_app(&[]);
        app.md_cache = Some(cache_lines(vec![
            Line::raw("hello 你好 world"),
            Line::raw("second"),
        ]));

        // 「你好」從顯示欄 6 開始，各佔 2 欄。
        app.selection = Some(((0, 6), (0, 9)));
        assert_eq!(selection_text(&app), "你好");

        // 跨行：第一行尾端到第二行開頭。
        app.selection = Some(((0, 11), (1, 1)));
        assert_eq!(selection_text(&app), "world\nse");

        // 反向拖曳（由下往上）也要正常。
        app.selection = Some(((1, 1), (0, 11)));
        assert_eq!(selection_text(&app), "world\nse");
    }

    #[test]
    fn highlight_columns_marks_only_selected_range() {
        let out = highlight_columns(&Line::raw("abcd"), 1, 3);
        let spans: Vec<(String, bool)> = out
            .spans
            .iter()
            .map(|s| {
                (
                    s.content.to_string(),
                    s.style.add_modifier.contains(Modifier::REVERSED),
                )
            })
            .collect();
        assert_eq!(
            spans,
            vec![
                ("a".to_string(), false),
                ("bc".to_string(), true),
                ("d".to_string(), false),
            ]
        );
    }

    #[test]
    fn selection_columns_cover_full_middle_lines() {
        let sel = ((2, 5), (4, 1));
        assert_eq!(selection_columns(sel, 1, 40), None);
        assert_eq!(selection_columns(sel, 2, 40), Some((5, 40)));
        assert_eq!(selection_columns(sel, 3, 40), Some((0, 40)));
        assert_eq!(selection_columns(sel, 4, 40), Some((0, 2)));
        assert_eq!(selection_columns(sel, 5, 40), None);
    }

    #[test]
    fn mouse_selection_copies_only_middle_pane() {
        let mut app = sample_app(&["alpha", "beta", "gamma"]);
        let _ = render(&mut app); // 先畫一次，建立 md_cache 與 transcript_area
        app.selection = Some(((1, 0), (1, 3)));
        let text = selection_text(&app);
        assert_eq!(text, "beta");
        assert!(!text.contains("Sessions"), "side pane leaked: {text}");

        let rendered = render(&mut app);
        assert!(rendered.contains("beta"));
    }

    #[test]
    fn transcript_pos_maps_rows_and_columns() {
        let mut app = sample_app(&[]);
        app.transcript_area = Rect::new(0, 0, 40, 10);
        app.transcript_inner_w = 37;
        app.transcript_total = 100;
        app.transcript_offset = 5;

        // 內框起點是 (1, 1)，所以 (3, 2) → 行 6、欄 2。
        assert_eq!(transcript_pos(&app, 3, 2, false), Some((6, 2)));
        // 區域外不啟動選取。
        assert_eq!(transcript_pos(&app, 39, 2, false), None);
        // 拖曳出界時夾在可見範圍。
        assert_eq!(transcript_pos(&app, 3, 99, true), Some((12, 2)));
        assert_eq!(transcript_pos(&app, 0, 2, true), Some((6, 0)));
    }
}
