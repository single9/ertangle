# T62 Investigation Findings & Protocol Probe Notes

This document records the architectural and protocol investigation into interactive user question tools across all supported agents (`opencode`, `antigravity`, `claude`, `codex`, and `pty`), addressing the feasibility question raised in `docs/tickets/T62-question-tool-support-for-all-agents.md`.

---

## 1. Findings (per agent)

### 1.1 OpenCode (`opencode acp`)
- **Native tool**: `question` (internal plugin ID: `opencode.tool.question`, alias `Dl = "question"`).
- **Input schema**: `{ "questions": [ { "question": string, "header": string, "options": [ { "label": string, "description": string } ], "multiple": boolean, "custom": boolean } ] }`.
- **Protocol surface in ACP**:
  - OpenCode executes the tool by checking permission and then calling its internal `ask` service: `e.ask({ sessionID, title: "Questions", metadata: { kind: "question", tool: ... }, fields: [...] })`.
  - In ACP standard protocol, interactive client input requests are routed as JSON-RPC requests via `elicitation/create` (mode: `form`).
  - **The Root Cause in ertangle**:
    1. During ACP initialization (`crates/ertangle-adapter/src/acp.rs:777-780`), ertangle sends:
       ```json
       {
         "protocolVersion": 1,
         "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } }
       }
       ```
       It does not declare `clientCapabilities.elicitation` (`{ "form": true }`). Under ACP specification, an agent must not issue elicitation requests if the client did not advertise support.
    2. In the ACP reader loop (`crates/ertangle-adapter/src/acp.rs:434-438`), any incoming server-to-client JSON-RPC request other than `"session/request_permission"` is immediately rejected with:
       ```json
       { "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "not supported" } }
       ```
       This causes OpenCode's `question` tool execution to cancel immediately with `QuestionTool.CancelledError`, leaving the agent with an empty answer or failure.

### 1.2 Antigravity (`agy acp`)
- **Native tool**: `ask_question` (registered in harness/CLI as `ask_question`).
- **Input schema**: `{ "questions": [ { "question": string, "options": [ string ], "is_multi_select": boolean } ] }`.
- **Protocol surface in ACP**:
  - Like OpenCode, Antigravity uses ACP stdio (`agy acp`).
  - When invoked in ACP, interactive questions arrive either as a tool call event (`AgentEvent::ToolCall`) in `session/update` or as an ACP elicitation request.
  - Because ACP `clientCapabilities` lacks elicitation support and `acp.rs` drops unhandled RPCs, questions fall through without interactive UI display.

### 1.3 Claude Code (`claude stream-json`)
- **Native tool**: `AskUserQuestion`.
- **Input schema**: `{ "questions": [ { "question": string, "header": string, "options": [ { "label": string, "description": string } ], "multi_select": boolean } ] }`.
- **Protocol surface in `stream-json`**:
  - Claude CLI runs in non-interactive batch mode (`-p --input-format stream-json --output-format stream-json`).
  - In stream-json mode without an attached TTY, Claude emits a `tool_use` content block with name `"AskUserQuestion"`, but cannot render an internal terminal widget.
  - Claude does not have a server-to-client JSON-RPC connection in `stream-json`.
  - While tool permissions are intercepted using `--permission-prompt-tool mcp__ertangle__ertangle_approve` (`crates/ertangle-adapter/src/claude.rs:452`), Claude Code executes non-permission tools internally.
  - **Answer path**:
    - For native `AskUserQuestion`, Claude requires tool completion via standard tool results.
    - Alternatively, ertangle already injects the `ertangle` MCP server into Claude (`--allowedTools=mcp__ertangle__*`). Registering an MCP tool `ertangle_ask_question` provides an infallible structured path where Claude delegates the question directly to ertangle's MCP server, which suspends and waits for user choice before returning the tool result to Claude.

### 1.4 Codex (`codex app-server`)
- **Native tool**: `request_user_input` (protocol method: `item/tool/requestUserInput` and `mcpServer/elicitation/request`).
- **Input schema**: `{ "threadId": string, "turnId": string, "questions": [ { "id": string, "header": string, "question": string, "options": [ { "label": string, "description": string } ] } ] }`.
- **Protocol surface in `app-server`**:
  - Codex `app-server` initiates a JSON-RPC request to the client (`item/tool/requestUserInput`).
  - **The Root Cause in ertangle**:
    In `crates/ertangle-adapter/src/codex.rs:136-165`, ertangle recognizes `mcpServer/elicitation/request`, but hardcodes it to `ApprovalKind::McpElicitation`, returning `{ "action": "accept", "content": {} }` (empty content). Unrecognized methods (like `item/tool/requestUserInput`) hit line 159 and are rejected with `-32601 ertangle 不支援 ...`.
  - Codex expects the client to respond to `item/tool/requestUserInput` with `{ "answers": { "<question_id>": ["<selected_option>"] } }`.

