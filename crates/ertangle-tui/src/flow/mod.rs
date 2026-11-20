//! T76: session flow dashboard (`CenterView::Flow`).
//!
//! The contract between the pure model (`model.rs`) and the renderer (`draw.rs`)
//! lives here so both halves can be built and tested independently.
//! Design: `docs/tickets/T76-tui-session-flow-dashboard.md`.

mod draw;
mod graph;
mod model;

pub(crate) use draw::draw_flow;
pub(crate) use graph::draw_flow_graph;
pub(crate) use model::build_flow_model;

use ertangle_core::{MsgKind, SessionId, SessionStatus, TaskId, TaskStatus};

/// Only messages newer than this (relative to `now_ms`) become edges.
pub(crate) const FLOW_WINDOW_MS: u64 = 10 * 60 * 1000;
/// Maximum number of edges kept in the model; the rest is counted in `edges_truncated`.
pub(crate) const FLOW_EDGE_CAP: usize = 40;
/// Edges whose `last_ts` is within this window of `now_ms` show a pulse marker.
pub(crate) const FLOW_PULSE_MS: u64 = 2000;
/// T77: how long a message packet takes to travel from sender to receiver in graph mode.
pub(crate) const FLOW_PACKET_MS: u64 = 1500;

/// Everything the Flow view renders, derived from `App` state at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FlowModel {
    pub summary: FlowSummary,
    /// Sessions in tree order (parent before children), as shown in the node board.
    pub nodes: Vec<FlowNode>,
    /// Ended sessions hidden because `show_ended` is off (0 when they are shown).
    pub ended_folded: usize,
    /// Dependency edges first (by task id), then message edges (newest first).
    pub edges: Vec<FlowEdge>,
    /// Edges dropped by `FLOW_EDGE_CAP`.
    pub edges_truncated: usize,
    /// `now_ms` the model was built with; the renderer uses it for relative time and pulse.
    pub now_ms: u64,
}

/// Counts for the one-line summary strip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FlowSummary {
    pub live: usize,
    pub busy: usize,
    pub waiting: usize,
    pub ended: usize,
    /// In-scope messages inside `FLOW_WINDOW_MS`.
    pub messages_in_window: usize,
    pub tasks_todo: usize,
    pub tasks_doing: usize,
    pub tasks_blocked: usize,
    pub tasks_review: usize,
    /// Active host name, only when connected to several hosts.
    pub host: Option<String>,
}

/// One session row on the node board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlowNode {
    pub id: SessionId,
    pub label: String,
    pub status: SessionStatus,
    /// Parent/child depth (0 = root).
    pub depth: usize,
    pub unread: usize,
    /// Queued prompts (`state.queues[id].len()`).
    pub queued: usize,
    pub questions: usize,
    pub permissions: usize,
    /// Non-done tasks assigned to this session.
    pub tasks: Vec<(TaskId, TaskStatus)>,
}

/// Where an edge points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlowEndpoint {
    Session(SessionId),
    Channel(String),
    Broadcast,
    /// Dependency task without an assignee.
    Unassigned,
}

/// Why an edge exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlowEdgeKind {
    /// Aggregated bus messages of one `MsgKind`.
    Message(MsgKind),
    /// `task` (assigned to `from`) waits on unfinished `dep` (assigned to `to`).
    BlockedBy { task: TaskId, dep: TaskId },
}

/// One row in the edge ribbon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlowEdge {
    pub from: SessionId,
    pub to: FlowEndpoint,
    pub kind: FlowEdgeKind,
    /// Number of aggregated messages (1 for dependency edges).
    pub count: usize,
    /// Newest message timestamp in ms (0 for dependency edges).
    pub last_ts: u64,
    /// First line of the newest message body (empty for dependency edges).
    pub preview: String,
}
