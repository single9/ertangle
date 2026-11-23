# T87 — TUI: keyboard navigation and expand/switch for collapsed `Projects & Sessions`

Status: ready-for-agent

Priority: P1　Scope: `ertangle-tui`　Depends on: T85

## Problem

In the left `Projects & Sessions` sidebar, when `ended` sessions are folded (`▸ 已結束 (N)`) or other projects are collapsed (`▸ <project> (...)`), keyboard navigation (`j`/`k`/`Down`/`Up`) stops at the last live session. If all sessions in the current project are collapsed or ended, the user cannot use the keyboard to move onto and expand `已結束` or switch to another project.

## Implementation

1. `crates/ertangle-tui/src/app.rs`:
   - Add `#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub(crate) enum SidebarFocus { EndedFold, OtherProject(usize) }` and `pub(crate) sidebar_focus: Option<SidebarFocus>` on `App` (default `None`).
   - Update `move_selection(&mut self, delta: isize)` (or navigation helper used by `j`/`k`/`Down`/`Up`) so moving down past the last visible session enters `EndedFold` (if `ended_count() > 0`) and then `OtherProject(0..others.len())` (when `scope_here` is true); moving up returns cleanly through those rows back to the last session. Also clamp/clear `sidebar_focus` when `show_ended` or `scope_here` changes.
   - Add `activate_sidebar_focus(&mut self)` (and `collapse_sidebar_focus(&mut self)`):
     - On `EndedFold`: toggles `self.show_ended` (and if expanded, moves focus to the first ended session or clears `sidebar_focus`).
     - On `OtherProject(idx)`: switches `self.home_project` (and `active_project`) to that project's root path, clears `sidebar_focus = None`, sets `self.selected = 0`, and updates `self.status`.
2. `crates/ertangle-tui/src/draw.rs` (`draw_sessions`):
   - Highlight the focused `EndedFold` or `OtherProject(idx)` row with `Modifier::REVERSED` and `"▸ "` prefix (matching selected sessions) when `app.sidebar_focus` points to it, and do not highlight a session row at the same time.
3. `crates/ertangle-tui/src/input.rs`:
   - In `Mode::Normal`, when `app.sidebar_focus.is_some()`, pressing `Enter`, `Space`, `Right`, or `l` activates the focused row (`activate_sidebar_focus`); pressing `Left` or `h` on `EndedFold` collapses `show_ended = false`.
4. Add unit tests in `crates/ertangle-tui/src/tests.rs` verifying `j`/`k` navigation onto `EndedFold` and `OtherProject`, `Enter`/`l` expanding `show_ended`, and `Enter` switching to another project (including when current project has 0 live sessions).

## Acceptance criteria

- Users can navigate with `j`/`k`/`Up`/`Down` onto `▸ 已結束 (N)` and `▸ <project>` rows and expand/switch via `Enter`/`l`/`Right`/`Space` without touching the mouse.
- Existing session selection (`app.selected` / `app.selected_id()`) continues to work seamlessly.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
