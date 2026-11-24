# T96 — TUI: offer "create new" or "resume" when a prompt is submitted with no session

Status: done

Priority: P2　Scope: `ertangle-tui`　Depends on: T88

## Problem

When no session exists (or none is selected) and the user types a prompt and presses Enter, `submit_prompt_or_route` only sets the status line to "沒有選取的 session". The draft is cleared and the user gets no path forward. The empty-state hints ("尚無 session：按 n 建立") are passive and only help if the user notices them before typing.

T88 already solved the analogous case for ended sessions with a confirm picker; the no-session case should behave the same way.

## Evidence

- `crates/ertangle-tui/src/commands.rs:112-114` — `else` branch of `if let Some(id) = app.selected_id()` only sets `app.status`.
- `crates/ertangle-tui/src/draw.rs:1022`, `draw.rs:2006`, `crates/ertangle-tui/src/project.rs:297` — passive empty-state text.
- `crates/ertangle-tui/src/menu.rs:375` — `PickStage::ConfirmResumePrompt` (T88) is the pattern to follow.
- `crates/ertangle-tui/src/tests.rs:6192` — T88 test to mirror.

## Analysis / decisions

- Decided: keep the draft; never drop the user's text on this path.
- Decided: show the "Resume previous session" choice only when resumable sessions (ended/archived with `native_id`) exist.
- Not confirmed: whether the new session should inherit the last-used agent/host/cwd or open the full spawn wizard. Check what `n` does today and reuse it.
- Not confirmed: how to deliver the prompt after spawn. Likely wait for `SessionStatus::Idle` before sending, otherwise the prompt may be lost while `Starting`. Verify against `submit_spawn` (`commands.rs:1303`) and `Request::Spawn` (is there an initial-prompt field?). Prefer that if it exists.
- Applies to plain prompts only. `/command`, `!cmd`, `@peer` and `#channel` routing is unchanged.

## Implementation

1. `menu.rs`: add `PickStage::NoSessionPrompt { text, attachments, can_resume }` with choices: create new session / resume previous (only if `can_resume`) / cancel and keep draft.
2. `commands.rs` (`submit_prompt_or_route`): in the no-selection branch, extract attachments, then open the picker instead of setting the status line.
3. Create: run the same flow as `n`, then deliver the stored prompt (initial prompt if supported, else on first `Idle`).
4. Resume: open the existing resume selector; after a session is chosen, reuse the T88 resume-and-send path.
5. Cancel / `Esc`: restore `app.input` and cursor, return to `Mode::Prompt`.
6. Update the empty-state texts to mention that typing a prompt also works.
7. `make i18n-extract`, fill `locales/en.po`, `make i18n-check`, `make pre-commit`.

## Acceptance criteria

- With zero sessions, Enter on a plain prompt opens the picker; no `Request::Prompt` is sent and the draft is not lost.
- Create sends the prompt to the new session exactly once, after it can accept input.
- Resume option is hidden when nothing is resumable.
- Cancel and `Esc` restore the draft.
- Slash, `!`, `@` and `#` routing behave as before.
- Unit tests cover create, resume, cancel, and no-resumable cases; `cargo test -p ertangle-tui` passes.
