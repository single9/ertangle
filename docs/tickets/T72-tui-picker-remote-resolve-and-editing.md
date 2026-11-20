# T72 — TUI question picker: close on remote answer, editable custom answer

Priority: P1　Scope: `ertangle-tui`　Depends on: T66, T67

Status: done

## Problem

- A question answered elsewhere (web, another client) left the TUI picker open; submitting it would send answers for questions the daemon had already resolved.
- The custom-answer row only supported append and Backspace; the cursor was always at the end.

## Decisions / implementation

- `menu::reconcile_question_picker` runs each loop iteration before `auto_open_question`. It drops questions missing from `pending_questions` (and any collected answers for them, which are never sent), advances to the next remaining question, and closes the picker with a status message when none remain. Resolved ids therefore never reopen.
- `PickStage::Question` gains `custom_cursor` (char index). `menu::edit_line` is a small single-line editor: Left/Right, Home/End, Ctrl+A/E, Backspace, Delete, Ctrl+W / Alt+Backspace (delete word), Ctrl+U (to start), Ctrl+K (to end), Ctrl/Alt+Left/Right (word moves).
- Bracketed paste goes through `input::handle_paste`, which inserts into the custom row when active (newlines become spaces).
- `draw_question_picker` wraps the answer with `inputbox::rows`, scrolls so the cursor row stays visible, and calls `set_cursor_position` at the edit point so the IME preedit anchors there; wide (CJK) chars use display width.

## Acceptance criteria

- Resolving the open question (or siblings) remotely closes or advances the picker without sending stale answers.
- Editing in the middle of CJK text, Home/End, Ctrl+W, and paste behave as above (unit tests).
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
