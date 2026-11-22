# T82 — Replace right-pane Bus display with live Agent/Session Flow graph

Status: ready-for-agent

Priority: P2　Scope: `ertangle-tui`　Depends on: T77

## Problem

In `LayoutMode::Three` (when `right_bus` is toggled on with `b`) and in `bus_overlay` (`b` in `Two`/`Tabs` mode), the right pane currently renders `draw_bus` — a plain text list of pending items, tasks, and raw `from→to` messages. Meanwhile, T76/T77 implemented `CenterView::Flow` (`crates/ertangle-tui/src/flow/`), which visualizes the session tree, live node statuses, and animated message packets, but only in the center pane.

The user wants the Flow graph to replace the right-side Bus display so they can directly watch how the project's Agents and Sessions operate alongside their main workspace.

## Evidence

- `crates/ertangle-tui/src/draw.rs:32-127` (`right_bus` / `bus_overlay` call `draw_bus`).
- `crates/ertangle-tui/src/draw.rs:2215-2325` (`draw_bus`).
- `crates/ertangle-tui/src/flow/graph.rs` (`MIN_BOX_W = 10`, `draw_flow_graph`).
- `crates/ertangle-tui/src/lib.rs:352-363` (`flow_motion` 60ms redraw only checks `app.view == CenterView::Flow`).

## Analysis / decisions

1. Replace `draw_bus` calls in `crates/ertangle-tui/src/draw.rs` (both `right_bus` in `LayoutMode::Three` and `bus_overlay` in `Two`/`Tabs`) with a right-pane Flow renderer (`crate::flow::draw_flow_pane` or `draw_flow_graph` with automatic fallback to `draw_flow`).
2. Keep pending approvals/questions visible: in `draw.rs:91`, show `draw_pending_banner` whenever `has_pending` is true (even when `right_bus` is true, since the Flow graph replaces the old text Bus list).
3. Adapt `flow/graph.rs` for narrower right-side panes (30–46 cols):
   - Lower `MIN_BOX_W` from `10` to `8` (so up to 4 leaf nodes can fit in a ~36-42 col right pane), while keeping `MAX_BOX_W = 24`.
   - When `draw_flow_graph` cannot fit (e.g., too many leaves or too short), fall back cleanly to `draw_flow` (node board + edge ribbon) inside the pane.
   - Give `side_r` in `draw.rs:45-53` slightly more width on wide terminals (e.g., `w >= 140 -> 46`, `w >= 110 -> 40`, `w >= 84 -> 32`) so the right-side graph is readable.
   - When rendered in the right pane (`right_bus` / `bus_overlay`), use `app.selected_id()` (or `flow_cursor` if `CenterView::Flow`) to highlight the active session node, and title the panel `" Flow "` (or `" Flow · 本專案 "` when `app.scope_here` is active).
4. Update `crates/ertangle-tui/src/lib.rs:352` so `flow_motion` (60ms animation redraw for travelling packets) also activates when `app.bus_visible && !app.flow_list`.
5. Keep `CenterView::Flow` (`F` / `:flow`) working as before, and update `app.rs` status messages / help text / tests that referenced `"Bus / Tasks"` for the right pane.

## Implementation

1. `crates/ertangle-tui/src/flow/graph.rs` & `flow/mod.rs`: adjust `MIN_BOX_W` to `8`; expose a helper `draw_flow_pane(f, area, app)` that builds `FlowModel`, resolves the highlighted node index from `app.selected_id()`, and renders `draw_flow_graph` (falling back to `draw_flow`).
2. `crates/ertangle-tui/src/draw.rs`: replace `draw_bus` in `right_bus` and `bus_overlay` with `crate::flow::draw_flow_pane(f, ..., app)`; always show `draw_pending_banner` when `has_pending`; widen `side_r` slightly on wide terminals; remove unused `draw_bus` if no longer called.
3. `crates/ertangle-tui/src/lib.rs`: enable `flow_motion` when `app.bus_visible && !app.flow_list`.
4. `crates/ertangle-tui/src/app.rs` & `tests.rs`: update toggle status text and existing `"Bus / Tasks"` layout tests; add tests verifying the right-side pane and `bus_overlay` render `"Flow"` with project sessions and animate packets.

## Acceptance criteria

- In `LayoutMode::Three` with `right_bus` enabled (`b`), and in `bus_overlay` (`b` in `Two`/`Tabs`), the right pane renders the Flow graph (or compact Flow board fallback) showing the project's Agents/Sessions and their live status/messages instead of the old `"Bus / Tasks"` text dump.
- Pending permissions/questions remain visible via `draw_pending_banner`.
- Packets animate smoothly in the right pane when messages arrive.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
