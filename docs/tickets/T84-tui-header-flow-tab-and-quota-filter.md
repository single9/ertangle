# T84 — TUI header: add `Flow (F)` tab and filter `3p-*` quota windows

Priority: P2　Scope: `ertangle-tui`　Depends on: T79

## Problem

1. `draw_header` lists `Overview`, `Diff (g)`, `Worktrees`, and `Tasks`, but omits `Flow (F)`, making the Flow view hard to discover.
2. `quota_spans` in `crates/ertangle-tui/src/draw.rs` does not filter out Antigravity's third-party `3p-*` windows (`Quota::is_inactive_window`), so the header still shows `agy:3p-7d 剩10%` in red even after T79 excluded `3p-*` from agent-level quota pressure.

## Evidence

- `crates/ertangle-tui/src/draw.rs:1120-1158` (`quota_spans`).
- `crates/ertangle-tui/src/draw.rs:1178-1196` (`draw_header` tabs).

## Implementation

1. In `quota_spans` (`crates/ertangle-tui/src/draw.rs`), skip `w` when `q.agent == "antigravity" && w.name.starts_with("3p-")`.
2. In `draw_header` (`crates/ertangle-tui/src/draw.rs`), add `tab("Flow (F)", app.view == CenterView::Flow)` to `tab_spans`.
3. Add unit tests in `crates/ertangle-tui/src/tests.rs` verifying `quota_spans` ignores `3p-*` windows and `draw_header` displays `Flow (F)`.

## Acceptance criteria

- Header tab bar shows `Flow (F)` and highlights it when `CenterView::Flow` is active.
- Header quota strip does not show `agy:3p-*` windows.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