### 1.5 PTY Sessions
- **Protocol surface**: Raw terminal emulation (VT100/ANSI) with no structured JSON-RPC or tool calls (`crates/ertangle-adapter/src/pty.rs`).
- Any question tool executed by a process in PTY prints text to the pseudo-terminal.
- **Answer path**: Injected directly into PTY stdin via `AgentControl::send_input`.

---

## 2. Answer Channels Summary

| Agent | Request Trigger | Incoming Protocol Message | Response Method / Channel | Payload Format |
|---|---|---|---|---|
| **OpenCode** | `question` tool | ACP `elicitation/create` (mode: form) | JSON-RPC reply to `elicitation/create` | `{ "content": { "q0": ["chosen"] } }` |
| **Antigravity** | `ask_question` | ACP `elicitation/create` or MCP tool | JSON-RPC reply or MCP tool result | `{ "answers": [...] }` |
| **Codex** | `request_user_input` | `item/tool/requestUserInput` or `mcpServer/elicitation/request` | JSON-RPC reply to server request | `{ "answers": { "q_id": ["selected"] } }` |
| **Claude Code** | `AskUserQuestion` / MCP | `tool_use` event or `ertangle_ask_question` MCP call | MCP tool result or user response block | Text or JSON answer object |
| **PTY** | CLI prompts | Unstructured terminal stdout | PTY stdin write (`send_input`) | Raw newline-terminated string |

---

## 3. Feasibility Assessment

**Feasibility is HIGH**, provided that:
1. **ACP Initialization is updated**: `crates/ertangle-adapter/src/acp.rs` must advertise `clientCapabilities.elicitation = { "form": true }` during `initialize`. This tells OpenCode and Antigravity that the client supports interactive question forms.
2. **ACP Reader handles `elicitation/create`**: Add an `on_elicitation` handler alongside `on_permission` in `acp.rs`.
3. **Codex Reader handles `item/tool/requestUserInput` and `mcpServer/elicitation/request`**: Store the pending request ID and send back structured selections instead of empty approval dictionaries.
4. **Universal MCP Fallback**: For Claude Code and other MCP-enabled agents, provide an `ertangle_ask_question` tool on the existing injected `ertangle` MCP server (`crates/ertangle-daemon/src/mcp.rs`). When called, it suspends and queries the daemon just like `ertangle_approve`.
5. **PTY Fallback**: Degrade gracefully to terminal text input without crashing or hanging.

---

## 4. Recommended Design & Implementation Plan

