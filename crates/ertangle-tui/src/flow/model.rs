//! Pure derivation of [`FlowModel`] from `App` state (no I/O, no mutation).

use std::collections::{BTreeMap, HashSet};

use ertangle_core::{BusMessage, SessionId, SessionStatus, Target, TaskStatus};

use super::{
    FlowEdge, FlowEdgeKind, FlowEndpoint, FlowModel, FlowNode, FlowSummary, FLOW_EDGE_CAP,
    FLOW_WINDOW_MS,
};
use crate::app::App;

/// Builds the Flow view model for the active host at `now_ms`.
///
/// `now_ms` is a parameter so tests are deterministic; callers pass
/// `ertangle_core::now_millis()`.
///
/// Only the active host's state (`app.state`) is read, matching
/// `App::visible_messages`; sessions of other hosts are not listed as nodes.
pub(crate) fn build_flow_model(app: &App, now_ms: u64) -> FlowModel {
    let nodes = build_nodes(app);
    let messages: Vec<&BusMessage> = app
        .visible_messages()
        .into_iter()
        .filter(|m| in_window(m.ts, now_ms))
        .collect();

    let mut edges = dependency_edges(app);
    edges.extend(message_edges(&messages));
    let edges_truncated = edges.len().saturating_sub(FLOW_EDGE_CAP);
    edges.truncate(FLOW_EDGE_CAP);

    FlowModel {
        summary: build_summary(app, &nodes, messages.len()),
        nodes,
        ended_folded: app.ended_count(),
        edges,
        edges_truncated,
        now_ms,
    }
}

fn in_window(ts: u64, now_ms: u64) -> bool {
    ts.saturating_add(FLOW_WINDOW_MS) >= now_ms
}

fn build_nodes(app: &App) -> Vec<FlowNode> {
    let state = &app.state;
    // Flow always focuses on live sessions regardless of the sidebar's `show_ended` fold.
    app.visible_sessions_with(false)
        .into_iter()
        .filter(|(host, _)| host == app.active_host())
        .filter_map(|(host, id)| {
            let session = state.sessions.get(&id)?;
            let tasks = state
                .tasks
                .values()
                .filter(|t| t.status != TaskStatus::Done && t.assignee.as_deref() == Some(&id))
                .map(|t| (t.id.clone(), t.status))
                .collect();
            Some(FlowNode {
                label: session.label.clone(),
                status: session.status,
                depth: app.session_tree_depth(&host, &id),
                unread: unread_for(app, &id),
                queued: state.queues.get(&id).map_or(0, Vec::len),
                questions: state
                    .pending_questions
                    .values()
                    .filter(|q| q.session == id)
                    .count(),
                permissions: state
                    .pending_permissions
                    .values()
                    .filter(|p| p.session == id)
                    .count(),
                tasks,
                id,
            })
        })
        .collect()
}

/// In-scope direct messages to `id` that arrived after the global bus read marker.
///
/// There is no per-session read marker, so this mirrors `App::unread_bus`
/// (`bus_seen` index, zero while the bus is visible) restricted to messages
/// addressed to this session by id or label.
fn unread_for(app: &App, id: &str) -> usize {
    if app.bus_visible {
        return 0;
    }
    // `message_in_scope` is private to `App`; `visible_messages` exposes the same filter.
    let in_scope: HashSet<&str> = app
        .visible_messages()
        .into_iter()
        .map(|m| m.id.as_str())
        .collect();
    app.state
        .messages
        .iter()
        .enumerate()
        .filter(|(i, m)| *i >= app.bus_seen && in_scope.contains(m.id.as_str()))
        .filter(|(_, m)| match &m.to {
            Target::Direct(t) => app.state.resolve_session_id(t).unwrap_or_else(|| t.clone()) == id,
            _ => false,
        })
        .count()
}

