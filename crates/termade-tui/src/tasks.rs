//! 任務視圖 (T18)：依狀態分組的任務清單、選取任務的說明，以及游標／跳轉／改狀態。

use crate::hosts::HostRouter;
use anyhow::Result;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame as TuiFrame;
use termade_core::{Task, TaskId, TaskStatus};
use termade_protocol::Request;
use tr::tr;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{short, App, CenterView};

/// 分組的顯示順序：正在進行的排最前，完成的排最後。
const GROUPS: [TaskStatus; 5] = [
    TaskStatus::Doing,
    TaskStatus::Review,
    TaskStatus::Todo,
    TaskStatus::Blocked,
    TaskStatus::Done,
];

pub(crate) fn status_color(s: TaskStatus) -> Color {
    match s {
        TaskStatus::Done => Color::Green,
        TaskStatus::Doing | TaskStatus::Review => Color::Cyan,
        TaskStatus::Blocked => Color::Red,
        TaskStatus::Todo => Color::Gray,
    }
}

/// 任務是否屬於目前的專案範圍。`Task` 沒有專案欄位，用負責人或建立者的 session 判斷；
/// 兩者都對不到 session (人建立、還沒指派，或 session 已移除)的任務一律顯示。
fn in_scope(app: &App, t: &Task) -> bool {
    if !app.scope_here {
        return true;
    }
    let mut roots = [&t.assignee, &t.created_by]
        .into_iter()
        .flatten()
        .filter_map(|id| app.state.sessions.get(id))
        .map(|s| s.project_root())
        .peekable();
    roots.peek().is_none() || roots.any(|r| r == app.home_project)
}

/// 任務視圖裡的任務 id，順序與畫面一致：依狀態分組，組內最近更新的在前。
pub(crate) fn visible_task_ids(app: &App) -> Vec<TaskId> {
    let mut v: Vec<&Task> = app
        .state
        .tasks
        .values()
        .filter(|t| in_scope(app, t))
        .collect();
    v.sort_by_key(|t| {
        let group = GROUPS.iter().position(|g| *g == t.status).unwrap_or(0);
        (group, std::cmp::Reverse(t.updated_ms))
    });
    v.into_iter().map(|t| t.id.clone()).collect()
}

/// 標題列的任務數：只算還沒完成的。
pub(crate) fn open_task_count(app: &App) -> usize {
    app.state
        .tasks
        .values()
        .filter(|t| t.status != TaskStatus::Done)
        .count()
}

pub(crate) fn selected_task(app: &App) -> Option<TaskId> {
    let ids = visible_task_ids(app);
    ids.get(app.task_cursor.min(ids.len().saturating_sub(1)))
        .cloned()
}

/// 移動任務游標 (鍵盤與滑鼠滾輪共用)。
pub(crate) fn move_task_cursor(app: &mut App, delta: i32) {
    let n = visible_task_ids(app).len();
    if n == 0 {
        app.task_cursor = 0;
        return;
    }
    let cur = app.task_cursor.min(n - 1);
    app.task_cursor = if delta < 0 {
        cur.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        (cur + delta as usize).min(n - 1)
    };
}

/// `Enter`：選取任務負責人的 session 並切回逐字稿。
pub(crate) fn jump_to_assignee(app: &mut App) {
    let Some(task) = selected_task(app).and_then(|id| app.state.tasks.get(&id)) else {
        app.status = tr!("沒有選取的任務");
        return;
    };
    let Some(assignee) = task.assignee.clone() else {
        app.status = tr!("這個任務還沒有負責人");
        return;
    };
    if !app.state.sessions.contains_key(&assignee) {
        app.status = tr!("負責人的 session ({id})已不存在", id = short(&assignee));
        return;
    }
    // 負責人在別的專案時，先改成顯示全部專案才選得到。
    if !app.session_ids().contains(&assignee) {
        app.scope_here = false;
    }
    if let Some(idx) = app.session_ids().iter().position(|s| *s == assignee) {
        app.selected = idx;
        app.view = CenterView::Transcript;
        app.selection = None;
        app.md_cache = None;
        app.transcript_scroll = None;
        app.status = tr!(
            "已跳到負責人 {label}",
            label = session_label(app, &assignee)
        );
    }
}

