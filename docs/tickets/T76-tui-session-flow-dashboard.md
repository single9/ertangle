# T76 — TUI session flow dashboard

Status: ready-for-agent

Priority: P3　Scope: `ertangle-tui` (plus i18n catalogs)　Depends on: —

## Problem

The TUI already surfaces coordination data, but never as one connected picture:

- `draw_bus` renders pending questions/permissions, tasks, and then a flat chronological `from → to` message list. It answers "what happened" but not "who talks to whom" or "where work is stuck".
- Session status, parent/child relationships, unread messages, queued prompts, and blocked tasks live in different panes (`draw_sessions`, `draw_bus`, `draw_tasks`). Understanding the current collaboration state requires scanning all of them.
- The user asked for a dashboard-like view of all sessions' interaction states and message flow. This is a "cool" enhancement, not a correctness fix; it must not disturb the existing bus list or any daemon behavior.

## Evidence

- `crates/ertangle-tui/src/draw.rs:2183` — `draw_bus` builds the Bus / Tasks pane: questions, permissions, tasks (`draw.rs:2232-2253`), then `from → to` message rows (`draw.rs:2255-2273`).
- `crates/ertangle-tui/src/draw.rs:102-120` — center-view dispatch (`Transcript | Diff | Worktrees | Tasks | Grid`) plus the bus area/overlay rendering.
- `crates/ertangle-tui/src/app.rs:63` — `CenterView` enum; `app.rs:26` — `Mode` enum.
- `crates/ertangle-tui/src/app.rs:496-560` — `visible_sessions_with(show_ended)` / `tree_order_sessions` already return sessions in parent→child tree order with scope and host filtering; `app.rs:616` `session_tree_depth(host, id)` gives the indent depth; `app.rs:533` `ended_count()`.
- `crates/ertangle-tui/src/app.rs:971` `message_in_scope`, `app.rs:1001` `visible_messages()` (reads `self.state`, i.e. the active host only); `app.rs:474` `select_or_pend(session)` is the selection entry point.
- `crates/ertangle-tui/src/input.rs:107-120` — `Tab` cycles `Transcript → Diff → Worktrees → Tasks → Transcript`; `Grid` exits to Transcript. `input.rs:252-253` — `Enter` handlers per view.
- `crates/ertangle-tui/src/input.rs` — `f` is search, `b` bus toggle, `E` toggle ended, `G` worktree GC, `T` thoughts, `H` host filter. **Uppercase `F` is unused** (verified).
- `crates/ertangle-core/src/lib.rs:210` — `SessionStatus { Starting, Idle, Busy, WaitingApproval, Sleeping, Exited, Failed }` with `glyph()`; there is no session-level `Blocked` (that is `TaskStatus::Blocked`, `lib.rs:507`).
- `crates/ertangle-core/src/lib.rs:416,443,481` — `Target { Direct, Channel, Broadcast }`, `MsgKind { Chat, Handoff, Question, Answer, Status, Artifact }`, `BusMessage { from, to, kind, body, reply_to, ts }`. Edge kind comes from `MsgKind`; no body parsing needed.
- `crates/ertangle-core/src/lib.rs:539,1562` — `Task { assignee, depends_on, status }`, `State::unfinished_deps(task)`.
- `crates/ertangle-core/src/lib.rs:73` — `pub fn now_millis()` is public and usable from the TUI.
- ratatui 0.30.2 / ratatui-widgets 0.3.2 default feature `all-widgets` includes `Canvas`. Available, but not used in T76 (see decisions).
- `crates/ertangle-tui/src/lib.rs` — the event loop redraws every 120ms while any session is `Busy`.
- `crates/ertangle-daemon/src/web/index.html:4027` — web Overview has a tree and counts but no flow; out of scope.

## Design

### Placement and navigation