fn build_summary(app: &App, nodes: &[FlowNode], messages_in_window: usize) -> FlowSummary {
    let count_status = |s: SessionStatus| nodes.iter().filter(|n| n.status == s).count();
    let count_task = |s: TaskStatus| app.state.tasks.values().filter(|t| t.status == s).count();
    FlowSummary {
        live: nodes.iter().filter(|n| n.status.is_live()).count(),
        busy: count_status(SessionStatus::Busy),
        waiting: count_status(SessionStatus::WaitingApproval),
        ended: app.ended_count(),
        messages_in_window,
        tasks_todo: count_task(TaskStatus::Todo),
        tasks_doing: count_task(TaskStatus::Doing),
        tasks_blocked: count_task(TaskStatus::Blocked),
        tasks_review: count_task(TaskStatus::Review),
        host: app
            .has_multiple_hosts()
            .then(|| app.active_host().to_string()),
    }
}

/// One edge per (non-done task with assignee, unfinished dependency), ordered by (task, dep).
fn dependency_edges(app: &App) -> Vec<FlowEdge> {
    let state = &app.state;
    let mut out = Vec::new();
    // `tasks` is a BTreeMap, so iteration is already ordered by task id.
    for task in state.tasks.values() {
        let Some(from) = task.assignee.clone() else {
            continue;
        };
        if task.status == TaskStatus::Done {
            continue;
        }
        let mut deps = state.unfinished_deps(task);
        deps.sort_by(|a, b| a.id.cmp(&b.id));
        for dep in deps {
            let to = match &dep.assignee {
                Some(s) => FlowEndpoint::Session(s.clone()),
                None => FlowEndpoint::Unassigned,
            };
            out.push(FlowEdge {
                from: from.clone(),
                to,
                kind: FlowEdgeKind::BlockedBy {
                    task: task.id.clone(),
                    dep: dep.id.clone(),
                },
                count: 1,
                last_ts: 0,
                preview: String::new(),
            });
        }
    }
    out
}

/// Aggregates messages by (from, to, kind), newest first with deterministic ties.
fn message_edges(messages: &[&BusMessage]) -> Vec<FlowEdge> {
    let mut groups: BTreeMap<(SessionId, EndpointKey, &'static str), FlowEdge> = BTreeMap::new();
    for m in messages {
        let to_key = EndpointKey::from(&m.to);
        let key = (m.from.clone(), to_key.clone(), m.kind.id());
        let edge = groups.entry(key).or_insert_with(|| FlowEdge {
            from: m.from.clone(),
            to: to_key.endpoint(),
            kind: FlowEdgeKind::Message(m.kind),
            count: 0,
            last_ts: 0,
            preview: String::new(),
        });
        edge.count += 1;
        if edge.count == 1 || m.ts >= edge.last_ts {
            edge.last_ts = m.ts;
            edge.preview = m.body.lines().next().unwrap_or("").to_string();
        }
    }
    // BTreeMap order is (from, to, kind id); a stable sort by last_ts desc keeps it as tie-break.
    let mut edges: Vec<FlowEdge> = groups.into_values().collect();
    edges.sort_by_key(|e| std::cmp::Reverse(e.last_ts));
    edges
}

/// Orderable mirror of the message-target half of [`FlowEndpoint`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EndpointKey {
    Session(SessionId),
    Channel(String),
    Broadcast,
}

impl EndpointKey {
    fn from(target: &Target) -> Self {
        match target {
            Target::Direct(s) => Self::Session(s.clone()),
            Target::Channel(c) => Self::Channel(c.clone()),
            Target::Broadcast => Self::Broadcast,
        }
    }