/// `:task-status <狀態>`：更新任務視圖裡選取的任務。
pub(crate) fn set_selected_status(app: &mut App, writer: &mut HostRouter, arg: &str) -> Result<()> {
    if app.view != CenterView::Tasks {
        app.status = tr!("task-status 作用在任務視圖選取的任務：先用 :tasks 或 Tab 切過去");
        return Ok(());
    }
    let Some(status) = TaskStatus::parse(arg) else {
        app.status = tr!("用法：task-status <todo|doing|blocked|review|done>");
        return Ok(());
    };
    let Some(id) = selected_task(app) else {
        app.status = tr!("沒有選取的任務");
        return Ok(());
    };
    crate::send(
        writer,
        &Request::TaskUpdate {
            id: id.clone(),
            status: Some(status),
            assignee: None,
            detail: None,
        },
    )?;
    app.status = tr!(
        "任務 {id} → {status}",
        id = short(&id),
        status = status.id()
    );
    Ok(())
}

fn session_label(app: &App, id: &str) -> String {
    app.state
        .sessions
        .get(id)
        .map_or_else(|| short(id), |s| s.label.clone())
}

/// 「多久前」的精簡寫法。
pub(crate) fn fmt_age(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// 依顯示寬度截斷並補空白到剛好 `w` 欄 (中文等寬字元算 2 欄)。
fn fit(s: &str, w: usize) -> String {
    let s = s.replace('\n', " ");
    let mut out = String::new();
    let mut used = 0;
    if UnicodeWidthStr::width(s.as_str()) <= w {
        used = UnicodeWidthStr::width(s.as_str());
        out = s;
    } else {
        for ch in s.chars() {
            let cw = ch.width().unwrap_or(0);
            if used + cw + 1 > w {
                break;
            }
            out.push(ch);
            used += cw;
        }
        out.push('…');
        used += 1;
    }
    out.push_str(&" ".repeat(w.saturating_sub(used)));
    out
}

/// 相依性清單：未完成的用醒目顏色，已完成的淡化。
fn dep_spans(app: &App, t: &Task) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, id) in t.depends_on.iter().enumerate() {
        let done = app
            .state
            .tasks
            .get(id)
            .is_none_or(|d| d.status == TaskStatus::Done);
        let style = if done {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(
            format!("{}{}", if i == 0 { " ⧗" } else { "," }, short(id)),
            style,
        ));
    }
    spans
}

