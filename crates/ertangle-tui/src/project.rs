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
        if let Some(p) = app.state.sessions[id]
            .parent
            .as_deref()
            .filter(|p| in_scope.contains(p))
        {
            children.entry(p).or_default().push(id.as_str());
            has_parent.insert(id.as_str());
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
        let s = &app.state.sessions[id];
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
        let s = &app.state.sessions[id];
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
            let mut out = split(rows[0], Direction::Horizontal, 2);
            if n == 3 {
                out.push(rows[1]);
            } else {
                out.extend(split(rows[1], Direction::Horizontal, 2));
            }
            out
        }
    }
}

pub(crate) fn draw_grid(f: &mut TuiFrame, area: Rect, app: &mut App) {
    let ids = scoped_ids(app);
    app.transcript_area = Rect::default();
    if ids.is_empty() {
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
    let rects = grid_rects(area, n);
    for (k, rect) in rects.iter().enumerate() {
        let id = &ids[start + k];
        let s = &app.state.sessions[id];
        let selected = start + k == sel;
        let inner_w = rect.width.saturating_sub(2).max(8) as usize;
        let (all, _) = build_transcript_lines(app, id, inner_w);
        let h = rect.height.saturating_sub(2) as usize;
        let tail: Vec<Line<'static>> = all[all.len().saturating_sub(h)..].to_vec();
        let mut title = format!(" {} {}", s.status.glyph(), s.label);
        if let Some(t) = s.active_tools.last() {
            title.push_str(&format!(
                " · {}:{}",
                if t.is_subagent { "sub" } else { "run" },
                truncate(&t.name, 14)
            ));
        }
        if k == 0 && pages > 1 {
            title.push_str(&format!(" · {}/{}", start / GRID_PAGE + 1, pages));
        }
        title.push(' ');
        let border = if selected {
            Style::default()
                .fg(Color::Cyan)
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