    fn endpoint(&self) -> FlowEndpoint {
        match self {
            Self::Session(s) => FlowEndpoint::Session(s.clone()),
            Self::Channel(c) => FlowEndpoint::Channel(c.clone()),
            Self::Broadcast => FlowEndpoint::Broadcast,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ertangle_core::{AgentKind, Cost, MsgKind, Role, Session, Task};

    fn session(id: &str, parent: Option<&str>, status: SessionStatus, cwd: &str) -> Session {
        Session {
            id: id.into(),
            kind: AgentKind::OpenCode,
            label: format!("lbl-{id}"),
            cwd: cwd.into(),
            branch: None,
            role: Role::Worker,
            status,
            model: None,
            cost: Cost::default(),
            parent: parent.map(String::from),
            created_ms: 1,
            transport: "acp".into(),
            status_note: None,
            repo_root: None,
            context: None,
            native_id: None,
            permission: Default::default(),
            budget_usd: None,
            over_budget: false,
            handed_off_to: None,
            effort: None,
            slash_commands: Vec::new(),
            active_tools: Vec::new(),
            bg_procs: Vec::new(),
            tool_calls: 0,
            ended_ms: None,
        }
    }

    fn app_with(sessions: &[Session]) -> App {
        let mut app = App::new();
        app.scope_here = false;
        for s in sessions {
            app.state.sessions.insert(s.id.clone(), s.clone());
            // `session_tree_depth` reads the per-host copy of the state.
            if let Some(host) = app.hosts.get_mut("local") {
                host.state.sessions.insert(s.id.clone(), s.clone());
            }
        }
        app.bus_visible = false;
        app
    }

    fn live(id: &str) -> Session {
        session(id, None, SessionStatus::Idle, "/tmp")
    }

    fn msg(n: u32, from: &str, to: Target, kind: MsgKind, body: &str, ts: u64) -> BusMessage {
        BusMessage {
            id: format!("m{n}"),
            from: from.into(),
            to,
            kind,
            body: body.into(),
            reply_to: None,
            ts,
        }
    }

    fn task(id: &str, status: TaskStatus, assignee: Option<&str>, deps: &[&str]) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            detail: String::new(),
            status,
            assignee: assignee.map(String::from),
            created_by: None,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            updated_ms: 0,
        }
    }

    const NOW: u64 = 10_000_000;

    #[test]
    fn empty_state_yields_empty_model() {
        let app = app_with(&[]);
        let m = build_flow_model(&app, NOW);
        assert!(m.nodes.is_empty() && m.edges.is_empty());
        assert_eq!(m.summary, FlowSummary::default());
        assert_eq!((m.ended_folded, m.edges_truncated, m.now_ms), (0, 0, NOW));
    }

    #[test]
    fn scope_filter_drops_other_project_messages() {
        let mut app = app_with(&[
            session("a", None, SessionStatus::Idle, "/proj/here"),
            session("b", None, SessionStatus::Idle, "/proj/other"),
        ]);
        app.scope_here = true;
        app.home_project = "/proj/here".into();
        app.state.messages = vec![
            msg(1, "a", Target::Broadcast, MsgKind::Chat, "mine", NOW),
            msg(2, "b", Target::Broadcast, MsgKind::Chat, "theirs", NOW),
        ];
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(m.edges[0].from, "a");
        assert_eq!(m.summary.messages_in_window, 1);
    }

