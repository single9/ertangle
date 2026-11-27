//! 右欄「Project」面板與中央「Grid」多 session 檢視。

use super::*;
use ertangle_core::{Role, SessionStatus, TaskStatus};
use std::collections::{HashMap, HashSet};
use tr::tr;

/// Grid 一頁最多顯示的 session 數。
pub(crate) const GRID_PAGE: usize = 4;

/// 目前專案範圍內 (依 `scope_here`) 的 session id，順序同左側清單。
fn scoped_ids(app: &App) -> Vec<SessionId> {
    app.visible_sessions()
        .into_iter()
        .filter(|(h, _)| *h == app.active_host)
        .map(|(_, id)| id)
        .filter(|id| app.state.sessions.contains_key(id))
        .collect()
}

fn fmt_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn section(title: String) -> Line<'static> {
    Line::styled(
        title,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
}

fn status_color(s: SessionStatus) -> Color {
    match s {
        SessionStatus::Failed => Color::Red,
        SessionStatus::Busy => Color::Green,
        SessionStatus::Sleeping => Color::Blue,
        SessionStatus::Idle => Color::Gray,
        SessionStatus::WaitingApproval => Color::Yellow,
        _ => Color::DarkGray,
    }
}

/// 組出 Project 面板內容。`now_ms` 由呼叫端傳入，方便測試。
pub(crate) fn project_panel_lines(app: &App, now_ms: u64) -> Vec<Line<'static>> {
    let ids = scoped_ids(app);
    let in_scope: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 每個 session 持有的進行中任務 (doing／blocked／review)。
    let mut held: HashMap<&str, Vec<&ertangle_core::Task>> = HashMap::new();
    for t in app.state.tasks.values() {
        if !matches!(
            t.status,
            TaskStatus::Doing | TaskStatus::Blocked | TaskStatus::Review
        ) {
            continue;
        }
        if let Some(a) = t.assignee.as_deref().filter(|a| in_scope.contains(a)) {
            held.entry(a).or_default().push(t);
        }
    }
    for v in held.values_mut() {
        v.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
    }

    // Collaboration：有親子關係或持有任務的 session，依 parent 排成樹。
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut has_parent: HashSet<&str> = HashSet::new();
    for id in &ids {
        if let Some(s) = app.state.sessions.get(id) {
            if let Some(p) = s.parent.as_deref().filter(|p| in_scope.contains(p)) {
                children.entry(p).or_default().push(id.as_str());
                has_parent.insert(id.as_str());
            }
        }
    }
    let participates =
        |id: &str| has_parent.contains(id) || children.contains_key(id) || held.contains_key(id);
    let mut collab: Vec<Line<'static>> = Vec::new();
    fn walk(
        app: &App,
        id: &str,
        depth: usize,
        children: &HashMap<&str, Vec<&str>>,
        held: &HashMap<&str, Vec<&ertangle_core::Task>>,
        out: &mut Vec<Line<'static>>,
    ) {
        let Some(s) = app.state.sessions.get(id) else {
            return;
        };
        let indent = "  ".repeat(depth);
        let mut spans = vec![
            Span::styled(
                format!(
                    "{indent}{}{} ",
                    if depth > 0 { "↳ " } else { "" },
                    s.status.glyph()
                ),
                Style::default().fg(status_color(s.status)),
            ),
            Span::raw(truncate(&s.label, 22)),
        ];
        if s.role != Role::Worker || s.parent.is_some() {
            spans.push(Span::styled(
                format!(" [{}]", s.role.id()),
                Style::default().fg(Color::Magenta),
            ));
        }
        if let Some(note) = s.status_note.as_deref().filter(|n| !n.is_empty()) {
            spans.push(Span::styled(format!(" · {}", truncate(note, 24)), dim()));
        }
        out.push(Line::from(spans));
        for t in held.get(id).into_iter().flatten() {
            out.push(Line::from(vec![
                Span::raw(format!("{indent}   ")),
                Span::styled(
                    format!("● {}: ", t.status.id()),
                    Style::default().fg(crate::tasks::status_color(t.status)),
                ),
                Span::raw(truncate(&t.title, 28)),
            ]));
        }
        for c in children.get(id).into_iter().flatten() {
            walk(app, c, depth + 1, children, held, out);
        }
    }
    for id in &ids {
        if !has_parent.contains(id.as_str()) && participates(id) {
            walk(app, id, 0, &children, &held, &mut collab);
        }
    }
    if !collab.is_empty() {
        lines.push(section(tr!("協作")));
        lines.extend(collab);
    }

    // Running：進行中的工具／Subagent 與背景處理程序。
    let mut running: Vec<Line<'static>> = Vec::new();
    for id in &ids {
        let Some(s) = app.state.sessions.get(id) else {
            continue;
        };
        for t in &s.active_tools {
            let tag = if t.is_subagent { "sub" } else { "run" };
            let secs = now_ms.saturating_sub(t.started_ms) / 1000;
            let mut spans = vec![
                Span::styled(
                    format!("{tag}:{} ", truncate(&t.name, 14)),
                    Style::default().fg(if t.is_subagent {
                        Color::Magenta
                    } else {
                        Color::Cyan
                    }),
                ),
                Span::raw(truncate(&s.label, 12)),
            ];
            if !t.summary.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", truncate(&t.summary, 24)),
                    dim(),
                ));
            }
            spans.push(Span::styled(format!(" {}", fmt_elapsed(secs)), dim()));
            running.push(Line::from(spans));
        }
        let idle = s.status == SessionStatus::Idle;
        for p in &s.bg_procs {
            running.push(Line::from(vec![
                Span::styled(
                    format!("bg:{} ", p.pid),
                    Style::default().fg(if idle { Color::Yellow } else { Color::Gray }),
                ),
                Span::raw(truncate(&p.name, 14)),
                Span::styled(format!(" {}", truncate(&s.label, 12)), dim()),
            ]));
        }
    }
    if !running.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(section(tr!("執行中")));
        lines.extend(running);
    }

    // 最新的一兩則 Bus 訊息。
    let msgs = app.visible_messages();
    if !msgs.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(section(tr!("最新訊息")));
        let name = |sid: &str| {
            app.state
                .sessions
                .get(sid)
                .map(|s| truncate(&s.label, 12))
                .unwrap_or_else(|| short(sid))
        };
        for m in msgs.iter().skip(msgs.len().saturating_sub(2)) {
            let to = match &m.to {
                Target::Direct(s) => format!("@{}", name(s)),
                Target::Channel(c) => format!("#{c}"),
                Target::Broadcast => "*".into(),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}→{} ", name(&m.from), to),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(truncate(&m.body, 40)),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            tr!("目前沒有協作或執行中的項目。b 切換 Bus。"),
            dim(),
        ));
    }
    lines
}

