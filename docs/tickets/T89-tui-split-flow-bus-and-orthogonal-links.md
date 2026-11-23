# T89 — TUI: vertical split for Flow + Bus in right pane and orthogonal Flow graph links

Priority: P2　Scope: `ertangle-tui`　Depends on: T82

## Problem

1. Currently the right pane (`right_bus` / `bus_overlay`) shows only `Flow`, hiding the `Bus / Tasks` message & task feed. Users want to see both `Flow` (top) and `Bus / Tasks` (bottom) stacked vertically in the right pane.
2. In `crates/ertangle-tui/src/flow/graph.rs`, tree links and message edges are drawn as straight diagonal lines, which look cluttered compared to clean right-angle (orthogonal) connectors.

## Implementation

1. **Vertical split for Flow + Bus (`crates/ertangle-tui/src/draw.rs`)**:
   - Restore `draw_bus(f: &mut TuiFrame, area: Rect, app: &App)` (showing pending questions, pending permissions, visible tasks, and visible bus messages with title `" Bus / Tasks "` or `" Bus / Tasks · 本專案 "`).
   - Add a helper `draw_right_flow_and_bus(f: &mut TuiFrame, area: Rect, app: &App)`:
     - When `area.height >= 16`: split `area` vertically (`Constraint::Percentage(55)` top for `crate::flow::draw_flow_pane`, `Constraint::Percentage(45)` bottom for `draw_bus`).
     - When `area.height < 16`: render `crate::flow::draw_flow_pane(f, area, app)` only.
   - Use `draw_right_flow_and_bus` in `draw()` for both `right_bus` and `bus_overlay`.
2. **Orthogonal links & packet routing (`crates/ertangle-tui/src/flow/graph.rs`)**:
   - Add `elbow_points(a: (f64, f64), b: (f64, f64)) -> Vec<(f64, f64)>`:
     - If axis-aligned (`(a.0 - b.0).abs() < 0.01 || (a.1 - b.1).abs() < 0.01`): `[a, b]`.
     - If vertical separation `>=` horizontal: `[a, (a.0, mid_y), (b.0, mid_y), b]` where `mid_y = (a.1 + b.1) / 2.0`.
     - Otherwise: `[a, (mid_x, a.1), (mid_x, b.1), b]` where `mid_x = (a.0 + b.0) / 2.0`.
   - Draw tree links, message edges, and `BlockedBy` dashed edges segment-by-segment using `elbow_points`.
   - Add `point_along_polyline(pts: &[(f64, f64)], t: f64) -> (f64, f64)` so travelling `PACKET` (`◆`) and its Braille trail follow the orthogonal segments around corners.
3. **Tests (`crates/ertangle-tui/src/tests.rs` & `flow/graph.rs`)**:
   - Add unit tests for `elbow_points` (vertical->horizontal->vertical turns, axis-aligned passthrough) and `point_along_polyline`.
   - Update/add layout tests verifying both `"Flow"` and `"Bus / Tasks"` appear in the right pane when `right_bus = true` and `height >= 16`.
4. Mark `T89` `(已完成)` in `docs/tickets/README.md` and drop `Status:` in `T89`.
5. Verify with `make pre-commit` and `cargo test -p ertangle-tui`.

## Acceptance criteria

- Right pane in 3-column mode (and `bus_overlay`) displays `Flow` on top and `Bus / Tasks` on the bottom when `height >= 16`.
- Flow graph connectors use right-angle (90-degree) turns and animated packets follow the orthogonal path.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
