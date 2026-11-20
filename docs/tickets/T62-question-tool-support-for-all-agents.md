# T62 — Render interactive question tool calls for every agent

Priority: P1　Scope: `ertangle-protocol`, `ertangle-core`, `ertangle-daemon`, `ertangle-adapter`, `ertangle-tui`, web　Depends on: —

Status: ready-for-agent

## Investigation result

See `T62-probe-notes.md`. The concrete blockers are confirmed: ACP `initialize` does not advertise `clientCapabilities.elicitation`, and `acp.rs` rejects unhandled server-to-client JSON-RPC requests with `-32601`; Codex answers `mcpServer/elicitation/request` with empty content and rejects `item/tool/requestUserInput`. Claude needs an MCP fallback and PTY needs a typed-answer fallback.

## Problem

When an agent emits an interactive question tool call (a prompt with a question, a header, and selectable options, such as the harness `question` tool), ertangle cannot display it. The call only arrives as a plain tool-call event, so it is rendered as text in the transcript. The user gets no interactive widget, cannot choose an option, and the tool call fails with a message such as "the question couldn't be shown to the user in this client", leaving the agent without an answer.

This must work for every supported agent (opencode, claude, codex, antigravity) and for PTY sessions. A fix that only handles one agent's tool name is not acceptable.

## Evidence

- `crates/ertangle-adapter/src/lib.rs:32-44` — `AgentEvent::ToolCall` carries only `name`, `input`, `ok`, and `output`. There is no option list, question id, or answer channel.
- `crates/ertangle-adapter/src/acp.rs:217-243` — ACP `tool_call` sends `rawInput` as an opaque JSON string; `tool_call_update` only reports success or failure.
- `crates/ertangle-adapter/src/codex.rs:294-338` — Codex `mcpToolCall` sends `arguments` as a JSON string and has no answer path back into the call.
- `crates/ertangle-adapter/src/claude.rs:171,217` and `crates/ertangle-adapter/src/pty.rs:70` — Claude emits plain tool calls; PTY has no structured protocol and `resolve_permission` is a no-op.
- `crates/ertangle-daemon/src/handlers/session.rs:1514-1547` — `ask_user` already implements a blocking request keyed by id with a five-minute timeout, but it always returns a `Decision` (allow/always/deny), not an arbitrary selected option.
- `crates/ertangle-tui/src/draw.rs:586-628` — `draw_pending_banner` only renders permission prompts with `[a]llow [A]lways [d]eny`; there is no option picker for questions.
- `crates/ertangle-daemon/src/mcp.rs:247-268,797` — `ertangle_ask` is agent-to-agent messaging and does not reach the human user.
- `crates/ertangle-protocol/src/lib.rs:251` — `Request::Permission` is the only interactive client request; no question/answer pair exists.

## Analysis / decisions

- Normalize at the adapter layer. Recognize the tool names each agent uses for interactive questions, including MCP-prefixed forms such as `mcp__<server>__question` and `server:tool`, and parse the JSON input into one shared `UserQuestion { id, session, question, header, options, multi_select }` model. Use a shared name registry so adding an agent does not require changing the UI.
- Add an `AgentEvent::Question(UserQuestion)` (or an optional question payload on `ToolCall`) and a daemon `Event::QuestionRequested` with a blocking waiter keyed by question id, following the `ask_user` pattern. Add `Request::AnswerQuestion` plus a reply frame so any client (TUI, web, remote) can answer. A timeout (proposed 300 seconds) must produce a clear result instead of hanging the session.
- TUI: reuse the existing `Picker` (`crates/ertangle-tui/src/menu.rs:318`) to render the options with keyboard selection and a free-text fallback, and extend `draw_pending_banner` to show pending questions in every layout mode. Multi-host sessions must route the answer to the correct host.
- Answer delivery is the agent-specific part, kept in thin adapter shims: ACP agents use their tool-result/response mechanism if available; Claude and Codex use the question tool's MCP result path when their protocol supports it; PTY and any agent without a native answer channel fall back to injecting the answer as a normal prompt, with a status message explaining the fallback. Verify each mechanism against the real agent protocol before coding; do not assume all agents expose the same response API.
- Compatibility rule: a question from any agent must never kill or hang the session. If options cannot be rendered or answered natively, show the question as text and accept a typed answer. Agents that never emit question tools are unaffected.
- Not confirmed: whether the harness tool call reaches the adapter at all or is intercepted by the client before ertangle sees it. This must be probed with one real session per agent as the first implementation step; if the call is invisible, the ticket needs a different approach (for example a generic MCP tool registered for all agents).
- Web parity: the web UI needs the same picker to satisfy "all clients"; include it or split it into a follow-up once the protocol is settled.

## Implementation

- `ertangle-protocol`: add the question/answer request and reply types with `#[serde(default)]` where needed for old clients.
- `ertangle-core`: add the normalized `UserQuestion` and answer types.
- `ertangle-adapter`: add the shared name/payload parser and one answer shim per agent kind, with a text fallback.
- `ertangle-daemon`: emit `QuestionRequested`, keep the waiter map, and handle the answer request including timeout and cancellation.
- `ertangle-tui` and web: render the picker and banner, route answers to the right session and host.
- Update i18n catalogs, CLI help, and README.

## Acceptance criteria

- For opencode, claude, codex, and antigravity, a question tool call produces an interactive option picker in the TUI and the selected answer is delivered back to that tool call.
- PTY sessions and agents without a native answer channel display the question as text and accept a typed answer without terminating the session.
- Multiple simultaneous questions on different sessions and hosts are independent; each answer reaches its own caller.
- A question that is not answered within the timeout yields a clear result and no stuck session.
- Unknown question tool names or malformed input degrade to a text question instead of an error.
- Tests with fake adapters cover every agent kind, the timeout, the text fallback, and the protocol round trip; `make pre-commit`, `make i18n-check`, and `make test` pass.