- New `CenterView::Flow`, rendered in the center pane. `draw_bus`, the `b` toggle, and the bus overlay are unchanged.
- `F` in normal mode toggles Flow ⇄ previous view (store `flow_return: CenterView`, default Transcript). `:flow` does the same.
- Flow is **not** added to the `Tab` cycle (keeps muscle memory). `Tab` from Flow goes to Transcript, same as Grid.
- In Flow: `j`/`k`/`↑`/`↓` move a node cursor (`App.flow_cursor: usize`, clamped on each draw); `Enter` calls `select_or_pend(id)` and switches to Transcript; `Esc` returns to `flow_return`; `E` keeps toggling ended sessions (shared `show_ended`).
- Single-host scope for v1: model reads `app.state` (active host), same as `visible_messages()`. Show the active host name in the summary strip when `has_multiple_hosts()`.

### Layout (80×24 target)

Top-to-bottom, inside the center block titled `Flow`:

```
 5 live · 2 busy · 1 waiting · 3 ended (E)   msgs 10m: 14   tasks: 2 doing · 1 blocked
──────────────────────────────────────────────────────────────────────────────────
▸ ● coordinator/70fa          ✉2  ⧗1  ?1          T:1
    ● worker/a1b2 (codex)            !perm        T:1 doing
    ◌ worker/c3d4 (claude)                        T:1 blocked ← T75
  … 3 ended (E to show)
──────────────────────────────────────────────────────────────────────────────────
 coordinator ──handoff×2──▶ worker/a1b2   "implement flow model…"     12s  ⚡
 worker/a1b2 ──status×5───▶ coordinator  "TASK_DONE T75 8060648"      1m
 worker/c3d4 ┄┄blocked by┄┄▶ worker/a1b2  T76b ← T76a                       (red)
 coordinator ──chat×1─────▶ #review       "QUERY: …"                  4m
```

- **Summary strip** (1 row): live/busy/waiting/ended counts, message count in window, task counts by status, host name if multi-host.
- **Node board** (≈45% of remaining height, min 3 rows): rows from `visible_sessions_with(show_ended)`, indent `2 × session_tree_depth`, `SessionStatus::glyph()` + status color, truncated label (width-aware, `unicode-width`), badges: `✉n` unread for this session, `⧗n` queued prompts (`state.queues[id].len()`), `?n` pending questions, `!perm` pending permissions, `T:n <status>` active tasks assigned (non-Done). When ended are hidden, a dim fold row `… N ended (E to show)` from `ended_count()`.
- **Edge ribbon** (rest): aggregated edges, one row each, newest first. Truncation by available width; drop preview first, then counts.
- Colors: Busy green, WaitingApproval yellow, Starting/Idle default, Sleeping dim, Exited/Failed dim/red. Dependency edges red, dashed `┄`. Pulse `⚡` (or `*` when ASCII-safe fallback is needed) on edges with `last_ts` within 2000 ms of `now_millis()`.
- Narrow fallback: below 60 columns, hide badges except unread and drop the edge preview; below 10 rows, show only the summary strip and node board.

### Model (pure, in `flow.rs`)

```rust
pub(crate) struct FlowModel {
    pub summary: FlowSummary,
    pub nodes: Vec<FlowNode>,
    pub ended_folded: usize,
    pub edges: Vec<FlowEdge>,
}
pub(crate) struct FlowNode {
    pub id: SessionId, pub label: String, pub status: SessionStatus, pub depth: usize,
    pub unread: usize, pub queued: usize, pub questions: usize, pub permissions: usize,
    pub tasks: Vec<(TaskId, TaskStatus)>,
}
pub(crate) enum FlowEndpoint { Session(SessionId), Channel(String), Broadcast }
pub(crate) enum FlowEdgeKind { Message(MsgKind), BlockedBy { task: TaskId, dep: TaskId } }
pub(crate) struct FlowEdge {
    pub from: SessionId, pub to: FlowEndpoint, pub kind: FlowEdgeKind,
    pub count: usize, pub last_ts: u64, pub preview: String,
}
pub(crate) fn build_flow_model(app: &App, now_ms: u64) -> FlowModel;
```

