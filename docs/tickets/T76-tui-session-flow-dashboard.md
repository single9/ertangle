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
- `crates/ertangle-tui/src/app.rs:1001` — `visible_messages()` applies the `scope_here` filter; `app.rs:954` computes unread count; `app.rs:1010` `toggle_bus`.
- `crates/ertangle-tui/src/input.rs:266` — `f` is search (do not reuse); `input.rs:321` — `b` toggles the bus pane; uppercase `F` is currently unused.
- `crates/ertangle-core/src/lib.rs:210,481,539` — `SessionStatus`, `BusMessage { from, to, body, ts }`, `Task { assignee, depends_on, status }`.
- `crates/ertangle-core/src/lib.rs:1562` — `State::unfinished_deps(task)` gives the incomplete prerequisites for blocked-task edges.
- `crates/ertangle-tui/src/lib.rs` — the event loop already redraws every 120ms while any session is `Busy`, so animation needs no new timer.
- `crates/ertangle-daemon/src/web/index.html:4027` — the web Overview has a session tree and counts but no flow representation; it is deliberately out of scope here.

## Analysis / decisions

- **TUI first, web later.** ertangle is terminal-native. All data needed for the flow view is already in `App.state` and `visible_messages()`, so no daemon or protocol change is required. The web version can follow in a separate ticket; if a shared model is extracted, put it in `ertangle-core`, not in the daemon.
- **Do not replace `draw_bus`.** Add a new `CenterView::Flow` rendered in the larger center pane. The flat bus list stays as the reliable fallback and keeps its `b` toggle and keybindings unchanged.
- **No force-directed graph.** Terminal cells are coarse and force layouts jitter, especially on mobile SSH clients. Use a deterministic layered layout:
  - Node board: one row per live in-scope session, indented by parent/child depth, with a status glyph, truncated label, and compact badges (unread, queued prompts, pending question/permission, active tasks).
  - Edge ribbon: a compact adjacency list of recent message pairs (`from ──kind×count──▶ to`, last-body preview, relative time), with blocked-task dependencies from `unfinished_deps` as a second edge kind. A small pulse marks traffic from the last ~2 seconds.
  - A literal node-link Canvas/Braille rendering may be added later; it is not required for T76.
- **Caps and folding are design assumptions, not user-validated.** Live in-scope sessions only; ended sessions folded behind a count row (respect the existing `E` / `toggle_ended` setting). Cap displayed edges (suggest 40) and keep ordering deterministic.
- **Not confirmed before implementation**: whether ratatui 0.30's `Canvas`/Braille shapes are enabled and readable at 80×24 in this workspace — check before choosing any canvas-based drawing; the box-drawing ribbon is the safe fallback. Whether uppercase `F` stays free. Whether `ertangle_core::now_millis()` is visible to the TUI crate for relative timestamps.

## Implementation

1. New module `crates/ertangle-tui/src/flow.rs`:
   - Pure `build_flow_model(app: &App) -> FlowModel` with `FlowNode { id, label, status, depth, unread, queued, pending_question, pending_permission, tasks_active }` and `FlowEdge { from, to, kind, count, last_ts, preview }`. Derive from `app.state.sessions`, `visible_messages()`, `state.queues`, `pending_questions`, `pending_permissions`, `state.tasks`, and `State::unfinished_deps`. No mutation, no I/O, so it is unit-testable without a terminal.
   - `draw_flow(f: &mut TuiFrame, area: Rect, app: &App)` renders a summary strip, the indented node board, and the edge ribbon. Colors follow existing conventions (`Busy` green, `WaitingApproval` yellow/cyan, `Blocked` red, `Sleeping` dim).
   - Minimal navigation: `j`/`k` or arrow keys move a cursor over node rows; `Enter` selects that session and returns to `CenterView::Transcript` (reuse the existing selection mechanism).
2. Wire it up:
   - Add `CenterView::Flow` in `app.rs`, dispatch in `draw.rs:102-108`.
   - `KeyCode::Char('F')` in `input.rs` normal mode toggles into/out of the Flow view; add `:flow` to the command list in `commands.rs` and its help text.
   - Keep `b`, `b` overlay behavior, and all existing center views untouched.
3. i18n: all new user-facing strings via `tr!` / `tr_noop!`. Run `make i18n-extract`, fill `locales/en.po` (no empty `msgstr` or fuzzy entries), then `make i18n-check`.
4. Tests in `crates/ertangle-tui/src/tests.rs` and `flow.rs`:
   - model: scope filtering drops messages from other projects; unread/queued/question/permission badges match `app` state; blocked task produces a dependency edge; ended sessions folded; edge cap and deterministic order.
   - render: Flow title, node labels, edge rows, no stale ended node, pulse on a recent edge using the buffer helper already used by other draw tests.
   - keys: `F` and `:flow` open/close the view; `Enter` selects a session and returns to Transcript; `b` still toggles the bus pane.
5. Do not change `crates/ertangle-daemon`, `crates/ertangle-protocol`, or `web/index.html`. Follow `AGENTS.md`: English comments/Rustdoc, no `unwrap`/`expect` in production code, English conventional commit.

## Acceptance criteria

- Pressing `F` or running `:flow` opens a Flow center view; all existing views and the `b` bus toggle behave exactly as before.
- The view lists every live in-scope session with status glyph, parent/child indentation, and correct badges; ended sessions are folded with a visible count.
- Message edges show direction, kind, count, preview, and relative time; a pulse appears for traffic in the last ~2 seconds while the session is busy (using the existing 120ms redraw).
- Blocked tasks show dependency edges derived from `State::unfinished_deps`, colored red.
- The view is readable at 80×24, never panics on empty state or malformed labels, and does not grow without bound.
- `make pre-commit` and `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false cargo test -p ertangle-tui` pass, and `make i18n-check` is clean.

## Verification

Manual check on a real session tree with at least one coordinator, two workers, one ended session, one queued prompt, and one blocked task; record results in `docs/VERIFICATION.md`, not in this ticket.
