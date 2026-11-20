# T67 — Auto-open the TUI question picker, docked above the input

Priority: P1　Scope: `ertangle-tui`　Depends on: T66

Status: done

## Problem

- A pending question for the selected session only showed a one-line banner at the top of the center pane; the user had to press Enter to open the picker, which feels odd.
- The picker was a centered popup in the top third of the screen, far from the input box.

## Decisions / implementation

- `App::auto_open_question` runs every loop iteration (covers new `QuestionRequested` state and selection changes). It opens the picker only when `mode == Normal` and no other picker is open, so prompts, commands and menus are never interrupted.
- `App::dismissed_questions` records question ids the user dismissed with Esc, or already answered; those never auto-open again. Manual Enter / `i` still re-opens. Ids are pruned once the daemon no longer lists the question.
- `draw_question_picker` is docked directly above the input box (`chunks[2]`), full width, growing upward; the hint line stays last.
- The top banner skips questions of the session whose picker is open; other sessions' questions and permissions keep their banner.

## Acceptance criteria

- A question arriving for the selected session in Normal mode opens the picker without a key press.
- No auto-open while typing a prompt/command or with another picker open.
- Esc dismisses without answering and does not re-open; Enter re-opens.
- The picker renders at the bottom, above the input box, and the duplicate top banner is hidden.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