- `now_ms` is a parameter (not read inside) so tests are deterministic; the caller passes `ertangle_core::now_millis()`.
- Message edges: from `visible_messages()` with `ts >= now_ms - FLOW_WINDOW_MS` (10 min), grouped by `(from, to, kind)`; `count`, max `ts`, and the newest body's first line as preview. `Target::Channel` → `#name`, `Target::Broadcast` → `*`.
- Dependency edges: for each non-Done task with an assignee, for each task in `state.unfinished_deps(task)` that has an assignee, edge `task.assignee ──blocked by──▶ dep.assignee`. Deps without assignee are shown as `(unassigned)`. Dependency edges sort before message edges.
- Ordering: dependency edges by task id; message edges by `last_ts` desc, then `from`, `to`, kind id (deterministic). Cap total at `FLOW_EDGE_CAP = 40`; the summary shows `+N more` when truncated.
- Unknown session ids in messages (e.g. pruned sessions) render as the raw short id; never panic, never index unchecked.

### Out of scope

- Node-link Canvas/Braille graph (possible follow-up; deterministic text layout first).
- Web flow view, cross-host aggregation, daemon/protocol changes.
- Message interrupt / steering (user explicitly deferred).

## Implementation slices

Each slice is independently mergeable; do them in order.

1. **T76a — model.** Add `crates/ertangle-tui/src/flow.rs` with the types and `build_flow_model`. Unit tests in `flow.rs`: scope filter drops other-project messages; window excludes old messages; aggregation count/last_ts/preview; channel and broadcast endpoints; unread/queued/question/permission/task badges; blocked task yields a dependency edge (including unassigned dep); ended sessions folded vs shown; cap at 40 with deterministic order; empty state.
2. **T76b — render and keys.** `CenterView::Flow`, `App.flow_cursor`, `App.flow_return`; dispatch in `draw.rs:102-108`; `draw_flow(f, area, app)` in `flow.rs`; `F`, `:flow` (in `commands.rs` + help text in `help.rs`), `j`/`k`/arrows, `Enter`, `Esc`, `Tab` from Flow. Render tests in `tests.rs` with the existing buffer helper: title, summary, indented labels, fold row, edge rows, red dependency edge, pulse on a recent edge only, 80×24 and 50×12 do not panic. Key tests: `F` and `:flow` open/close and restore previous view; `Enter` selects and returns to Transcript; `Tab` cycle unchanged when not in Flow; `b` still toggles the bus.
3. **i18n.** All user-facing strings via `tr!` / `tr_noop!`; `make i18n-extract`, fill `locales/en.po` (no empty `msgstr` or fuzzy), `make i18n-check`.

Do not change `crates/ertangle-daemon`, `crates/ertangle-protocol`, or `web/index.html`. Follow `AGENTS.md`: English comments/Rustdoc, no `unwrap`/`expect` in production code, English conventional commit.

## Acceptance criteria

- `F` or `:flow` opens the Flow center view and returns to the previous view; `Tab` cycle, all existing views, and the `b` bus toggle behave exactly as before.
- The node board lists every live in-scope session in tree order with status glyph/color, indentation, and correct badges; ended sessions are folded with a count and follow `E`.
- Message edges show direction, `MsgKind`, count, preview, and relative time within a 10-minute window; channel/broadcast targets render as `#name` / `*`; a pulse appears only for traffic in the last 2 seconds.
- Blocked tasks show red dependency edges derived from `State::unfinished_deps`.
- Readable at 80×24, degrades at narrow sizes, never panics on empty state, unknown ids, or wide/CJK labels, and shows at most 40 edges.
- `make pre-commit` and `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false cargo test -p ertangle-tui` pass, and `make i18n-check` is clean.

## Verification

Manual check on a real session tree with at least one coordinator, two workers, one ended session, one queued prompt, and one blocked task; record results in `docs/VERIFICATION.md`, not in this ticket.
