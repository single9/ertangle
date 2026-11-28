# T110: TUI left sidebar `Projects` & `Sessions` split and density polish

Priority: P2 | Scope: `crates/ertangle-tui` | Depends on: T85, T87

## Problem

The left sidebar (`Projects & Sessions`) packed three distinct concerns into a single `ratatui::widgets::List`:
1. Active-project sessions (and ended fold header).
2. Other projects' summaries (`other_project_summaries`), appended below a `───` divider at the bottom of the list.
3. A static shortcut hint row (`P 展開 · Space P 切換`) rendered as a non-interactive `ListItem`.

As a result:
- When `scope_here` was enabled with only one project, the sidebar did not show which project was currently active.
- When the current project had many sessions, other projects were pushed off-screen below the fold.
- Each session row (`22..=34` columns total) displayed `N calls` on the right side, truncating the session label prematurely even though tool call counts are already shown in the transcript footer.
- Multiple `▸` symbols (`highlight_symbol`, inactive project rows, and `▸ 已結束`) caused visual clutter, expanded ended sessions blended into live sessions, and toggling `show_ended` mutated the right-pane `Flow` graph.

## Evidence

- `crates/ertangle-tui/src/draw.rs`: `side_l` width tiers (`22`, `26`, `30`, `34`) and `draw_sessions`.
- `crates/ertangle-tui/src/flow/model.rs`: `build_nodes` and `ended_folded` depended on `app.show_ended`.

## Analysis / decisions

- Split the left sidebar vertically into two dedicated blocks when `area.height >= 9`:
  - Top `Projects` block (`projects_area`): shows the active project (`●`) or `全部專案` on row 0, followed by other/all projects (`○`), with `p 聚焦 · P 全部 · Space P` in `title_bottom`.
  - Bottom `Sessions` block (`sessions_area`): focuses solely on sessions and `已結束 ({n})`, with `E 摺疊` in `title_bottom` when ended sessions exist.
- Remove `highlight_symbol("▸ ")` from both `Projects` and `Sessions` (use `REVERSED` highlight) so `▸`/`▾` only indicates the `已結束` fold state; reclaim 2 columns of width for session labels.
- Indent (`2` spaces) and dim (`DarkGray`) ended sessions under `▾ 已結束 ({n})` so they stay visually distinct from live sessions.
- Update `move_selection` to match the top-to-bottom layout (`Projects` above `Sessions`: pressing `k`/`↑` at row 0 of `Sessions` enters `Projects`), and add `p` (`toggle_projects_focus`) in `Normal` mode.
- Add mouse click handling on `projects_area` in `mouse.rs` (row 0 toggles `scope_here`, row 1+ switches project).
- Decouple `Flow` (`flow/model.rs`) from `app.show_ended` so expanding `已結束` in the sidebar does not alter the `Flow` graph.

## Implementation

- `crates/ertangle-tui/src/app.rs`: `projects_area`, `projects_offset`, `all_project_roots()`, `switch_to_project_root()`, `move_selection()` top-to-bottom order, and `toggle_projects_focus()`.
- `crates/ertangle-tui/src/draw.rs`: `draw_projects` + `draw_sessions` split, `side_l` width (`32`/`36` on wide terminals), `●`/`○` project markers, ended session indent/dimming, and `WaitingApproval`/`Failed` label styling.
- `crates/ertangle-tui/src/input.rs` & `crates/ertangle-tui/src/help.rs`: `p` shortcut to toggle focus between `Projects` and `Sessions`.
- `crates/ertangle-tui/src/mouse.rs`: left-click handling in `projects_area`.
- `crates/ertangle-tui/src/flow/model.rs`: always use `app.visible_sessions_with(false)` and `ended_folded: app.ended_count()`.
- `crates/ertangle-tui/src/tests.rs` & `locales/en.po`: updated tests, new `t110_clicking_projects_panel_switches_project_and_toggles_scope`, and English translations.

## Acceptance criteria

- `Projects` and `Sessions` render as distinct panels in the left sidebar without pushing other projects below long session lists.
- Keyboard navigation (`p`, `k`/`↑` from row 0 into `Projects`, `t87_sidebar_*`) and mouse clicks (`projects_area` and `sessions_area`) pass.
- Expanding `已結束` indents/dims ended sessions and does not alter the `Flow` graph.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
