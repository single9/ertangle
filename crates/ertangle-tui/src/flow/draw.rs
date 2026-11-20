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

/// One-line summary; zero counts are omitted so the line fits narrow panes.
pub(super) fn summary_text(model: &FlowModel) -> String {
    let s = &model.summary;
    let mut sessions = vec![tr!("{n} 存活", n = s.live)];
    if s.busy > 0 {
        sessions.push(tr!("{n} 忙碌", n = s.busy));
    }
    if s.waiting > 0 {
        sessions.push(tr!("{n} 等待", n = s.waiting));
    }
    if s.ended > 0 {
        sessions.push(tr!("{n} 已結束", n = s.ended));
    }
    let mut groups = vec![sessions.join(" · ")];
    if s.messages_in_window > 0 {
        groups.push(tr!("{n} 則訊息/10 分", n = s.messages_in_window));
    }
    // Task status words stay in English: they are the literal `:task-status` values.
    let tasks: Vec<String> = [
        ("todo", s.tasks_todo),
        ("doing", s.tasks_doing),
        ("blocked", s.tasks_blocked),
        ("review", s.tasks_review),
    ]
    .iter()
    .filter(|(_, n)| *n > 0)
    .map(|(name, n)| format!("{n} {name}"))
    .collect();
    if !tasks.is_empty() {
        groups.push(tr!("任務 {list}", list = tasks.join(" · ")));
    }
    if let Some(host) = &s.host {
        groups.push(host.clone());
    }
    groups.join(" │ ")
}

pub(super) fn status_style(status: SessionStatus) -> Style {
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

/// Badge parts for one node, without the status name; `narrow` keeps only unread.
fn badge_parts(node: &FlowNode, narrow: bool) -> Vec<String> {
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
    }
    parts
}

/// Badge text for one list row; `narrow` keeps only the unread badge.
pub(super) fn badges(node: &FlowNode, narrow: bool) -> String {
    let mut parts = badge_parts(node, narrow);
    if !narrow {
        parts.push(status_name(node.status).to_string());
    }
    parts.join(" ")
}

/// Badge text for a graph box: the glyph already shows status, so the name is
/// only used when there is nothing else to show.
pub(super) fn box_badges(node: &FlowNode) -> String {
    let parts = badge_parts(node, false);
    if parts.is_empty() {
        status_name(node.status).to_string()
    } else {
        parts.join(" ")
    }
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
pub(super) fn edge_lines(
    model: &FlowModel,
    rows: usize,
    w: usize,
    narrow: bool,
) -> Vec<Line<'static>> {
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
pub(super) fn truncate(s: &str, w: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::FlowSummary;

    fn model(summary: FlowSummary) -> FlowModel {
        FlowModel {
            summary,
            ..FlowModel::default()
        }
    }

    #[test]
    fn summary_omits_zero_counts() {
        let text = summary_text(&model(FlowSummary {
            live: 3,
            ended: 1,
            tasks_doing: 1,
            tasks_blocked: 1,
            ..FlowSummary::default()
        }));
        assert_eq!(text, "3 存活 · 1 已結束 │ 任務 1 doing · 1 blocked");
    }

    #[test]
    fn summary_empty_state_shows_only_live_count() {
        assert_eq!(summary_text(&model(FlowSummary::default())), "0 存活");
    }

    #[test]
    fn summary_includes_messages_and_host() {
        let text = summary_text(&model(FlowSummary {
            live: 1,
            busy: 1,
            waiting: 1,
            messages_in_window: 8,
            host: Some("remote".into()),
            ..FlowSummary::default()
        }));
        assert_eq!(text, "1 存活 · 1 忙碌 · 1 等待 │ 8 則訊息/10 分 │ remote");
    }
}