### Phase 1: Shared Models (`ertangle-core` & `ertangle-protocol`)
- **`crates/ertangle-core/src/lib.rs`**:
  Define normalized question structs:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct QuestionOption {
      pub label: String,
      pub description: Option<String>,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct UserQuestion {
      pub id: String,
      pub session: String,
      pub header: Option<String>,
      pub question: String,
      pub options: Vec<QuestionOption>,
      pub multi_select: bool,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct QuestionAnswer {
      pub id: String,
      pub session: String,
      pub selected: Vec<String>,
      pub custom_text: Option<String>,
  }
  ```
- **`crates/ertangle-protocol/src/lib.rs`**:
  - Add client request:
    ```rust
    Request::AnswerQuestion {
        session: SessionId,
        id: String,
        answers: Vec<String>,
    }
    ```
  - Add daemon events:
    ```rust
    Event::QuestionRequested { question: UserQuestion }
    Event::QuestionResolved { session: SessionId, id: String, answers: Vec<String> }
    ```

### Phase 2: Adapter Layer Normalization (`ertangle-adapter`)
- **`crates/ertangle-adapter/src/lib.rs`**:
  - Add `AgentEvent::Question(UserQuestion)` to `AgentEvent`.
  - Add `resolve_question(&self, id: &str, answers: &[String]) -> Result<()>` to `AgentControl` trait (with default returning `Err` or injecting into `send_input`).
- **`crates/ertangle-adapter/src/acp.rs`**:
  - In `initialize`: advertise `"elicitation": { "form": true }`.
  - In `reader_loop`: dispatch `"elicitation/create"` to `conn.on_elicitation(&msg)`.
  - In `on_elicitation`: parse fields into `UserQuestion`, save `(jsonrpc_id, fields)` in `conn.pending_questions`, emit `AgentEvent::Question`.
  - In `AcpControl::resolve_question`: reply with `{ "action": "accept", "content": { ... } }`.
- **`crates/ertangle-adapter/src/codex.rs`**:
  - In `on_server_request`: handle `"item/tool/requestUserInput"` and structured `"mcpServer/elicitation/request"`.
  - Store request ID in `conn.pending_questions` and emit `AgentEvent::Question`.
  - In `CodexControl::resolve_question`: reply to `item/tool/requestUserInput` with `{ "answers": ... }`.
- **`crates/ertangle-adapter/src/claude.rs`**:
  - Detect `AskUserQuestion` tool calls from assistant message content.
  - When using the injected MCP server, `ertangle_ask_question` handles blocking directly.
- **`crates/ertangle-adapter/src/pty.rs`**:
  - Implement `resolve_question`: format selection as string and send via `self.send_input(&format!("{answer}\n"))`.

### Phase 3: Daemon Waiter & MCP Tool (`ertangle-daemon`)
- **`crates/ertangle-daemon/src/handlers/session.rs`**:
  - Add `question_waiters: HashMap<String, Sender<Vec<String>>>` in `Shared` (mirroring `approval_waiters`).
  - Add `ask_user_question(shared, session, question) -> Vec<String>` with a 300-second timeout.
  - Handle `Request::AnswerQuestion`: find waiter, wake with answers, emit `Event::QuestionResolved`.
- **`crates/ertangle-daemon/src/mcp.rs`**:
  - Register MCP tool `ertangle_ask_question` for universal agent support.

### Phase 4: UI Rendering (`ertangle-tui` & web)
- **`crates/ertangle-tui/src/draw.rs` & `crates/ertangle-tui/src/menu.rs`**:
  - Extend `draw_pending_banner` to display active questions when `state.pending_question` is present.
  - Activate `Picker` allowing arrow key / number selection, multi-select toggles with Space, and Enter to submit.
  - Provide a fallback text box for custom answers.
- **`crates/ertangle-daemon/src/web/`**:
  - Render a modal/banner dialog for pending questions in the web client.

---

## 5. Open Questions
1. **Timeout behavior**: If the 300-second timeout expires, should ACP cancel the elicitation (`{ "action": "cancel" }`), or return an empty answer set?
   - *Recommendation*: Return cancelled (`{ "action": "cancel" }` for ACP/Codex) so the agent runtime can gracefully degrade or proceed with its own fallback instead of hanging.
2. **Free-form custom text fallback**: Does the agent want custom typed text or only predefined enum values?
   - OpenCode's `question` tool explicitly includes `custom: true` and appends `"Type your own answer"`. ACP form mode supports string inputs.
   - *Recommendation*: The TUI `Picker` should always have an `"Other (type answer)"` option that switches to text input.

---

## 6. Evidence & References

- `crates/ertangle-adapter/src/acp.rs:434-438` — Unhandled ACP requests return `-32601 not supported`.
- `crates/ertangle-adapter/src/acp.rs:777-780` — ACP `initialize` does not advertise `elicitation` capabilities.
- `crates/ertangle-adapter/src/codex.rs:136-165` — Codex `mcpServer/elicitation/request` returns empty content; unhandled `item/tool/requestUserInput` returns `-32601`.
- `crates/ertangle-adapter/src/claude.rs:450-457` — Claude per-item approvals routed through MCP tool `--permission-prompt-tool`.
- `crates/ertangle-daemon/src/handlers/session.rs:1514-1557` — Existing 300-second blocking `ask_user` approval waiter pattern in daemon.
- `crates/ertangle-protocol/src/lib.rs:251` — `Request::Permission` is currently the only interactive user request.
- `OpenCode Binary` (`/home/duye/.opencode/bin/opencode`) — Contains `opencode.tool.question`, `QuestionTool.Plugin`, and calls `e.ask({ sessionID, fields: [...] })`.
- `Codex Musl Binary` (`.../vendor/x86_64-unknown-linux-musl/bin/codex`) — Contains `item/tool/requestUserInput`, `McpElicitationTitledMultiSelectEnumSchema`, and `request_user_input requires an interactive stdin terminal`.
- ACP Specification: `agentclientprotocol.com` — Defines `elicitation/create` and `clientCapabilities.elicitation`.
