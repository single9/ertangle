# T104 — Grid View interaction, monitoring titles, and transcript cache

Priority: P2　Scope: `ertangle-tui`　Depends on: T41

## Problem

1. In `CenterView::Grid`, cells are not registered as mouse click targets (`app.grid_areas`), so clicking a pane does not switch the selected session, and there is no `1`–`4` slot shortcut to jump between visible panes.
2. Each cell title only shows `status.glyph()`, `label`, and `active_tools.last()`. Pending approval/question state, active `doing` task or `status_note`, and context usage `%` are invisible in Grid view.
3. `draw_grid` calls `build_transcript_lines(app, id, inner_w)` for all 4 visible panes on every redraw without caching, causing unnecessary Markdown re-parsing on busy sessions.

## Evidence

- `crates/ertangle-tui/src/project.rs:297-353` — `draw_grid` rebuilds transcript lines on every frame, does not record cell `Rect`s, and formats minimal titles.
- `crates/ertangle-tui/src/mouse.rs:63-74` — `tab_areas` handles tab clicks, but no equivalent exists for Grid cells.
- `crates/ertangle-tui/src/input.rs:321` — `Enter` exits `CenterView::Grid` to `Transcript`, with no `1`–`4` pane selection.

## Analysis / decisions

- Add `grid_areas: Vec<(Rect, SessionId)>` on `App`, cleared alongside `tab_areas` on each draw and handled in `mouse.rs`.
- Add `1`–`4` slot shortcuts in Normal mode when `app.view == CenterView::Grid` to select the corresponding pane on the current page.
- Enrich cell titles with `[1]`..`[4]` slot index, pending `[?]` / `[!]` badge, `doing` task or `status_note`, and `ctx%` while keeping them compact.
- Add a bounded `grid_cache` keyed by `SessionId` + `(transcript_stamp, inner_w, raw_md, show_tools, show_thoughts)` (pruned to visible grid IDs) so idle panes are not re-parsed on every tick.
- Per-pane independent scrolling is intentionally omitted: small 2×2 cells have limited height and `Enter` already opens the full scrollable `Transcript` view.

## Implementation

1. `crates/ertangle-tui/src/app.rs` & `draw.rs`: add `grid_areas` and `GridCellCache` (`grid_cache`), make `transcript_stamp` `pub(crate)`.
2. `crates/ertangle-tui/src/project.rs`: record `grid_areas`, use `grid_cache` in `draw_grid`, and render slot index, pending badges, active task/`status_note`, and `ctx%` in cell titles.
3. `crates/ertangle-tui/src/mouse.rs` & `input.rs`: handle left-click on `grid_areas` and `1`–`4` key presses in `CenterView::Grid`.
4. `crates/ertangle-tui/src/tests.rs`: add unit tests for Grid click selection, `1`–`4` key selection, enriched titles, and `grid_cache` reuse.

## Acceptance criteria

- Clicking a Grid cell selects that session; pressing `1`–`4` in Grid view selects the corresponding pane on the current page.
- Cell titles display `[1]`–`[4]`, pending question/approval badges, active task/`status_note`, and context `%` when available.
- `draw_grid` reuses `grid_cache` when `transcript_stamp` and render flags are unchanged.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