pub(crate) fn draw_project(f: &mut TuiFrame, area: Rect, app: &App) {
    let lines = project_panel_lines(app, ertangle_core::now_millis());
    let title = if app.scope_here {
        format!(" Project · {} ", project_name(&app.home_project, &[]))
    } else {
        tr!(" Project · 全部專案 ")
    };
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

/// 包含選取項目的那一頁：回傳 (起始索引, 該頁筆數, 總頁數)。
pub(crate) fn grid_page(selected: usize, total: usize) -> (usize, usize, usize) {
    if total == 0 {
        return (0, 0, 1);
    }
    let selected = selected.min(total - 1);
    let start = selected / GRID_PAGE * GRID_PAGE;
    (
        start,
        (total - start).min(GRID_PAGE),
        total.div_ceil(GRID_PAGE),
    )
}

/// 依窗格數與寬度切出格子：1 → 全版；2 → 寬時左右、窄時上下；3 → 上兩下一；4 → 2x2。
pub(crate) fn grid_rects(area: Rect, n: usize) -> Vec<Rect> {
    let split = |a: Rect, dir: Direction, k: u32| -> Vec<Rect> {
        Layout::default()
            .direction(dir)
            .constraints(vec![Constraint::Ratio(1, k); k as usize])
            .split(a)
            .to_vec()
    };
    match n {
        0 => Vec::new(),
        1 => vec![area],
        2 => {
            let dir = if area.width >= 100 {
                Direction::Horizontal
            } else {
                Direction::Vertical
            };
            split(area, dir, 2)
        }
        _ => {
            let rows = split(area, Direction::Vertical, 2);
            let first_row = rows.first().copied().unwrap_or(area);
            let mut out = split(first_row, Direction::Horizontal, 2);
            if n == 3 {
                if let Some(&second_row) = rows.get(1) {
                    out.push(second_row);
                }
            } else if let Some(&second_row) = rows.get(1) {
                out.extend(split(second_row, Direction::Horizontal, 2));
            }
            out
        }
    }
}

/// Returns cached transcript lines for a Grid pane, rebuilding only when the
/// session transcript, active tool count, or render settings change.
fn grid_cell_lines(app: &mut App, id: &SessionId, inner_w: usize) -> Vec<Line<'static>> {
    let stamp = crate::draw::transcript_stamp(app, id);
    let active_tools_len = app
        .state
        .sessions
        .get(id)
        .map_or(0, |s| s.active_tools.len());
    if let Some(c) = app.grid_cache.get(id) {
        if c.stamp == stamp
            && c.width == inner_w
            && c.raw == app.raw_md
            && c.show_tools == app.show_tools
            && c.show_thoughts == app.show_thoughts
            && c.active_tools_len == active_tools_len
        {
            return c.lines.clone();
        }
    }
    let (lines, _) = build_transcript_lines(app, id, inner_w);
    app.grid_cache.insert(
        id.clone(),
        crate::app::GridCellCache {
            stamp,
            width: inner_w,
            raw: app.raw_md,
            show_tools: app.show_tools,
            show_thoughts: app.show_thoughts,
            active_tools_len,
            lines: lines.clone(),
        },
    );
    lines
}

