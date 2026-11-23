# T86 — TUI: sync layout cycling (`L` / `b` / `:layout`) with adaptive 3-column Flow

Status: ready-for-agent

Priority: P1　Scope: `ertangle-tui`　Depends on: T85

## Problem

After T85 introduced adaptive 3-column expansion (`!layout_pinned && layout == Two && w >= 130` renders `Three`), `app.layout` remains `LayoutMode::Two` while the screen shows `Three`:

1. Pressing `L` (`cycle_layout`) calls `self.layout.next()`, which computes `Two.next() == Tabs` — skipping `Two` and making layout switching feel broken.
2. Pressing `b` (`toggle_bus`) checks `self.layout == LayoutMode::Three`, which is `false` even when 3 columns are rendered, so `b` opens an overlay instead of toggling the right pane.
3. Users have no `:layout auto` command to return to adaptive mode after pinning a layout.

## Implementation

1. `crates/ertangle-tui/src/app.rs`:
   - Add `pub(crate) rendered_layout: LayoutMode` on `App` (initialized to `layout`).
   - Update `cycle_layout(&mut self)` to advance from `self.rendered_layout.next()`.
   - Update `toggle_bus(&mut self)` to check `self.rendered_layout == LayoutMode::Three`.
   - Add `set_layout_auto(&mut self)` (`self.layout_pinned = false; self.layout = LayoutMode::Two;`) and support `"auto"` in `:layout` (`crates/ertangle-tui/src/commands.rs`).
2. `crates/ertangle-tui/src/draw.rs`:
   - Set `app.rendered_layout = layout;` right after `effective_layout(desired_layout, w)`.
3. Add unit tests in `crates/ertangle-tui/src/tests.rs` verifying `L` cycles `Three (adaptive) -> Two -> Tabs -> Three`, `b` toggles `right_bus` when `rendered_layout == Three`, and `:layout auto` unpins layout.

## Acceptance criteria

- On wide terminals (`>= 130`), pressing `L` cycles cleanly through `two -> tabs -> three -> two` without skipping modes.
- Pressing `b` in adaptive 3-column mode toggles the right-hand Flow/Project pane instead of an overlay.
- `:layout auto` re-enables adaptive layout.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
