# T63 — Complete question tool coverage

Priority: P1　Scope: `ertangle-adapter`, `ertangle-tui`, `ertangle-daemon`　Depends on: T62

Status: done

## Problem

T62 landed the shared question model, protocol requests, daemon waiter, MCP fallback, adapter normalization, and a TUI picker. Several acceptance criteria remained unmet:

1. **Web UI cannot answer (fixed in `389a602`).** The web client now renders pending questions in the chat banner and review panel with option, multi-select, and custom answers, and submits `Request::AnswerQuestion`. A headless browser click has now confirmed the full path.
2. **Claude native `AskUserQuestion` has no answer path (fixed in `0645845`).** `ertangle_approve` now returns `behavior: "allow"` with `updatedInput.answers` keyed by question text. Injected tool results (`649a4f2`) still produced "The user did not answer the questions." and were abandoned; auto-approving permissions (`ff79985`) was also superseded by routing through the permission tool.
3. **Only the first question is handled (fixed).** All three normalizers now return a list with a shared `group` id. Answering one question clears its siblings from daemon state.
4. **ACP tool-call fallback cannot be answered (fixed in `4c741ed`).** Pending entries are stored per tool-call id, and the empty `rawInput` placeholder that precedes ACP elicitation is ignored so it no longer creates a `{}` question.
5. **Real-agent verification is incomplete.** See `T63-live-probe-notes.md` for the final results. Claude, OpenCode, and Antigravity are verified live; the Codex live limitation is recorded below.

## Analysis / decisions

- Each answer path was verified against a live session before changing code; T62 probe notes contained speculative Antigravity claims that are now documented as unverified.
- The web reuses the normalized pending-question payload rather than parsing raw session events, and clears on `question_resolved` and session exit.
- Claude native questions are answered through `ertangle_approve` with `updatedInput.answers`. Injected tool results do not work with the Claude CLI's question tool.
- A question list shares one `group` id per request; the daemon clears sibling questions when any member is answered, so the web and TUI do not keep stale prompts.
- Questions are dropped from state when their session exits or is removed, preventing a stale web banner after a session disappears.
- ACP elicitation JSON Schema `oneOf` / `anyOf` variants are parsed into labeled options with descriptions. When a variant has an opaque `const` (Antigravity uses `"1"` / `"2"`), `QuestionOption.value` carries the submission value while `label` remains human-readable; `acp.rs` maps answers back to the const before replying.
- Skipped questions in a group are resolved as omitted fields, so an optional "Other" companion field cannot discard a real selection.
- A cancellable timer replaces the previous per-question sleeping thread (`cancellable_question_timeout_cleans_up_timer`).

## Implementation

- `ertangle-adapter`: per-agent answer shims, multi-question parsing with group ids, ACP tool-call pending entries, `oneOf` option parsing, and empty-placeholder suppression.
- `ertangle-tui`: multiple questions per session with the quit key available while answering.
- `ertangle-daemon`: web answer UI, grouped and session-scoped question cleanup, and a cancellable question timeout.
- i18n catalogs regenerated; unit tests added per agent and client.

## Final verification

Commits in order: `389a602`, `4c741ed` (merged as `df52e38`), `70ef490`, `ff79985`, `649a4f2`, `0645845`, `bcd6a43`, `2a6bed8`, `784a755`, `8214413` (docs), `aa23cc9`, `bd6b58b`.

| Path | Result |
| --- | --- |
| OpenCode MCP `ertangle_question` | PASS — answered `Postgres`, session returned to `idle` |
| OpenCode native `question` / ACP elicitation | PASS — answered `Vue` / `React`; real options with descriptions; grouped "other" question cleared |
| Antigravity native `ask_question` / ACP elicitation | PASS — answered `Postgres`; const value `"1"` submitted; agent reported the selection |
| Claude native `AskUserQuestion` (single) | PASS — answered `Postgres`, tool output "Your questions have been answered" |
| Claude native `AskUserQuestion` (two in one call) | PASS — answered `Postgres` and `React` independently |
| Web DOM banner render and submit | PASS — options rendered, clicked `Redis`, banner hidden (`display: none`), pending count 0, agent replied `Redis` |
| Grouped question cleanup | PASS — unit test `grouped_question_siblings_are_scoped_to_one_request` |
| Session-exit cleanup | PASS — unit test `session_exit_clears_pending_questions` |
| Cancellable timeout | PASS — unit test `cancellable_question_timeout_cleans_up_timer` |
| Codex `item/tool/requestUserInput` | PASS at protocol level — `user_input_answer_result` and multi-input parsing covered by unit tests |
| Codex native live probe | BLOCKED — the tool is absent from the default-mode tool set and its binary requires an interactive stdin terminal, while the adapter uses piped stdin. The binary exposes an `experimental_request_user_input` toggle, but it does not remove the interactive-terminal requirement. A fallback probe also hit a token-mismatch error, unrelated to the answer path. |
| Antigravity agent | PASS — `agy` is installed at `/home/duye/.local/bin/agy`; native question probe answered `Postgres` |

`make pre-commit` and `cargo test --workspace` pass.

## Acceptance criteria

- A question from opencode, claude, and antigravity is answerable in both TUI and web, and the selected answer reaches the originating tool call. Codex is covered by protocol/unit tests; its live probe is blocked by the environment.
- Multiple questions in one request and simultaneous questions on different sessions are independent.
- Live probes are recorded in `T63-live-probe-notes.md` with command, observed messages, and result.
- `make pre-commit` and `cargo test --workspace` pass.
