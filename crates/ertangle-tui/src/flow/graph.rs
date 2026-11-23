//! T77: animated node-link rendering of a [`FlowModel`].
//!
//! Layout is a deterministic tidy tree (no physics), so frames never jitter.
//! Animation is a pure function of `FlowModel::now_ms`, which keeps it testable.

use ertangle_core::{MsgKind, SessionStatus};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine, Points};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame as TuiFrame;
use unicode_width::UnicodeWidthStr;

use super::draw::{box_badges, edge_lines, status_style, summary_text, truncate};
use super::{FlowEdgeKind, FlowEndpoint, FlowModel, FlowNode, FLOW_PACKET_MS};

/// Node box height: border, label, badges, border.
const BOX_H: u16 = 4;
const MIN_BOX_W: u16 = 8;
const MAX_BOX_W: u16 = 24;
/// Rows between tree levels; links and packets travel through this gap.
const MIN_GAP_ROWS: u16 = 1;
const MAX_GAP_ROWS: u16 = 6;
/// Edge rows shown under the graph.
const RIBBON_ROWS: u16 = 4;
/// Message edges newer than this are drawn in their kind color; older ones dim.
const HOT_EDGE_MS: u64 = 60_000;
/// Packet glyph; distinct from every `SessionStatus::glyph()`.
pub(super) const PACKET: &str = "◆";
/// Green shades a busy node's border cycles through (256-color palette).
const BREATH: [u8; 8] = [22, 28, 34, 40, 46, 40, 34, 28];

/// Box placement for the graph, relative to nothing: rects are absolute frame cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GraphLayout {
    pub rects: Vec<Rect>,
    /// Parent index into `rects` (rebuilt from tree-ordered depths).
    pub parents: Vec<Option<usize>>,
}

/// Rebuilds parent links from tree-ordered nodes: the nearest earlier node with a smaller depth.
fn parents_of(nodes: &[FlowNode]) -> Vec<Option<usize>> {
    let mut stack: Vec<usize> = Vec::new();
    let mut parents = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        while stack.last().is_some_and(|&p| nodes[p].depth >= n.depth) {
            stack.pop();
        }
        parents.push(stack.last().copied());
        stack.push(i);
    }
    parents
}

/// Computes a tidy-tree layout inside `area`, or `None` when the tree does not fit.
pub(super) fn layout(nodes: &[FlowNode], area: Rect) -> Option<GraphLayout> {
    if nodes.is_empty() {
        return None;
    }
    let parents = parents_of(nodes);
    let n = nodes.len();
    let mut level = vec![0usize; n];
    for i in 0..n {
        level[i] = parents[i].map_or(0, |p| level[p] + 1);
    }
    // Children always follow their parent, so a reverse pass sums leaf counts bottom-up.
    let mut leaves = vec![0usize; n];
    for i in (0..n).rev() {
        leaves[i] = leaves[i].max(1);
        if let Some(p) = parents[i] {
            leaves[p] += leaves[i];
        }
    }
    let total_leaves: usize = (0..n)
        .filter(|&i| parents[i].is_none())
        .map(|i| leaves[i])
        .sum();
    let slot = f64::from(area.width) / total_leaves as f64;
    if slot < f64::from(MIN_BOX_W + 1) {
        return None;
    }

    let levels = level.iter().max().map_or(1, |m| m + 1) as u16;
    let gaps = levels.saturating_sub(1);
    let needed = levels * BOX_H + gaps * MIN_GAP_ROWS;
    if needed > area.height {
        return None;
    }
    let gap = ((area.height - levels * BOX_H).checked_div(gaps))
        .unwrap_or(0)
        .min(MAX_GAP_ROWS);

    // Each subtree owns `leaves` consecutive slots; the node is centered over them.
    let mut start = vec![0usize; n];
    let mut next_child = vec![0usize; n];
    let mut next_root = 0usize;
    let mut rects = Vec::with_capacity(n);
    for i in 0..n {
        let s = match parents[i] {
            Some(p) => {
                let s = next_child[p];
                next_child[p] += leaves[i];
                s
            }
            None => {
                let s = next_root;
                next_root += leaves[i];
                s
            }
        };
        start[i] = s;
        next_child[i] = s;
        let span = slot * leaves[i] as f64;
        let want = (nodes[i].label.width() + 4) as u16;
        let w = want
            .clamp(MIN_BOX_W, MAX_BOX_W)
            .min((span as u16).saturating_sub(1));
        let center = slot * s as f64 + span / 2.0;
        let x = (center - f64::from(w) / 2.0).max(0.0) as u16;
        let x = x.min(area.width.saturating_sub(w));
        let y = level[i] as u16 * (BOX_H + gap);
        rects.push(Rect::new(area.x + x, area.y + y, w, BOX_H));
    }
    Some(GraphLayout { rects, parents })
}

