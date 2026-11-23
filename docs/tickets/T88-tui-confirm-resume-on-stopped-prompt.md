# T88 — TUI: confirm before resuming and sending prompt to ended/stopped session

Status: ready-for-agent

Priority: P1　Scope: `ertangle-tui`　Depends on: —

## Problem

When the user types and submits a prompt while an `Exited`, `Failed`, or `Sleeping` session is selected, `submit_prompt_or_route` immediately sends `Request::Prompt`, which fails on exited/failed sessions (or wakes/errors unexpectedly) only after the user already hit Enter and cleared their input box.

## Implementation

1. `crates/ertangle-tui/src/menu.rs`:
   - Add `PickStage::ConfirmResumePrompt { session: SessionId, label: String, status: SessionStatus, text: String, attachments: Vec<ertangle_core::Attachment> }`.
   - Render a concise confirmation modal/picker: e.g. `Session "{label}" 已停止 ({status})，是否接續並送出 prompt？` with two choices:
     1. `是，接續並送出` (`Resume / Wake & Send`)
     2. `否，保留草稿並取消` (`Cancel & Keep Draft`)
   - On confirm:
     - If `status == SessionStatus::Sleeping`: send `Request::Wake { session }` then `Request::Prompt { session, text, attachments, queue: false }`.
     - If `status == SessionStatus::Exited | SessionStatus::Failed`:
       - If `attachments.is_empty()`: send `Request::Resume { session, prompt: Some(text) }`.
       - Otherwise: send `Request::Resume { session: session.clone(), prompt: None }` then `Request::Prompt { session, text, attachments, queue: false }`.
   - On cancel (`Esc` or choosing cancel): restore `app.input = text`, `app.input_cursor = app.input.chars().count()`, and return to `Mode::Prompt` (or `Mode::Normal` with draft preserved in `app.input`) so the user never loses their prompt.
2. `crates/ertangle-tui/src/commands.rs` (`submit_prompt_or_route`):
   - Before sending `Request::Prompt`, check `app.state.sessions.get(&id).map(|s| s.status)`.
   - If `matches!(status, Some(SessionStatus::Exited | SessionStatus::Failed | SessionStatus::Sleeping))`:
     - Extract attachments first; if valid, open `PickStage::ConfirmResumePrompt` instead of sending `Request::Prompt` directly.
     - Note: if the session has `native_id.is_none()` and `status` is `Exited | Failed`, still inform the user or only offer cancel if non-resumable (check `s.native_id.is_some()`).
3. Run `make i18n-extract`, fill `locales/en.po`, and verify `make i18n-check`.
4. Add unit tests in `crates/ertangle-tui/src/tests.rs` covering submitting a prompt to an `Exited` session (opens confirmation, confirming sends `Request::Resume`, cancelling preserves `app.input`) and `Sleeping` session (confirming sends `Wake` + `Prompt`).

## Acceptance criteria

- Submitting a prompt to an `Exited`, `Failed`, or `Sleeping` session prompts for confirmation instead of failing silently/post-submit.
- Confirming resumes/wakes the session and sends the prompt; cancelling preserves the typed prompt.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
