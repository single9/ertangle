# T85 — TUI: unified `Projects & Sessions` sidebar and adaptive 3-column Flow

Status: ready-for-agent

Priority: P2　Scope: `ertangle-tui`　Depends on: T82, T84

## Problem

1. In the default 2-column layout with `scope_here = true`, the left `Sessions` panel leaves most of its vertical space empty when only 1–2 sessions are active in the current project, and hides other projects behind an opaque `Sessions · +3` counter.
2. In 3-column mode, `right_bus` defaults to `false` (showing a redundant `Project` panel), while in 2-column mode pressing `b` opens `Flow` as an overlay that covers the right 40% of the transcript instead of reflowing it.

## Analysis / decisions

1. **Left sidebar (`Projects & Sessions`)**:
   - Keep `app.selected` indexing over the current scope's visible sessions unchanged so keyboard/mouse selection remains rock-solid.
   - Below the current project's sessions (and ended fold header), when `app.scope_here` is true and other projects have sessions, render a dim separator and compact summary rows for each other project (e.g. `▸ tiya-assets (1 live / 3)`), plus a short hint `P 展開 · Space P 切換`.
2. **Adaptive 3-column Flow**:
   - Default `right_bus` to `true` so 3-column mode shows `Flow` directly instead of the redundant `Project` panel.
   - Track `layout_pinned: bool` on `App` (set to `true` when the user explicitly cycles layout with `v` / `:layout` or loads an explicit `layout` from `config.toml`).
   - When `!app.layout_pinned` and `app.layout == LayoutMode::Two` and terminal width `>= 130` and there are `>= 2` live sessions in scope, `effective_layout` (or draw-time layout resolution) expands to `LayoutMode::Three` so `Flow` appears cleanly beside the transcript without overlapping it; single-session or narrow terminals (`< 130`) stay in 2-column mode.

## Implementation

1. `crates/ertangle-tui/src/app.rs`: default `right_bus: true`; add `layout_pinned: bool` (true when config specifies `layout` or `set_layout` is called); add helper `other_project_summaries(&self) -> Vec<(String, usize, usize)>` (project name, live count, total count).
2. `crates/ertangle-tui/src/draw.rs`:
   - In `draw()`: compute effective layout considering adaptive expansion when `!app.layout_pinned && app.layout == LayoutMode::Two && w >= 130 && app.state.live_sessions().count() >= 2`.
   - In `draw_sessions()`: render other-project summary rows below the ended fold header when `app.scope_here` is true and other projects exist; update title to `Projects & Sessions` (localized via `tr!`).
3. `crates/ertangle-tui/src/tests.rs` & `locales/`:
   - Update any tests expecting `right_bus: false` by default or exact `"Sessions"` title if changed.
   - Add unit tests for adaptive 3-column expansion (`w >= 130` with `>= 2` live sessions vs 1 live session vs `layout_pinned = true`) and other-project summary rows in the left sidebar.
   - Run `make i18n-extract`, fill `locales/en.po`, and run `make i18n-check`.

## Acceptance criteria

- Left sidebar shows other projects' names and live/total counts instead of an opaque `+N` count.
- Wide terminals (`>= 130` cols) with `>= 2` live sessions automatically show the 3-column layout with `Flow` on the right (non-overlapping), while single-session or narrow terminals stay 2-column, and explicit `:layout` / `v` overrides are respected.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
