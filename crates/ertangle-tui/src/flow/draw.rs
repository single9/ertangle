//! Renders a [`FlowModel`] into the center pane.

use ertangle_core::{MsgKind, SessionStatus};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame as TuiFrame;
use tr::tr;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{FlowEdge, FlowEdgeKind, FlowEndpoint, FlowModel, FlowNode, FLOW_PULSE_MS};
use crate::app::short;

/// Below this width badges (except unread) and previews are dropped.
const NARROW_COLS: u16 = 60;
/// Below this height only the summary and node board are drawn.
const SHORT_ROWS: u16 = 10;

/// Draws the Flow view: summary strip, node board, edge ribbon.
///
/// `cursor` is the selected index into `model.nodes` (already clamped by the caller).
pub(crate) fn draw_flow(f: &mut TuiFrame, area: Rect, model: &FlowModel, cursor: usize) {
    let block = Block::default().borders(Borders::ALL).title("Flow");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let w = inner.width as usize;
    let narrow = inner.width < NARROW_COLS;

    let summary = Rect::new(inner.x, inner.y, inner.width, 1);
    f.render_widget(
        Paragraph::new(Line::styled(
            truncate(&summary_text(model), w),
            Style::default().fg(Color::Cyan),
        )),
        summary,
    );
    if inner.height == 1 {
        return;
    }

    let rest = inner.height - 1;
    let board_h = if area.height < SHORT_ROWS {
        rest
    } else {
        ((u32::from(inner.height) * 45 / 100) as u16)
            .max(3)
            .min(rest)
    };
    let board = Rect::new(inner.x, inner.y + 1, inner.width, board_h);
    f.render_widget(
        Paragraph::new(node_lines(model, cursor, board_h as usize, w, narrow)),
        board,
    );

    let ribbon_h = rest - board_h;
    if ribbon_h == 0 {
        return;
    }
    let ribbon = Rect::new(inner.x, inner.y + 1 + board_h, inner.width, ribbon_h);
    f.render_widget(
        Paragraph::new(edge_lines(model, ribbon_h as usize, w, narrow)),
        ribbon,
    );
}

fn summary_text(model: &FlowModel) -> String {
    let s = &model.summary;
    let mut out = tr!(
        "{live} 存活 · {busy} 忙碌 · {waiting} 等待 · {ended} 已結束 │ 近 10 分鐘 {msgs} 則訊息 │ 任務 todo {todo} doing {doing} blocked {blocked} review {review}",
        live = s.live,
        busy = s.busy,
        waiting = s.waiting,
        ended = s.ended,
        msgs = s.messages_in_window,
        todo = s.tasks_todo,
        doing = s.tasks_doing,
        blocked = s.tasks_blocked,
        review = s.tasks_review
    );
    if let Some(host) = &s.host {
        out.push_str(&format!(" │ {host}"));
    }
    out
}

fn status_style(status: SessionStatus) -> Style {
    match status {
        SessionStatus::Busy => Style::default().fg(Color::Green),
        SessionStatus::WaitingApproval => Style::default().fg(Color::Yellow),
        SessionStatus::Failed => Style::default().fg(Color::Red),
        SessionStatus::Sleeping | SessionStatus::Exited => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    }
}

fn status_name(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Starting => "starting",
        SessionStatus::Idle => "idle",
        SessionStatus::Busy => "busy",
        SessionStatus::WaitingApproval => "waiting",
        SessionStatus::Sleeping => "sleeping",
        SessionStatus::Exited => "exited",
        SessionStatus::Failed => "failed",
    }
}

/// Badge text for one node; `narrow` keeps only the unread badge.
fn badges(node: &FlowNode, narrow: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if node.unread > 0 {
        parts.push(format!("✉{}", node.unread));
    }
    if !narrow {
        if node.queued > 0 {
            parts.push(format!("⧗{}", node.queued));
        }
        if node.questions > 0 {
            parts.push(format!("?{}", node.questions));
        }
        if node.permissions > 0 {
            parts.push("!perm".to_string());
        }
        if !node.tasks.is_empty() {
            parts.push(format!("T:{}", node.tasks.len()));
        }
        parts.push(status_name(node.status).to_string());
    }
    parts.join(" ")
}

