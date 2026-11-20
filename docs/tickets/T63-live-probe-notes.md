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

Binary: `./target/debug/ertangle`. Probes 1-4 used the build from `389a602`; final probes 5-9 used the rebuilt binary from `784a755` (grouped question cleanup, ACP `oneOf` parsing, and session-exit cleanup).

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

## Final verification (rebuilt binary `784a755`)

The daemon was restarted with a fresh socket and web token, and headless Chrome was relaunched against the current token, because ACP capabilities are negotiated at startup. Each probe ran in a new session after cleaning old ones.

### Probe 5 — OpenCode MCP question answered (final)

Session `ses_e83f096c05a6` answered `qst_05c394ab0f85` with `Postgres`.

```text
STATUS idle TOOLS 1
PENDING {}
Postgres.
```

**Result: PASS.**

### Probe 6 — OpenCode native `question` tool (final)

Session `ses_d8d08a3c9013` used the built-in tool. Two grouped questions appeared:

```json
{"id":"qst_82ae93630c87","header":"UI Framework","question":"Which UI framework should we use?",
 "options":[{"label":"React","description":"The more common option, large ecosystem and community"},
            {"label":"Vue","description":"Simpler learning curve, well-integrated tooling"}],
 "group":"qgrp_4b0eba80a4ed"}
{"id":"qst_471db683cf87","header":"UI Framework (other)","question":"Type your own answer",
 "options":[],"group":"qgrp_4b0eba80a4ed"}
```

Answering the first with `Vue` also cleared the sibling. Tool output: `User has answered your questions: "Which UI framework should we use?"="Vue".`

```text
STATUS idle TOOLS 2
PENDING {}
```

**Result: PASS.** Raw ACP `session/update` frames confirmed the empty `rawInput` placeholder is now ignored, and `elicitation/create` schemas expose `oneOf` variants that become labeled options.

### Probe 7 — Claude native `AskUserQuestion` (final)

Single question, session `ses_372a285dd66a`:

```text
✓ Your questions have been answered: "Which database should we use?"="Postgres".
```

Two questions in one call, session `ses_7736cf339c1e`:

```text
QUESTION qst_ee35593dd830 -> Postgres
QUESTION qst_d9e4557a3f2a -> React
✓ Your questions have been answered: "Which database should we use?"="Postgres", "Which UI framework s…
PENDING {}
```

**Result: PASS.** The answer path is `ertangle_approve` returning `behavior: "allow"` plus `updatedInput.answers`; injected tool results were abandoned after live probes still produced "The user did not answer the questions."

### Probe 8 — Web DOM submit and banner clearing (final)

Session `ses_c398b794f052`, pending `qst_fdef8277e8af`, answered from headless Chrome through the DevTools Protocol:

```text
BANNER {"display":"block","detail":"Cache selection：Which cache should we use?",
        "actions":"...radio Redis...radio Memcached...送出答案按钮"}
INPUT {"kind":"option","value":"Redis"}
AFTER {"display":"none"}
BANNER_AFTER {"display":"none"}
PENDING_COUNT 0
TRANSCRIPT_TAIL ... Redis
```

**Result: PASS.** The banner renders real options, the click submits the answer, the banner hides, and the agent continues with `Redis`. Earlier stale-banner observations were traced to pending questions surviving session exit and grouped siblings; both are now fixed.

### Probe 9 — Codex and Antigravity

Codex `item/tool/requestUserInput` answer shape and multi-input parsing are covered by unit tests (`user_input_answer_result_has_codex_shape` and the parser tests). A live native probe is not possible here: the tool is absent from the default-mode tool set; the binary reports `request_user_input requires an interactive stdin terminal` while the adapter uses piped stdin; the `experimental_request_user_input` toggle does not remove that requirement; and a fallback probe hit an unrelated token-mismatch error. Antigravity is not installed in this environment.

**Result: protocol/unit PASS; live probes BLOCKED (environment).**

## Coverage summary

| Path | Probe | Status |
| --- | --- | --- |
| OpenCode ACP + `ertangle_question` MCP | Probe 1, 5 | PASS |
| Web WebSocket `answer_question` | Probe 2 | PASS (wire path) |
| Web DOM rendering and submit handler | Probe 3, 8 | PASS |
| OpenCode native `question` tool | Probe 4, 6 | PASS |
| ACP raw `tool_call` fallback | Probe 6 | PASS |
| ACP elicitation `oneOf` options | Probe 6 | PASS |
| Claude native `AskUserQuestion` | Probe 7 | PASS (single and multi) |
| Codex `item/tool/requestUserInput` | Probe 9 | PASS (protocol/unit); live blocked |
| Multiple questions per request | Probe 6, 7 | PASS |
| Cancellable daemon timeout | unit test | PASS |
| Grouped / session-exit cleanup | unit tests, Probe 8 | PASS |
| Antigravity agent | Probe 9 | BLOCKED (binary not installed) |

## Notes

- The isolated daemon inherited `ERTANGLE_SESSION` from the coordinator shell, so probe sessions have the coordinator as their parent and receive the collaboration instructions. This did not affect the question path.
- The event frames sent by the daemon use `ev` for `ready`, `snapshot`, and `record`; client requests use `op`. The web client already follows this convention.
