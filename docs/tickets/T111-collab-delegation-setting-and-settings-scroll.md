# T111: Configurable coordinator delegation (`[collab] delegation`) and tabbed Settings panel

Priority: P2 | Scope: `crates/ertangle-daemon`, `crates/ertangle-tui` | Depends on: T11, T47

## Problem

1. Coordinator sessions received a single hardcoded rule (`Spawn the fewest workers; do small tasks inline.`), which caused expensive coordinator models to perform multi-file edits and iterative build/test loops inline instead of delegating to cheaper default workers via `ertangle_spawn`.
2. The TUI Settings panel (`Space ,`) grew to 26 items with section headers, clipping bottom settings on shorter terminals, squeezing narrow/English layouts when labels were long, and shifting column widths while `loading_models()` was active.

## Evidence

- `crates/ertangle-daemon/src/collab.rs`: hardcoded coordinator token-economy rule.
- `crates/ertangle-tui/src/draw.rs`: `draw_settings` rendered all items without tabs, wrapping/truncation caps, or a description area.

## Analysis / decisions

- Add `DelegationMode` (`proactive`, `balanced`, `manual`; default `balanced`) under `[collab] delegation` in `config.rs` and wire it into `collab_instructions_with` in `collab.rs`.
- Reorganize `Space ,` into 4 tabs (`1: Agent`, `2: Spawn & Git`, `3: Collab & Context`, `4: Web & System`) with `Tab` / `BackTab` / `1`–`4` switching, soft `[title]` active tab styling, capped label column width, and a pinned 3-line wrapped description area at the bottom.
- Keep `Item::Model`'s label fixed and show `(loading…)` in the value column so background model fetching never shifts table alignment.

## Implementation

- `crates/ertangle-daemon/src/config.rs`: `DelegationMode` enum, `CollabCfg::delegation`, `merge_config`, and `Config::delegation_mode()`.
- `crates/ertangle-daemon/src/collab.rs`: `collab_instructions_with` and per-mode coordinator delegation guidance + tests.
- `crates/ertangle-tui/src/settings.rs`: `Item::CollabDelegation`, `Item::tab()`, `Item::description()`, tab navigation (`Tab` / `BackTab` / `1`–`4`), and fixed `Item::Model` label.
- `crates/ertangle-tui/src/draw.rs`: tabbed `draw_settings` with `title_bottom`, scrollable list area, and pinned description area.
- `locales/en.po` & `crates/ertangle-tui/src/tests.rs`: updated translations and settings tests.

## Acceptance criteria

- `[collab] delegation = "proactive" | "balanced" | "manual"` changes coordinator instructions accordingly (defaulting to `balanced`).
- `Space ,` switches cleanly across the 4 tabs, displays concise values with bottom descriptions, and renders cleanly in both Traditional Chinese and English.
- `make pre-commit`, `cargo test -p ertangle-daemon`, and `cargo test -p ertangle-tui` pass.
