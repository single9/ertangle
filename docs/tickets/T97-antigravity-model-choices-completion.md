# T97 — TUI & Adapter: Antigravity model choices, command candidate completion, and live cache

Status: done

Priority: P2　Scope: `ertangle-adapter`, `ertangle-tui`, `ertangle-daemon`　Depends on: T26

## Problem

When the user selects an Antigravity (`agy`) session and triggers `Space + s + m` (which-key shortcut to switch model in the current session), the TUI enters command mode with `:model `. However, the candidate completion popup above the input bar does not list any available models for Antigravity, and executing `:model` without arguments returns `(用 :model <名稱> 直接指定)`.

This happens because `crate::menu::model_choices` only hardcoded Claude models (`CLAUDE_MODELS`) and relied on `config.toml` for all other agents. Without a pre-configured `models` list in `config.toml`, `model_choices(&AgentKind::Antigravity)` only returns the default item which is filtered out, leaving the candidate list completely empty. Furthermore, `parse_models` dropped rows when column delimiters were spaces instead of tabs.

## Evidence

- `crates/ertangle-tui/src/menu.rs:125-128` — `SPC s m` prefills `:model `.
- `crates/ertangle-tui/src/commands.rs:1840-1848`, `1856` — `:model ` calls `model_choices(&kind)` for candidate autocompletion.
- `crates/ertangle-tui/src/menu.rs:620-643` — `model_choices` only matched `AgentKind::ClaudeCode`.
- `crates/ertangle-adapter/src/models.rs:320-335` — `parse_models` only split by `\t` and dropped space-separated outputs.

## Implementation

1. `crates/ertangle-adapter/src/models.rs`:
   - Add `ANTIGRAVITY_MODELS` slice with default model choices (`gemini-4-neon`, `gemini-4-carbon`, `gemini-4-argon`, `gemini-4-hydrogen`, `gemini-3.8-flash`, orchestrators, third-party Claude, and GPT-OSS).
   - Enhance `parse_models` to handle both `\t` and multi-space delimiters.
2. `crates/ertangle-tui/src/menu.rs`:
   - Include `ANTIGRAVITY_MODELS` in `model_choices(&AgentKind::Antigravity)`.
   - Add `LIVE_CACHE` so dynamically queried models for all agents are cached and surfaced in `model_choices`.
   - Trigger a single background fetch for uncached agents to keep candidate lists up-to-date.
   - Synchronize `LIVE_CACHE` when `Picker::poll_pending` completes.
3. `crates/ertangle-daemon/src/handlers/mod.rs` & `web/index.html`:
   - Fall back to `ANTIGRAVITY_MODELS` when dynamic query returns empty for Antigravity.
   - Update `MODEL_CATALOG` in `web/index.html` with default Antigravity models.
4. Unit tests:
   - Add adapter test for space-delimited `parse_models` and `ANTIGRAVITY_MODELS`.
   - Add TUI test verifying `model_choices` and `:model ` candidates offer Antigravity models.

## Acceptance criteria

- `model_choices(&AgentKind::Antigravity)` contains default Antigravity models.
- Typing `:model ` or pressing `Space + s + m` on an Antigravity session displays model candidates in the floating completion popup.
- `make pre-commit` passes cleanly.