pub(crate) fn draw_tasks(f: &mut TuiFrame, area: Rect, app: &mut App) {
    let ids = visible_task_ids(app);
    app.task_cursor = app.task_cursor.min(ids.len().saturating_sub(1));
    let total = app.state.tasks.len();
    let title = if ids.len() == total {
        format!(" Tasks ({total}) ")
    } else {
        tr!(
            " Tasks ({n}/{total} · 本專案，P 顯示全部) ",
            n = ids.len(),
            total = total
        )
    };
    if ids.is_empty() {
        app.tasks_area = Rect::default();
        app.task_rows.clear();
        let para = Paragraph::new(Line::styled(
            tr!("還沒有任務。用 :task <標題> 建立，或讓 agent 呼叫 termade_task_create。"),
            Style::default().fg(Color::DarkGray),
        ))
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
        f.render_widget(para, area);
        return;
    }

    // 下方留給選取任務的完整說明；太矮時整塊都給清單。
    let detail_h = if area.height >= 16 {
        (area.height / 3).clamp(6, 12)
    } else {
        0
    };
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(detail_h)])
        .split(area);
    let list_area = parts[0];

    let inner_w = list_area.width.saturating_sub(2) as usize;
    let title_w = inner_w.saturating_sub(46).clamp(12, 60);
    let now = termade_core::now_millis();
    // 每一列對應的任務索引 (分組標題是 None)，供捲動與滑鼠點選。
    let mut rows: Vec<Option<usize>> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut last_group = None;
    for (i, id) in ids.iter().enumerate() {
        let t = &app.state.tasks[id];
        if last_group != Some(t.status) {
            last_group = Some(t.status);
            let n = ids
                .iter()
                .filter(|x| app.state.tasks[*x].status == t.status)
                .count();
            lines.push(Line::styled(
                format!("── {} ({n}) ──", t.status.id()),
                Style::default()
                    .fg(status_color(t.status))
                    .add_modifier(Modifier::BOLD),
            ));
            rows.push(None);
        }
        let selected = i == app.task_cursor;
        let base = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(if selected { "▶ " } else { "  " }, base),
            Span::styled(format!("{:<6} ", short(&t.id)), base.fg(Color::DarkGray)),
            Span::styled(
                format!("{:<8}", t.status.id()),
                base.fg(status_color(t.status)),
            ),
            Span::styled(fit(&t.title, title_w), base),
            Span::styled(
                format!(
                    " {}",
                    fit(
                        &t.assignee
                            .as_deref()
                            .map_or_else(|| "-".to_string(), |a| session_label(app, a)),
                        14
                    )
                ),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(
                format!("{:>4}", fmt_age(now, t.updated_ms)),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        spans.extend(dep_spans(app, t));
        lines.push(Line::from(spans));
        rows.push(Some(i));
    }

    // 捲動到游標可見；游標在組內第一列時連同分組標題一起顯示。
    let viewport = list_area.height.saturating_sub(2) as usize;
    let cursor_row = rows
        .iter()
        .position(|r| *r == Some(app.task_cursor))
        .unwrap_or(0);
    let mut offset = app.task_offset.min(rows.len().saturating_sub(viewport));
    if cursor_row < offset + 1 {
        offset = cursor_row.saturating_sub(1);
    } else if viewport > 0 && cursor_row >= offset + viewport {
        offset = cursor_row + 1 - viewport;
    }
    app.task_offset = offset;
    app.tasks_area = list_area;
    app.task_rows = rows.into_iter().skip(offset).take(viewport).collect();

    let para = Paragraph::new(lines.into_iter().skip(offset).collect::<Vec<_>>())
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(para, list_area);

    if detail_h > 0 {
        draw_task_detail(f, parts[1], app, &ids[app.task_cursor]);
    }
}

fn draw_task_detail(f: &mut TuiFrame, area: Rect, app: &App, id: &str) {
    let t = &app.state.tasks[id];
    let who = |s: &Option<String>| {
        s.as_deref()
            .map_or_else(|| "-".to_string(), |a| session_label(app, a))
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("[{}] ", t.status.id()),
                Style::default().fg(status_color(t.status)),
            ),
            Span::styled(
                t.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::styled(
            tr!(
                "{id} · 負責人 {assignee} · 建立者 {creator}",
                id = t.id,
                assignee = who(&t.assignee),
                creator = who(&t.created_by)
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    for dep in &t.depends_on {
        let (text, style) = match app.state.tasks.get(dep) {
            Some(d) if d.status == TaskStatus::Done => (
                format!("  ✓ {} {}", short(dep), d.title),
                Style::default().fg(Color::DarkGray),
            ),
            Some(d) => (
                format!(
                    "  ⧗ {} {} [{}] {}",
                    short(dep),
                    d.title,
                    d.status.id(),
                    who(&d.assignee)
                ),
                Style::default().fg(Color::Yellow),
            ),
            None => (
                tr!("  ? {id} (已不存在)", id = short(dep)),
                Style::default().fg(Color::DarkGray),
            ),
        };
        lines.push(Line::styled(text, style));
    }
    if t.detail.trim().is_empty() {
        lines.push(Line::styled(
            tr!("(沒有說明)"),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.extend(t.detail.lines().map(|l| Line::raw(l.to_string())));
    }
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(tr!(" 任務說明 (Enter 跳到負責人 · :task-status <狀態>) ")),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

/// 滑鼠點在任務清單上時選取該列的任務；回傳是否命中清單區域。
pub(crate) fn click_task(app: &mut App, column: u16, row: u16) -> bool {
    let area = app.tasks_area;
    if area.width == 0 || !area.contains(ratatui::layout::Position::new(column, row)) {
        return false;
    }
    let rel = row.saturating_sub(area.y + 1) as usize;
    if row > area.y {
        if let Some(Some(idx)) = app.task_rows.get(rel) {
            app.task_cursor = *idx;
        }
    }
    true
}

/// 給 `Y`／`:copy` 用的純文字。
pub(crate) fn tasks_text(app: &App) -> String {
    visible_task_ids(app)
        .iter()
        .map(|id| {
            let t = &app.state.tasks[id];
            format!(
                "[{}] {} {} @{}",
                t.status.id(),
                t.id,
                t.title,
                t.assignee
                    .as_deref()
                    .map_or_else(|| "-".to_string(), |a| session_label(app, a))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
