# T63 — Complete question tool coverage

Priority: P1　Scope: `ertangle-adapter`, `ertangle-tui`, `ertangle-daemon`　Depends on: T62

Status: doing

## Problem

T62 landed the shared question model, protocol requests, daemon waiter, MCP fallback, adapter normalization, and a TUI picker. Several acceptance criteria remain unmet:

1. **Web UI cannot answer (fixed in `389a602`).** The web client now renders pending questions in the chat banner and review panel with option, multi-select, and custom answers, and submits `Request::AnswerQuestion`. The wire path passed a live probe; a real browser click is still outstanding. See `T63-live-probe-notes.md`.
2. **Claude native `AskUserQuestion` has no answer path.** `crates/ertangle-adapter/src/claude.rs` emits `AgentEvent::Question` for the tool call, but `ClaudeControl` does not override `resolve_question`. The question times out unless the agent uses the `ertangle_question` MCP tool.
3. **Only the first question is handled.** `parse_tool_call_question`, `parse_acp_elicitation`, and `parse_codex_user_input` normalize a single question; agents can send up to three.
4. **ACP tool-call fallback cannot be answered.** `acp.rs` emits `AgentEvent::Question` for a recognized `tool_call`, but no pending entry is stored for that id, so `resolve_question` fails.
5. **Real-agent verification is incomplete.** OpenCode MCP questions and the web wire path now have passing live probes in `T63-live-probe-notes.md`. Claude, Codex, and Antigravity still need live confirmation.

## Analysis / decisions

- Verify each answer path against a live session before changing code; T62 probe notes contain speculative claims about Antigravity that still need confirmation.
- For web, add a question modal that submits `Request::AnswerQuestion` and clears on `question_resolved`; reuse the normalized payload rather than parsing raw session events.
- For Claude, decide between a tool-result shim for `AskUserQuestion` and documenting the MCP tool as the required path. Do not silently time out a native tool.
- Normalize a list of questions with a shared response mapping per agent protocol. Keep backward compatibility with the single-question payload.
- For ACP tool calls, either store a pending answer keyed by the tool call id and reply through the agent's result channel, or degrade to a clearly labeled text-only question.
- Use a cancellable timer instead of one sleeping thread per question; the current 300-second thread in `ertangle-daemon/src/lib.rs` is acceptable only as an interim measure.

## Implementation

- `ertangle-adapter`: complete answer shims per agent kind, multi-question parsing, and ACP tool-call pending entries.
- `ertangle-tui`: support multiple questions per session and keep the quit key available while answering.
- `ertangle-daemon`: add the web answer UI and a cancellable question timeout.
- Update i18n catalogs and add tests per agent and client.

## Acceptance criteria

- A question from opencode, claude, codex, and antigravity is answerable in both TUI and web, and the selected answer reaches the originating tool call.
- Multiple questions in one request and simultaneous questions on different sessions are independent.
- Live probes for each agent are recorded in the ticket with command, observed messages, and result.
- `make pre-commit` and `cargo test --workspace` pass.