    #[test]
    fn window_excludes_old_messages() {
        let mut app = app_with(&[live("a")]);
        app.state.messages = vec![
            msg(
                1,
                "a",
                Target::Broadcast,
                MsgKind::Chat,
                "old",
                NOW - FLOW_WINDOW_MS - 1,
            ),
            msg(
                2,
                "a",
                Target::Broadcast,
                MsgKind::Status,
                "edge",
                NOW - FLOW_WINDOW_MS,
            ),
            msg(3, "a", Target::Broadcast, MsgKind::Chat, "future", NOW + 5),
        ];
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.summary.messages_in_window, 2);
        assert_eq!(m.edges.len(), 2);
        // Tiny `now_ms` must not underflow.
        assert_eq!(build_flow_model(&app, 0).summary.messages_in_window, 3);
    }

    #[test]
    fn aggregates_count_last_ts_and_preview() {
        let mut app = app_with(&[live("a"), live("b")]);
        let to = || Target::Direct("b".into());
        app.state.messages = vec![
            msg(1, "a", to(), MsgKind::Chat, "first", NOW - 300),
            msg(
                2,
                "a",
                to(),
                MsgKind::Chat,
                "newest line\nsecond line",
                NOW - 100,
            ),
            msg(3, "a", to(), MsgKind::Chat, "middle", NOW - 200),
            msg(4, "a", to(), MsgKind::Question, "q?", NOW - 400),
        ];
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.edges.len(), 2);
        let chat = &m.edges[0];
        assert_eq!(chat.kind, FlowEdgeKind::Message(MsgKind::Chat));
        assert_eq!(chat.to, FlowEndpoint::Session("b".into()));
        assert_eq!((chat.count, chat.last_ts), (3, NOW - 100));
        assert_eq!(chat.preview, "newest line");
        assert_eq!(m.edges[1].kind, FlowEdgeKind::Message(MsgKind::Question));
    }

    #[test]
    fn channel_and_broadcast_endpoints() {
        let mut app = app_with(&[live("a")]);
        app.state.messages = vec![
            msg(
                1,
                "a",
                Target::Channel("dev".into()),
                MsgKind::Chat,
                "c",
                NOW - 1,
            ),
            msg(2, "a", Target::Broadcast, MsgKind::Chat, "b", NOW - 2),
        ];
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.edges[0].to, FlowEndpoint::Channel("dev".into()));
        assert_eq!(m.edges[1].to, FlowEndpoint::Broadcast);
    }

    #[test]
    fn badges_are_counted_per_session() {
        use ertangle_core::PermissionRequest;
        let mut app = app_with(&[live("a"), live("b")]);
        app.state.messages = vec![
            msg(
                1,
                "b",
                Target::Direct("a".into()),
                MsgKind::Chat,
                "seen",
                NOW,
            ),
            msg(2, "b", Target::Direct("a".into()), MsgKind::Chat, "u1", NOW),
            msg(
                3,
                "b",
                Target::Direct("lbl-a".into()),
                MsgKind::Chat,
                "u2",
                NOW,
            ),
            msg(
                4,
                "a",
                Target::Direct("b".into()),
                MsgKind::Chat,
                "to b",
                NOW,
            ),
            msg(5, "b", Target::Broadcast, MsgKind::Chat, "bc", NOW),
        ];
        app.bus_seen = 1;
        app.state
            .queues
            .insert("a".into(), vec!["p1".into(), "p2".into()]);
        app.state.pending_questions.insert(
            "q1".into(),
            serde_json::from_value(serde_json::json!({
                "id": "q1", "session": "a", "question": "?"
            }))
            .unwrap_or_else(|e| panic!("{e}")),
        );
        app.state.pending_permissions.insert(
            "r1".into(),
            PermissionRequest {
                req: "r1".into(),
                session: "a".into(),
                title: "Bash".into(),
                detail: String::new(),
            },
        );
        app.state
            .tasks
            .insert("T1".into(), task("T1", TaskStatus::Doing, Some("a"), &[]));
        app.state
            .tasks
            .insert("T2".into(), task("T2", TaskStatus::Done, Some("a"), &[]));
        app.state
            .tasks
            .insert("T3".into(), task("T3", TaskStatus::Todo, Some("b"), &[]));

        let m = build_flow_model(&app, NOW);
        let a = &m.nodes[0];
        assert_eq!(a.id, "a");
        assert_eq!(
            (a.unread, a.queued, a.questions, a.permissions),
            (2, 2, 1, 1)
        );
        assert_eq!(a.tasks, vec![("T1".to_string(), TaskStatus::Doing)]);
        let b = &m.nodes[1];
        assert_eq!(
            (b.unread, b.queued, b.questions, b.permissions),
            (1, 0, 0, 0)
        );
        assert_eq!(b.tasks, vec![("T3".to_string(), TaskStatus::Todo)]);
        assert_eq!((m.summary.tasks_doing, m.summary.tasks_todo), (1, 1));

        app.bus_visible = true;
        assert_eq!(build_flow_model(&app, NOW).nodes[0].unread, 0);
    }

    #[test]
    fn blocked_task_yields_dependency_edges() {
        let mut app = app_with(&[live("a"), live("b")]);
        app.state
            .tasks
            .insert("T1".into(), task("T1", TaskStatus::Doing, Some("b"), &[]));
        app.state
            .tasks
            .insert("T2".into(), task("T2", TaskStatus::Todo, None, &[]));
        app.state
            .tasks
            .insert("T3".into(), task("T3", TaskStatus::Done, Some("b"), &[]));
        app.state.tasks.insert(
            "T4".into(),
            task(
                "T4",
                TaskStatus::Blocked,
                Some("a"),
                &["T2", "T1", "T3", "T9"],
            ),
        );
        // Done tasks and unassigned tasks produce no outgoing edges.
        app.state.tasks.insert(
            "T5".into(),
            task("T5", TaskStatus::Done, Some("a"), &["T1"]),
        );
        app.state
            .tasks
            .insert("T6".into(), task("T6", TaskStatus::Todo, None, &["T1"]));
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.edges.len(), 2);
        assert_eq!(m.edges[0].to, FlowEndpoint::Session("b".into()));
        assert_eq!(
            m.edges[0].kind,
            FlowEdgeKind::BlockedBy {
                task: "T4".into(),
                dep: "T1".into()
            }
        );
        assert_eq!(m.edges[1].to, FlowEndpoint::Unassigned);
        assert_eq!((m.edges[0].count, m.edges[0].last_ts), (1, 0));
        assert!(m.edges[0].preview.is_empty());
        assert_eq!(m.summary.tasks_blocked, 1);
    }

    #[test]
    fn ended_sessions_folded_or_shown() {
        let mut app = app_with(&[
            live("a"),
            session("c", Some("a"), SessionStatus::Busy, "/tmp"),
            session("x", None, SessionStatus::Exited, "/tmp"),
            session("y", None, SessionStatus::Failed, "/tmp"),
        ]);
        app.show_ended = false;
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.nodes.len(), 2);
        assert_eq!((m.nodes[0].depth, m.nodes[1].depth), (0, 1));
        assert_eq!(m.nodes[1].label, "lbl-c");
        assert_eq!(m.ended_folded, 2);
        assert_eq!((m.summary.live, m.summary.busy, m.summary.ended), (2, 1, 2));
        assert_eq!(m.summary.host, None);

        app.show_ended = true;
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.nodes.len(), 2);
        assert_eq!(m.ended_folded, 2);
        assert_eq!(m.summary.ended, 2);
    }

    #[test]
    fn cap_and_deterministic_order() {
        let mut app = app_with(&[live("a")]);
        app.state
            .tasks
            .insert("T1".into(), task("T1", TaskStatus::Doing, Some("a"), &[]));
        app.state.tasks.insert(
            "T2".into(),
            task("T2", TaskStatus::Todo, Some("a"), &["T1"]),
        );
        // 50 distinct channels, each with a distinct timestamp; plus 1 dependency edge.
        app.state.messages = (0..50u32)
            .map(|i| {
                let to = Target::Channel(format!("ch{i:02}"));
                msg(i, "a", to, MsgKind::Chat, "x", NOW - 1000 + u64::from(i))
            })
            .collect();
        let m = build_flow_model(&app, NOW);
        assert_eq!(m.edges.len(), FLOW_EDGE_CAP);
        assert_eq!(m.edges_truncated, 51 - FLOW_EDGE_CAP);
        assert!(matches!(m.edges[0].kind, FlowEdgeKind::BlockedBy { .. }));
        assert_eq!(m.edges[1].to, FlowEndpoint::Channel("ch49".into()));
        assert_eq!(m.edges[2].to, FlowEndpoint::Channel("ch48".into()));
        // Same-timestamp ties break by (from, to, kind id).
        let mut app = app_with(&[live("a"), live("b")]);
        app.state.messages = vec![
            msg(1, "b", Target::Broadcast, MsgKind::Chat, "x", NOW),
            msg(2, "a", Target::Broadcast, MsgKind::Status, "x", NOW),
            msg(3, "a", Target::Broadcast, MsgKind::Chat, "x", NOW),
        ];
        let m = build_flow_model(&app, NOW);
        let order: Vec<_> = m.edges.iter().map(|e| e.from.as_str()).collect();
        assert_eq!(order, ["a", "a", "b"]);
        assert_eq!(m.edges[0].kind, FlowEdgeKind::Message(MsgKind::Chat));
        assert_eq!(m.edges[1].kind, FlowEdgeKind::Message(MsgKind::Status));
        assert_eq!(build_flow_model(&app, NOW), m);
    }
}