/// Draws the Flow view in graph mode. Returns `false` (drawing nothing) when the
/// tree does not fit, so the caller can fall back to the T76 list.
pub(crate) fn draw_flow_graph(
    f: &mut TuiFrame,
    area: Rect,
    model: &FlowModel,
    cursor: usize,
) -> bool {
    draw_flow_graph_titled(f, area, model, cursor, "Flow")
}

pub(crate) fn draw_flow_graph_titled(
    f: &mut TuiFrame,
    area: Rect,
    model: &FlowModel,
    cursor: usize,
    title: &str,
) -> bool {
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    if inner.height < 2 || inner.width == 0 {
        return false;
    }
    let body = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    let ribbon_rows = RIBBON_ROWS.min(model.edges.len() as u16);
    let (graph_area, ribbon_h, laid) = match fit(model, body, ribbon_rows) {
        Some(v) => v,
        None => return false,
    };

    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(Line::styled(
            truncate(&summary_text(model), inner.width as usize),
            Style::default().fg(Color::Cyan),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    draw_links(f, graph_area, model, &laid);
    for (i, node) in model.nodes.iter().enumerate() {
        if let Some(r) = laid.rects.get(i) {
            draw_box(f, *r, node, i == cursor, model.now_ms);
        }
    }
    if model.ended_folded > 0 {
        let hint = truncate(
            &tr::tr!("… {n} 個已結束 (E 顯示)", n = model.ended_folded),
            graph_area.width as usize,
        );
        let y = graph_area.y + graph_area.height.saturating_sub(1);
        f.render_widget(
            Paragraph::new(Line::styled(hint, Style::default().fg(Color::DarkGray))),
            Rect::new(graph_area.x, y, graph_area.width, 1),
        );
    }
    if ribbon_h > 0 {
        let r = Rect::new(
            body.x,
            body.y + body.height - ribbon_h,
            body.width,
            ribbon_h,
        );
        f.render_widget(
            Paragraph::new(edge_lines(
                model,
                ribbon_h as usize,
                r.width as usize,
                false,
            )),
            r,
        );
    }
    true
}

/// Tries the graph with a ribbon (plus a separator row), then without one.
fn fit(model: &FlowModel, body: Rect, ribbon_rows: u16) -> Option<(Rect, u16, GraphLayout)> {
    if ribbon_rows > 0 && body.height > ribbon_rows + 1 {
        let g = Rect::new(body.x, body.y, body.width, body.height - ribbon_rows - 1);
        if let Some(l) = layout(&model.nodes, g) {
            return Some((g, ribbon_rows, l));
        }
    }
    layout(&model.nodes, body).map(|l| (body, 0, l))
}

fn kind_color(kind: MsgKind) -> Color {
    match kind {
        MsgKind::Chat => Color::White,
        MsgKind::Handoff => Color::Magenta,
        MsgKind::Question => Color::Yellow,
        MsgKind::Answer => Color::Cyan,
        MsgKind::Status => Color::Blue,
        MsgKind::Artifact => Color::Green,
    }
}

/// Cell-space anchor points for a link from box `a` to box `b` (outside both boxes).
fn anchors(a: Rect, b: Rect) -> ((f64, f64), (f64, f64)) {
    let mid_x = |r: Rect| f64::from(r.x) + f64::from(r.width) / 2.0;
    let mid_y = |r: Rect| f64::from(r.y) + f64::from(r.height) / 2.0;
    if b.y >= a.bottom() {
        (
            (mid_x(a), f64::from(a.bottom())),
            (mid_x(b), f64::from(b.y) - 1.0),
        )
    } else if a.y >= b.bottom() {
        (
            (mid_x(a), f64::from(a.y) - 1.0),
            (mid_x(b), f64::from(b.bottom())),
        )
    } else if b.x >= a.right() {
        (
            (f64::from(a.right()), mid_y(a)),
            (f64::from(b.x) - 1.0, mid_y(b)),
        )
    } else {
        (
            (f64::from(a.x) - 1.0, mid_y(a)),
            (f64::from(b.right()), mid_y(b)),
        )
    }
}

/// Converts absolute cell coordinates to canvas coordinates of `area` (y grows upward).
fn to_canvas(area: Rect, (cx, cy): (f64, f64)) -> (f64, f64) {
    let x = cx - f64::from(area.x) + 0.5;
    let y = f64::from(area.height) - (cy - f64::from(area.y)) - 0.5;
    (x, y)
}

fn lerp(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// Stack-allocated polyline for orthogonal elbows, holding up to 4 points.
///
/// Avoids heap allocations in per-frame rendering of tree links, edges, and packets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Elbow {
    pub(super) pts: [(f64, f64); 4],
    pub(super) len: usize,
}

impl Elbow {
    #[inline]
    pub(super) fn as_slice(&self) -> &[(f64, f64)] {
        &self.pts[..self.len]
    }
}

impl std::ops::Deref for Elbow {
    type Target = [(f64, f64)];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl AsRef<[(f64, f64)]> for Elbow {
    #[inline]
    fn as_ref(&self) -> &[(f64, f64)] {
        self.as_slice()
    }
}

impl PartialEq<[(f64, f64)]> for Elbow {
    fn eq(&self, other: &[(f64, f64)]) -> bool {
        self.as_slice() == other
    }
}

impl PartialEq<Vec<(f64, f64)>> for Elbow {
    fn eq(&self, other: &Vec<(f64, f64)>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

pub(super) fn elbow_points(a: (f64, f64), b: (f64, f64)) -> Elbow {
    if (a.0 - b.0).abs() < 0.01 || (a.1 - b.1).abs() < 0.01 {
        Elbow {
            pts: [a, b, (0.0, 0.0), (0.0, 0.0)],
            len: 2,
        }
    } else if (b.1 - a.1).abs() >= (b.0 - a.0).abs() {
        let mid_y = (a.1 + b.1) / 2.0;
        Elbow {
            pts: [a, (a.0, mid_y), (b.0, mid_y), b],
            len: 4,
        }
    } else {
        let mid_x = (a.0 + b.0) / 2.0;
        Elbow {
            pts: [a, (mid_x, a.1), (mid_x, b.1), b],
            len: 4,
        }
    }
}

pub(super) fn point_along_polyline(pts: &[(f64, f64)], t: f64) -> (f64, f64) {
    if pts.is_empty() {
        return (0.0, 0.0);
    }
    if pts.len() == 1 {
        return pts[0];
    }
    let t = t.clamp(0.0, 1.0);
    let mut total_len = 0.0;
    for w in pts.windows(2) {
        total_len += ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
    }
    if total_len <= 0.0001 {
        return pts[0];
    }
    let target = total_len * t;
    let mut accum = 0.0;
    for w in pts.windows(2) {
        let seg_len = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
        if accum + seg_len >= target || (target - (accum + seg_len)).abs() < 1e-9 {
            let seg_t = if seg_len > 0.0001 {
                ((target - accum) / seg_len).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return lerp(w[0], w[1], seg_t);
        }
        accum += seg_len;
    }
    *pts.last().unwrap_or(&pts[0])
}

fn line(ctx: &mut Context<'_>, a: (f64, f64), b: (f64, f64), color: Color) {
    ctx.draw(&CanvasLine::new(a.0, a.1, b.0, b.1, color));
}

/// Dashed line: Braille points along the segment, two on / two off.
fn dashed(ctx: &mut Context<'_>, a: (f64, f64), b: (f64, f64), color: Color) {
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let steps = (len * 4.0).ceil().max(1.0) as usize;
    let coords: Vec<(f64, f64)> = (0..=steps)
        .filter(|i| (i / 3) % 2 == 0)
        .map(|i| lerp(a, b, i as f64 / steps as f64))
        .collect();
    ctx.draw(&Points {
        coords: &coords,
        color,
    });
}

fn draw_orthogonal(
    ctx: &mut Context<'_>,
    area: Rect,
    a: (f64, f64),
    b: (f64, f64),
    color: Color,
    is_dashed: bool,
) {
    let pts = elbow_points(a, b);
    for w in pts.windows(2) {
        let p0 = to_canvas(area, w[0]);
        let p1 = to_canvas(area, w[1]);
        if is_dashed {
            dashed(ctx, p0, p1, color);
        } else {
            line(ctx, p0, p1, color);
        }
    }
}

/// Packets in flight: `(from_anchor, to_anchor, progress 0..1, color)` in cell space.
type Packet = ((f64, f64), (f64, f64), f64, Color);

fn packets(model: &FlowModel, laid: &GraphLayout) -> Vec<Packet> {
    let index = |id: &str| model.nodes.iter().position(|n| n.id == id);
    let mut out = Vec::new();
    for e in &model.edges {
        let FlowEdgeKind::Message(kind) = e.kind else {
            continue;
        };
        let age = model.now_ms.saturating_sub(e.last_ts);
        if e.last_ts == 0 || age >= FLOW_PACKET_MS {
            continue;
        }
        let Some(from) = index(&e.from) else {
            continue;
        };
        let targets: Vec<usize> = match &e.to {
            FlowEndpoint::Session(id) => index(id).into_iter().collect(),
            FlowEndpoint::Broadcast => (0..model.nodes.len()).filter(|&i| i != from).collect(),
            FlowEndpoint::Channel(_) | FlowEndpoint::Unassigned => Vec::new(),
        };
        let t = age as f64 / FLOW_PACKET_MS as f64;
        for to in targets.into_iter().filter(|&to| to != from) {
            if let (Some(a), Some(b)) = (laid.rects.get(from), laid.rects.get(to)) {
                let (pa, pb) = anchors(*a, *b);
                out.push((pa, pb, t, kind_color(kind)));
            }
        }
    }
    out
}

fn draw_links(f: &mut TuiFrame, area: Rect, model: &FlowModel, laid: &GraphLayout) {
    let index = |id: &str| model.nodes.iter().position(|n| n.id == id);
    let in_flight = packets(model, laid);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, f64::from(area.width)])
        .y_bounds([0.0, f64::from(area.height)])
        .paint(|ctx| {
            for (child, parent) in laid.parents.iter().enumerate() {
                let (Some(p), Some(c)) = (
                    parent.and_then(|p| laid.rects.get(p)),
                    laid.rects.get(child),
                ) else {
                    continue;
                };
                let (a, b) = anchors(*p, *c);
                draw_orthogonal(ctx, area, a, b, Color::DarkGray, false);
            }
            for e in &model.edges {
                let Some(from) = index(&e.from).and_then(|i| laid.rects.get(i)) else {
                    continue;
                };
                let FlowEndpoint::Session(to_id) = &e.to else {
                    continue;
                };
                let Some(to) = index(to_id).and_then(|i| laid.rects.get(i)) else {
                    continue;
                };
                if from == to {
                    continue;
                }
                let (a, b) = anchors(*from, *to);
                match e.kind {
                    FlowEdgeKind::BlockedBy { .. } => {
                        draw_orthogonal(ctx, area, a, b, Color::Red, true);
                    }
                    FlowEdgeKind::Message(kind) => {
                        let hot = model.now_ms.saturating_sub(e.last_ts) < HOT_EDGE_MS;
                        if hot {
                            draw_orthogonal(ctx, area, a, b, kind_color(kind), false);
                        }
                    }
                }
            }
            ctx.layer();
            for (a, b, t, color) in &in_flight {
                let pts = elbow_points(*a, *b);
                let trail: Vec<(f64, f64)> = [0.06, 0.12, 0.18]
                    .iter()
                    .filter(|d| *t > **d)
                    .map(|d| to_canvas(area, point_along_polyline(&pts, t - d)))
                    .collect();
                ctx.draw(&Points {
                    coords: &trail,
                    color: *color,
                });
                let (x, y) = to_canvas(area, point_along_polyline(&pts, *t));
                ctx.print(
                    x,
                    y,
                    Span::styled(
                        PACKET,
                        Style::default().fg(*color).add_modifier(Modifier::BOLD),
                    ),
                );
            }
        });
    f.render_widget(canvas, area);
}

/// Border color for a node at `now_ms`: busy breathes, waiting blinks.
fn border_style(status: SessionStatus, now_ms: u64) -> Style {
    match status {
        SessionStatus::Busy => {
            let phase = (now_ms / 120) as usize % BREATH.len();
            Style::default().fg(Color::Indexed(BREATH[phase]))
        }
        SessionStatus::WaitingApproval if (now_ms / 500).is_multiple_of(2) => {
            Style::default().fg(Color::Yellow)
        }
        SessionStatus::WaitingApproval => Style::default().fg(Color::DarkGray),
        other => Style::default().fg(status_style(other).fg.unwrap_or(Color::Gray)),
    }
}

fn draw_box(f: &mut TuiFrame, r: Rect, node: &FlowNode, selected: bool, now_ms: u64) {
    let mut style = border_style(node.status, now_ms);
    let border = if selected {
        style = style.add_modifier(Modifier::BOLD);
        BorderType::Double
    } else {
        BorderType::Rounded
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border)
        .border_style(style);
    let inner_w = r.width.saturating_sub(2) as usize;
    let label = truncate(&format!("{} {}", node.status.glyph(), node.label), inner_w);
    let badge = truncate(&box_badges(node), inner_w);
    let label_style = if selected {
        status_style(node.status).add_modifier(Modifier::BOLD)
    } else {
        status_style(node.status)
    };
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(label, label_style),
            Line::styled(badge, Style::default().fg(Color::Yellow)),
        ])
        .block(block),
        r,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{FlowEdge, FlowSummary};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn node(id: &str, depth: usize) -> FlowNode {
        FlowNode {
            id: id.into(),
            label: id.into(),
            status: SessionStatus::Idle,
            depth,
            unread: 0,
            queued: 0,
            questions: 0,
            permissions: 0,
            tasks: Vec::new(),
        }
    }

    fn tree() -> Vec<FlowNode> {
        vec![node("coord", 0), node("worker/a", 1), node("worker/b", 1)]
    }

    fn msg(from: &str, to: FlowEndpoint, ts: u64) -> FlowEdge {
        FlowEdge {
            from: from.into(),
            to,
            kind: FlowEdgeKind::Message(MsgKind::Handoff),
            count: 1,
            last_ts: ts,
            preview: "go".into(),
        }
    }

    fn model(nodes: Vec<FlowNode>, edges: Vec<FlowEdge>, now_ms: u64) -> FlowModel {
        FlowModel {
            summary: FlowSummary::default(),
            nodes,
            ended_folded: 0,
            edges,
            edges_truncated: 0,
            now_ms,
        }
    }

    fn render(m: &FlowModel, w: u16, h: u16) -> (bool, String) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut drawn = false;
        term.draw(|f| drawn = draw_flow_graph(f, f.area(), m, 0))
            .unwrap();
        let text = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        (drawn, text)
    }

    #[test]
    fn parents_follow_tree_order_depths() {
        let mut nodes = tree();
        nodes.push(node("other-root", 0));
        nodes.push(node("orphan-deep", 2));
        assert_eq!(
            parents_of(&nodes),
            vec![None, Some(0), Some(0), None, Some(3)]
        );
    }

    #[test]
    fn layout_centers_parent_over_children_without_overlap() {
        let l = layout(&tree(), Rect::new(0, 0, 80, 20)).unwrap();
        let [p, a, b] = [l.rects[0], l.rects[1], l.rects[2]];
        assert!(a.y >= p.bottom() + MIN_GAP_ROWS && a.y == b.y);
        assert!(a.right() <= b.x, "siblings overlap: {a:?} {b:?}");
        let mid = |r: Rect| r.x + r.width / 2;
        let between = (mid(a) + mid(b)) / 2;
        assert!(mid(p).abs_diff(between) <= 1, "{p:?} not centered");
        for r in &l.rects {
            assert!(r.right() <= 80 && r.bottom() <= 20);
        }
    }

    #[test]
    fn layout_rejects_too_narrow_or_too_short() {
        let many: Vec<FlowNode> = (0..9).map(|i| node(&format!("w{i}"), 0)).collect();
        assert!(layout(&many, Rect::new(0, 0, 60, 20)).is_none());
        assert!(layout(&tree(), Rect::new(0, 0, 80, 8)).is_none());
        assert!(layout(&[], Rect::new(0, 0, 80, 20)).is_none());
    }

    #[test]
    fn graph_draws_boxes_and_cursor() {
        let (drawn, text) = render(&model(tree(), Vec::new(), 1_000), 80, 24);
        assert!(drawn);
        for want in ["coord", "worker/a", "worker/b", "╔", "╭"] {
            assert!(text.contains(want), "missing {want:?}");
        }
    }

    #[test]
    fn packet_visible_only_while_in_flight() {
        let now = 10_000;
        let edge = |ts| msg("coord", FlowEndpoint::Session("worker/a".into()), ts);
        let (_, mid) = render(
            &model(tree(), vec![edge(now - FLOW_PACKET_MS / 2)], now),
            80,
            24,
        );
        assert!(mid.contains(PACKET));
        let (_, done) = render(
            &model(tree(), vec![edge(now - FLOW_PACKET_MS)], now),
            80,
            24,
        );
        assert!(!done.contains(PACKET));
    }

    #[test]
    fn broadcast_sends_a_packet_to_every_other_node() {
        let now = 10_000;
        let m = model(
            tree(),
            vec![msg(
                "coord",
                FlowEndpoint::Broadcast,
                now - FLOW_PACKET_MS / 2,
            )],
            now,
        );
        let laid = layout(&m.nodes, Rect::new(0, 1, 80, 18)).unwrap();
        assert_eq!(packets(&m, &laid).len(), 2);
    }

    #[test]
    fn busy_border_breathes_over_time() {
        let a = border_style(SessionStatus::Busy, 0);
        let b = border_style(SessionStatus::Busy, 120 * 4);
        assert_ne!(a, b);
    }

    #[test]
    fn returns_false_when_tree_does_not_fit_and_never_panics() {
        let many: Vec<FlowNode> = (0..12).map(|i| node(&format!("w{i}"), 0)).collect();
        assert!(!render(&model(many, Vec::new(), 0), 80, 24).0);
        for (w, h) in [(1, 1), (5, 3), (20, 5), (200, 60)] {
            let _ = render(&model(tree(), Vec::new(), 0), w, h);
        }
    }

    #[test]
    fn elbow_points_and_point_along_polyline() {
        // Axis-aligned cases
        let p_h = elbow_points((10.0, 5.0), (20.0, 5.0));
        assert_eq!(p_h.as_slice(), &[(10.0, 5.0), (20.0, 5.0)]);
        assert_eq!(p_h.len, 2);
        let p_v = elbow_points((10.0, 5.0), (10.0, 25.0));
        assert_eq!(p_v.as_slice(), &[(10.0, 5.0), (10.0, 25.0)]);
        assert_eq!(p_v.len, 2);

        // Vertical dominant: dy >= dx -> mid_y
        let p_v_dom = elbow_points((10.0, 0.0), (20.0, 20.0));
        assert_eq!(
            p_v_dom.as_slice(),
            &[(10.0, 0.0), (10.0, 10.0), (20.0, 10.0), (20.0, 20.0)]
        );
        assert_eq!(p_v_dom.len, 4);

        // Horizontal dominant: dx > dy -> mid_x
        let p_h_dom = elbow_points((0.0, 10.0), (30.0, 20.0));
        assert_eq!(
            p_h_dom.as_slice(),
            &[(0.0, 10.0), (15.0, 10.0), (15.0, 20.0), (30.0, 20.0)]
        );
        assert_eq!(p_h_dom.len, 4);

        // point_along_polyline edge cases
        assert_eq!(point_along_polyline(&[], 0.5), (0.0, 0.0));
        assert_eq!(point_along_polyline(&[(5.0, 5.0)], 0.5), (5.0, 5.0));

        // Along p_v_dom: total len is 10 + 10 + 10 = 30
        let pt0 = point_along_polyline(&p_v_dom, 0.0);
        assert!((pt0.0 - 10.0).abs() < 1e-6 && (pt0.1 - 0.0).abs() < 1e-6);

        let pt_one_third = point_along_polyline(&p_v_dom, 1.0 / 3.0);
        assert!((pt_one_third.0 - 10.0).abs() < 1e-6 && (pt_one_third.1 - 10.0).abs() < 1e-6);

        let pt_half = point_along_polyline(&p_v_dom, 0.5);
        assert!((pt_half.0 - 15.0).abs() < 1e-6 && (pt_half.1 - 10.0).abs() < 1e-6);

        let pt1 = point_along_polyline(&p_v_dom, 1.0);
        assert!((pt1.0 - 20.0).abs() < 1e-6 && (pt1.1 - 20.0).abs() < 1e-6);
    }
}
