# T106 — Open Diff file at cursor line in `$EDITOR`

Priority: P3　Scope: `ertangle-tui`　Depends on: T35

## Problem

When reviewing a worker's diff in `CenterView::Diff`, users can add review comments (`c`) or discard hunks (`X`), but cannot jump directly into their external editor (`$VISUAL` / `$EDITOR`) at the currently highlighted file and line to make a quick manual edit.

## Evidence

- `crates/ertangle-tui/src/input.rs:264-273, 329-332` — `c` opens Review mode and `X` discards hunks, but `e` is unbound in `CenterView::Diff`.
- `crates/ertangle-tui/src/app.rs:1435-1459` — `current_diff_location` already resolves the file path under `diff_cursor`.
- `crates/ertangle-tui/src/lib.rs:221-244, 379-384` — `restore_terminal` / `setup_terminal` can suspend and resume raw mode/alternate screen cleanly around a child process.

## Analysis / decisions

- Extract `current_diff_file_and_line(&self) -> Option<(String, Option<usize>)>` in `app.rs` / `diff.rs` that finds the file path and computes the new-file line number from the nearest `@@ -old +new @@` hunk header above `diff_cursor`.
- Pressing `e` in `CenterView::Diff` resolves the file under `app.selected_cwd()` (falling back to `.`), validates that it exists, and stores `app.pending_editor = Some((path, line))`.
- In `lib.rs` `run_loop`, when `pending_editor` is `Some((path, line))`, call `restore_terminal`, run the editor (`$VISUAL` -> `$EDITOR` -> `vi`) with `+line` (or `--goto path:line` for VS Code), re-initialize the terminal with `setup_terminal`, and refresh the diff via `request_selected_diff`.

## Implementation

1. `crates/ertangle-tui/src/diff.rs` & `app.rs`: add `current_diff_file_and_line` and `pending_editor: Option<(PathBuf, Option<usize>)>`.
2. `crates/ertangle-tui/src/input.rs`: bind `e` in `CenterView::Diff` to populate `pending_editor` (or show a status error if the file does not exist).
3. `crates/ertangle-tui/src/lib.rs`: suspend TUI, spawn editor, restore TUI, and refresh diff.
4. `crates/ertangle-tui/src/draw.rs` & `locales/en.po`: update Diff footer hint (`e 編輯`) and translation.
5. `crates/ertangle-tui/src/tests.rs`: unit tests for hunk line-number calculation and `e` key setting `pending_editor`.

## Acceptance criteria

- Pressing `e` in `CenterView::Diff` resolves the file path and exact new-file line number from the hunk header.
- Terminal raw mode/alternate screen are restored before running `$EDITOR` and re-enabled afterward, with the diff refreshed.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