/// Node board rows, scrolled so the cursor stays visible.
fn node_lines(
    model: &FlowModel,
    cursor: usize,
    rows: usize,
    w: usize,
    narrow: bool,
) -> Vec<Line<'static>> {
    if rows == 0 {
        return Vec::new();
    }
    if model.nodes.is_empty() && model.ended_folded == 0 {
        return vec![Line::styled(
            truncate(&tr!("(沒有 session)"), w),
            Style::default().fg(Color::DarkGray),
        )];
    }
    let fold = model.ended_folded > 0;
    let visible = rows.saturating_sub(usize::from(fold)).max(1);
    let start = (cursor + 1).saturating_sub(visible);
    let mut lines: Vec<Line<'static>> = model
        .nodes
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, n)| node_line(n, i == cursor, w, narrow))
        .collect();
    if fold && rows > 1 {
        lines.push(Line::styled(
            truncate(&tr!("… {n} 個已結束 (E 顯示)", n = model.ended_folded), w),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines
}

fn node_line(node: &FlowNode, selected: bool, w: usize, narrow: bool) -> Line<'static> {
    let prefix = format!("{}{} ", "  ".repeat(node.depth), node.status.glyph());
    let badge = badges(node, narrow);
    let badge_w = if badge.is_empty() {
        0
    } else {
        badge.width() + 1
    };
    let label_w = w.saturating_sub(prefix.width() + badge_w);
    // Not enough room for the badges next to a readable label: drop them.
    let (label_w, badge) = if label_w < 4 && !badge.is_empty() {
        (w.saturating_sub(prefix.width()), String::new())
    } else {
        (label_w, badge)
    };
    let label = truncate(&node.label, label_w);
    let used = prefix.width() + label.width();
    let mut spans = vec![Span::raw(prefix), Span::raw(label)];
    if !badge.is_empty() {
        let gap = w.saturating_sub(used + badge.width()).max(1);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(badge, Style::default().fg(Color::Yellow)));
    }
    let base = status_style(node.status);
    let style = if selected {
        base.add_modifier(Modifier::REVERSED)
    } else {
        base
    };
    let mut line = Line::from(spans);
    if selected {
        let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
        line.spans
            .push(Span::raw(" ".repeat(w.saturating_sub(total))));
    }
    line.style(style)
}

fn session_name(model: &FlowModel, id: &str) -> String {
    model
        .nodes
        .iter()
        .find(|n| n.id == id)
        .map_or_else(|| short(id), |n| n.label.clone())
}

fn endpoint_name(model: &FlowModel, to: &FlowEndpoint) -> String {
    match to {
        FlowEndpoint::Session(id) => session_name(model, id),
        FlowEndpoint::Channel(name) => format!("#{name}"),
        FlowEndpoint::Broadcast => "*".to_string(),
        FlowEndpoint::Unassigned => tr!("(未指派)"),
    }
}

/// Compact "time ago" label (`12s`, `3m`, `2h`, `1d`).
fn age(now_ms: u64, then_ms: u64) -> String {
    crate::tasks::fmt_age(now_ms, then_ms)
}

fn kind_name(kind: MsgKind) -> &'static str {
    kind.id()
}

fn edge_line(model: &FlowModel, e: &FlowEdge, w: usize, narrow: bool) -> Line<'static> {
    let from = session_name(model, &e.from);
    let to = endpoint_name(model, &e.to);
    match &e.kind {
        FlowEdgeKind::BlockedBy { task, dep } => {
            let text = format!(
                "{from} ┄┄blocked by┄┄▶ {to}  {} ← {}",
                short(task),
                short(dep)
            );
            Line::styled(truncate(&text, w), Style::default().fg(Color::Red))
        }
        FlowEdgeKind::Message(kind) => {
            let pulse = model.now_ms.saturating_sub(e.last_ts) <= FLOW_PULSE_MS;
            let mut text = format!(
                "{}{from} ──{}×{}──▶ {to}",
                if pulse { "⚡ " } else { "" },
                kind_name(*kind),
                e.count
            );
            if !narrow && !e.preview.is_empty() {
                text.push_str(&format!(" \"{}\"", e.preview.replace('\n', " ")));
            }
            let when = age(model.now_ms, e.last_ts);
            let room = w.saturating_sub(when.width() + 1);
            let mut text = truncate(&text, room);
            let pad = w.saturating_sub(text.width() + when.width());
            text.push_str(&" ".repeat(pad));
            text.push_str(&when);
            let style = if pulse {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            Line::styled(text, style)
        }
    }
}

/// Edge ribbon rows; the last row becomes `+N more` when edges do not all fit.
fn edge_lines(model: &FlowModel, rows: usize, w: usize, narrow: bool) -> Vec<Line<'static>> {
    let total = model.edges.len();
    let dropped = model.edges_truncated;
    let fits_all = total <= rows && dropped == 0;
    let shown = if fits_all {
        total
    } else {
        rows.saturating_sub(1).min(total)
    };
    let mut lines: Vec<Line<'static>> = model
        .edges
        .iter()
        .take(shown)
        .map(|e| edge_line(model, e, w, narrow))
        .collect();
    let more = total - shown + dropped;
    if more > 0 && rows > 0 {
        lines.push(Line::styled(
            truncate(&tr!("+{n} 筆更多", n = more), w),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines
}

/// Truncates `s` to at most `w` display columns, ending with `…` when cut.
fn truncate(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w - 1 {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}