fn has_pending_question(app: &App, id: &SessionId) -> bool {
    app.state
        .pending_questions
        .values()
        .any(|q| &q.session == id)
}

/// Formats a compact monitoring title for a Grid pane.
fn grid_cell_title(
    app: &App,
    s: &Session,
    slot: usize,
    page_info: Option<(usize, usize)>,
) -> String {
    let mut title = format!(" [{}] {} {}", slot + 1, s.status.glyph(), s.label);
    if has_pending_question(app, &s.id) {
        title.push_str(" [?]");
    } else if s.status == SessionStatus::WaitingApproval {
        title.push_str(" [!]");
    }
    if let Some(t) = s.active_tools.last() {
        title.push_str(&format!(
            " · {}:{}",
            if t.is_subagent { "sub" } else { "run" },
            truncate(&t.name, 14)
        ));
    } else if let Some(task) = app
        .state
        .tasks
        .values()
        .find(|t| t.assignee.as_deref() == Some(s.id.as_str()) && t.status == TaskStatus::Doing)
    {
        title.push_str(&format!(" · {}", truncate(&task.title, 18)));
    } else if let Some(note) = s.status_note.as_deref().filter(|n| !n.is_empty()) {
        title.push_str(&format!(" · {}", truncate(note, 18)));
    }
    if let Some(ctx) = &s.context {
        if let Some(window) = ctx.window.filter(|&w| w > 0) {
            let pct = (ctx.used.saturating_mul(100) / window).min(100);
            title.push_str(&format!(" · {pct}%"));
        }
    }
    if let Some((cur, total)) = page_info {
        title.push_str(&format!(" · {cur}/{total}"));
    }
    title.push(' ');
    title
}

/// Selects the `slot`-th (0-based) visible pane on the current Grid page.
pub(crate) fn select_grid_slot(app: &mut App, slot: usize) -> bool {
    let ids = scoped_ids(app);
    let sel_id = app.selected_id();
    let sel = ids
        .iter()
        .position(|i| Some(i) == sel_id.as_ref())
        .unwrap_or(0);
    let (start, n, _) = grid_page(sel, ids.len());
    if slot >= n {
        return false;
    }
    let Some(target_id) = ids.get(start + slot).cloned() else {
        return false;
    };
    select_session_by_id(app, &target_id)
}

/// Selects a session in `app.session_ids()` by ID.
pub(crate) fn select_session_by_id(app: &mut App, id: &SessionId) -> bool {
    let Some(idx) = app.session_ids().iter().position(|s| s == id) else {
        return false;
    };
    app.selected = idx;
    app.sidebar_focus = None;
    app.selection = None;
    app.sync_active_host_from_selection();
    true
}

pub(crate) fn draw_grid(f: &mut TuiFrame, area: Rect, app: &mut App) {
    let ids = scoped_ids(app);
    app.transcript_area = Rect::default();
    app.grid_areas.clear();
    if ids.is_empty() {
        app.grid_cache.clear();
        f.render_widget(
            Paragraph::new(tr!("尚無 session：按 n 建立，或直接輸入 prompt"))
                .block(Block::default().borders(Borders::ALL).title(" Grid ")),
            area,
        );
        return;
    }
    let sel_id = app.selected_id();
    let sel = ids
        .iter()
        .position(|i| Some(i) == sel_id.as_ref())
        .unwrap_or(0);
    let (start, n, pages) = grid_page(sel, ids.len());
    let visible_ids: Vec<SessionId> = ids.get(start..start + n).unwrap_or(&[]).to_vec();
    app.grid_cache.retain(|k, _| visible_ids.contains(k));
    let rects = grid_rects(area, n);
    for (k, rect) in rects.iter().enumerate() {
        let Some(id) = visible_ids.get(k).cloned() else {
            continue;
        };
        let Some(s) = app.state.sessions.get(&id).cloned() else {
            continue;
        };
        app.grid_areas.push((*rect, id.clone()));
        let selected = start + k == sel;
        let inner_w = rect.width.saturating_sub(2).max(8) as usize;
        let all = grid_cell_lines(app, &id, inner_w);
        let h = rect.height.saturating_sub(2) as usize;
        let tail_slice = all.get(all.len().saturating_sub(h)..).unwrap_or(&[]);
        let tail: Vec<Line<'static>> = tail_slice.to_vec();
        let page_info = (k == 0 && pages > 1).then_some((start / GRID_PAGE + 1, pages));
        let title = grid_cell_title(app, &s, k, page_info);
        let has_question = has_pending_question(app, &id);
        let border = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if has_question || s.status == SessionStatus::WaitingApproval {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(status_color(s.status))
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title);
        f.render_widget(Paragraph::new(tail).block(block), *rect);
    }
}

/// Grid／單一逐字稿互相切換。
pub(crate) fn toggle_grid(app: &mut App) {
    app.selection = None;
    app.view = if app.view == CenterView::Grid {
        CenterView::Transcript
    } else {
        CenterView::Grid
    };
}
