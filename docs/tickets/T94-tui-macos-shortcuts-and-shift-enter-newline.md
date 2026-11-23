# T94 — TUI input box: macOS Option/Cmd shortcuts and native `Shift+Enter` newline

Priority: P1　Scope: `ertangle-tui`　Depends on: —

## Problem

1. `Shift+Enter` does not insert a newline in most terminals (including macOS iTerm2 / Ghostty / WezTerm / Kitty) because `crates/ertangle-tui/src/lib.rs` does not push Kitty keyboard enhancement flags (`DISAMBIGUATE_ESCAPE_CODES`), causing terminals to send plain `\r` (`KeyModifiers::NONE`) instead of `Enter` with `SHIFT`.
2. The TUI input box (`crates/ertangle-tui/src/input.rs`) lacks macOS / readline word and line editing shortcuts (`Option+Left/Right`, `Option+b/f`, `Option+Backspace/Delete`, `Option+d`, `Ctrl+w`, `Ctrl+k`, `Cmd+Left/Right/Backspace`).

## Implementation

1. `crates/ertangle-tui/src/lib.rs`:
   - When entering raw/alternate screen, check `crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)`; if supported, execute `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` and track whether it was pushed so `restore_terminal` executes `PopKeyboardEnhancementFlags`.
2. `crates/ertangle-tui/src/input.rs`:
   - Add word/line editing helpers for `(app.input, app.input_cursor)` (CJK/char-index safe):
     - `move_word_left`, `move_word_right`, `delete_word_back`, `delete_word_forward`, `delete_to_line_start`, `delete_to_line_end`.
   - Bind in `handle_input_key`:
     - `KeyCode::Left` with `ALT` or `CONTROL` (and `Alt+b`): `move_word_left`.
     - `KeyCode::Right` with `ALT` or `CONTROL` (and `Alt+f`): `move_word_right`.
     - `KeyCode::Backspace` with `ALT` or `CONTROL` (and `Ctrl+w`): `delete_word_back`.
     - `KeyCode::Delete` with `ALT` or `CONTROL` (and `Alt+d`): `delete_word_forward`.
     - `KeyCode::Left` / `Right` / `Backspace` with `SUPER` (macOS Cmd): line start / line end / delete to line start.
     - `Ctrl+k`: `delete_to_line_end`.
     - Ensure `Shift+Enter` (plus `Ctrl+Enter` and existing `Ctrl+j` fallback) inserts `\n` in `is_multiline(app.mode)` without triggering `Alt+Enter` queue or submit.
     - Guard the catch-all `KeyCode::Char(c)` arm against `CONTROL | ALT | SUPER` so `Alt+b/f/d` do not insert literal characters.
3. Add unit tests in `crates/ertangle-tui/src/tests.rs` covering `Shift+Enter` newline, `Option+Left/Right`, `Option+b/f`, `Option+Backspace`, `Ctrl+w`, `Ctrl+k`, and `Cmd+Left/Right/Backspace` (`SUPER`).

## Acceptance criteria

- `Shift+Enter` inserts a newline in multiline input modes when the terminal supports keyboard enhancement (and in unit tests).
- `Option+Left/Right`, `Option+Backspace`, `Option+b/f/d`, `Ctrl+w/k`, and `Cmd+Left/Right/Backspace` work in the TUI input box.
- `make pre-commit` and `cargo test -p ertangle-tui --lib` pass.
