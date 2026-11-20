# T63 — Live probe notes

These probes use an isolated daemon so the coordinator session is unaffected.

## Environment

```sh
mkdir -p /tmp/opencode/t63-verify/{state,config,runtime,logs}
env \
  ERTANGLE_SOCKET=/tmp/opencode/t63-verify/daemon.sock \
  XDG_STATE_HOME=/tmp/opencode/t63-verify/state \
  XDG_CONFIG_HOME=/tmp/opencode/t63-verify/config \
  XDG_RUNTIME_DIR=/tmp/opencode/t63-verify/runtime \
  ./target/debug/ertangle daemon --web 127.0.0.1 --web-port 7421
```

Binary: `./target/debug/ertangle` built from `389a602` (T62 core, protocol-shape fixes, and web question UI).

## Probe 1 — OpenCode MCP question answered over the daemon socket

Command:

```sh
env ERTANGLE_SOCKET=/tmp/opencode/t63-verify/daemon.sock \
  XDG_STATE_HOME=/tmp/opencode/t63-verify/state \
  XDG_CONFIG_HOME=/tmp/opencode/t63-verify/config \
  ./target/debug/ertangle spawn --kind opencode --permission ask \
  --label probe/opencode \
  --prompt "Use the ertangle_question MCP tool to ask me: Which database should we use? Options: Postgres, Redis. Wait for the answer, then reply with the selection." \
  --cwd /tmp/opencode/t63-verify
```

Observed session state after the agent called the tool:

```json
"status": "waiting_approval",
"tool_calls": 1,
"pending_questions": {
  "qst_dc6635acbf38": {
    "id": "qst_dc6635acbf38",
    "session": "ses_3f787670d395",
    "header": "Database",
    "question": "Which database should we use?",
    "options": [{"label": "Postgres"}, {"label": "Redis"}],
    "multi_select": false,
    "custom": true
  }
}
```

Answer sent as one JSON line over the Unix socket:

```json
{"op":"answer_question","session":"ses_3f787670d395","id":"qst_dc6635acbf38","answers":["Postgres"]}
```

Daemon reply: `{"ev":"ok","note":null}`. The question disappeared from `pending_questions`, the session returned to `idle`, and the transcript showed the tool result followed by `Postgres.`.

**Result: PASS.** The OpenCode ACP session can raise a question through the `ertangle_question` MCP tool and continue with the human answer.

## Probe 2 — OpenCode question answered from the web client

Command:

```sh
env ERTANGLE_SOCKET=/tmp/opencode/t63-verify/daemon.sock \
  XDG_STATE_HOME=/tmp/opencode/t63-verify/state \
  XDG_CONFIG_HOME=/tmp/opencode/t63-verify/config \
  ./target/debug/ertangle spawn --kind opencode --permission ask \
  --label probe/opencode-web \
  --prompt "Use the ertangle_question MCP tool to ask me: Which cache should we use? Options: Redis, Memcached. Wait for the answer, then reply with the selection." \
  --cwd /tmp/opencode/t63-verify
```

A Node 22 WebSocket client connected to `ws://127.0.0.1:7421/ws`, sent the `hello` frame with the token from the daemon log, and read the snapshot:

```json
{"ev":"snapshot","state":{"pending_questions":{"qst_faf8c096d05e":{...}}}}
```

The client sent the same payload the web UI sends:

```json
{"op":"answer_question","session":"ses_830dca047482","id":"qst_faf8c096d05e","answers":["Redis"]}
```

The question disappeared from `pending_questions`, the transcript showed `✓ {"answers":["Redis"]}`, and the agent replied `Redis`.

**Result: PASS for the web wire path.** The browser rendering itself was checked with `node --check`; a real browser click is still outstanding.

## Probe 3 — Headless browser rendering and click

A pending question from a separate OpenCode ACP session was opened in headless Chrome through the DevTools Protocol. The chat banner rendered as:

```json
{"display":"block","title":"待回答問題 (Waiting Question)","detail":"question：{}"}
```

The review panel and banner both exposed the question's custom text input and the `submitQuestionAnswer('banner')` button. Typing `Vue` and pressing the button called the web client's answer path without a JavaScript exception.

**Result: PARTIAL.** The browser UI renders pending questions and the submit handler runs. The question did not clear because it came from an ACP adapter event rather than the MCP waiter; `ctl.resolve_question` failed for that id, so no `question_resolved` event was emitted. This is the ACP tool-call fallback gap listed below.

## Probe 4 — ACP native `question` tool produces unusable question payloads

Session `ses_c093f04f28e9` used OpenCode's native `question` tool instead of the MCP tool. The event log showed:

- seq 93 `question_requested` with `question: "{}"`, `header: "question"`, `options: []`, created from a `session/update` tool call named `question` whose input was `{}`.
- seq 95 another `question_requested` with `header: "UI Framework"`, `question: "Questions"`, `options: []`, created from an ACP `elicitation/create` schema.

Both pending questions were unanswerable: no options, no meaningful question text, and answering them did not emit `question_resolved`.

**Result: FAIL.** This is direct evidence for the ACP tool-call fallback and elicitation parsing items. The worker received these details during implementation.

## Coverage summary

| Path | Probe | Status |
| --- | --- | --- |
| OpenCode ACP + `ertangle_question` MCP | Probe 1 | PASS |
| Web WebSocket `answer_question` | Probe 2 | PASS (wire path) |
| Web DOM rendering and submit handler | Probe 3 | PARTIAL (blocked by ACP fallback) |
| OpenCode native `question` tool | Probe 4 | FAIL (pending worker fix) |
| Claude native `AskUserQuestion` | — | Pending worker implementation |
| Codex `item/tool/requestUserInput` | — | Pending worker implementation |
| ACP raw `tool_call` fallback | — | Pending worker implementation |
| Multiple questions per request | — | Pending worker implementation |
| Cancellable daemon timeout | — | Pending worker implementation |
| Antigravity agent | — | Binary not installed in this environment |

## Notes

- The isolated daemon inherited `ERTANGLE_SESSION` from the coordinator shell, so probe sessions have the coordinator as their parent and receive the collaboration instructions. This did not affect the question path.
- The event frames sent by the daemon use `ev` for `ready`, `snapshot`, and `record`; client requests use `op`. The web client already follows this convention.
