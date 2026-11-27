# T109 — Fix Web UI session spawn failure, optional initial prompt, role selector, and case-insensitive wire enum aliases

Priority: P1　Scope: `ertangle-core`, `ertangle-protocol`, `ertangle-daemon`　Depends on: T30

## Problem

1. **Spawning a Claude session (or handing off to Claude) from the Web UI fails with a JSON parse error**:
   - In `crates/ertangle-core/src/lib.rs:102-110`, `AgentKind` uses `#[serde(rename_all = "snake_case")]`, so `AgentKind::ClaudeCode` expects `"claude_code"` on the wire.
   - However, the Web UI (`crates/ertangle-daemon/src/web/index.html:1913, 2030, 5143, 5229`) sends `kind: "claude"`, causing `serde_json` to reject `Request::Spawn` and `Request::Handoff` with `無法解析 JSON：unknown variant "claude"`.
2. **Web UI silently does nothing if the initial prompt is left empty**:
   - In `crates/ertangle-daemon/src/web/index.html:5131-5133`, `submitDetailedSession()` has `if (!promptText || !ws) return;` with no toast or feedback, whereas TUI allows spawning a session without an initial prompt (`prompt: None`) and also allows choosing the `Role` (`coordinator` / `worker` / `reviewer`), while Web UI hardcodes `role: "worker"`.
3. **Other Web UI operations send PascalCase enum strings while `serde` expects `snake_case`**:
   - `submitPermission('Allow' | 'Always' | 'Deny')` (`index.html:3663-3665, 3709-3711`) sends `"Allow"`, `"Always"`, `"Deny"`, whereas `Decision` (`crates/ertangle-core/src/lib.rs:717-723`) expects `"allow"`, `"allow_always"`, `"deny"`.
   - `cycleTaskStatus` (`index.html:5338-5343`) sends `"Todo"`, `"Doing"`, `"Done"`, whereas `TaskStatus` (`crates/ertangle-core/src/lib.rs:509-516`) expects `"todo"`, `"doing"`, `"done"`.

## Evidence

- `crates/ertangle-core/src/lib.rs:101-110` — `AgentKind::ClaudeCode` has no `#[serde(alias = "claude")]`.
- `crates/ertangle-core/src/lib.rs:509-516, 717-723` — `TaskStatus` and `Decision` lack aliases for PascalCase / `"Always"` values sent by Web UI.
- `crates/ertangle-daemon/src/web/index.html:5131-5156` — `submitDetailedSession()` silently returns when `promptText` is empty, does not select the newly created session, and hardcodes `role: "worker"`.

## Analysis / decisions

1. **Add serde aliases on wire enums in `ertangle-core`**:
   - `AgentKind::ClaudeCode`: add `#[serde(alias = "claude", alias = "claude-code", alias = "ClaudeCode")]` (and aliases for other variants as safety).
   - `Decision`: add `#[serde(alias = "Allow")]` on `Allow`, `#[serde(alias = "Always", alias = "always", alias = "AllowAlways")]` on `AllowAlways`, and `#[serde(alias = "Deny")]` on `Deny`.
   - `TaskStatus`: add `#[serde(alias = "Todo")]`, `#[serde(alias = "Doing")]`, `#[serde(alias = "Blocked")]`, `#[serde(alias = "Review")]`, `#[serde(alias = "Done")]`.
2. **Fix Web UI (`crates/ertangle-daemon/src/web/index.html`)**:
   - Allow `submitDetailedSession()` to spawn a session even when `new-prompt` is empty (`prompt: promptText || null`), matching TUI behavior.
   - Add a Role segmented selector (`預設` / `coordinator` / `worker` / `reviewer`) to `#sheet-new-session` so users can spawn a `coordinator` or `worker` from the Web UI (sending `role: newSessionState.role || null`).
   - Switch to the newly spawned session automatically when `session_started` arrives for a human-spawned session (or when no active session is selected).
   - Send lowercase snake_case values in `submitPermission` (`'allow'`, `'allow_always'`, `'deny'`) and `cycleTaskStatus` (`'todo'`, `'doing'`, `'done'`).

## Implementation

1. `crates/ertangle-core/src/lib.rs`: add `#[serde(alias = ...)]` attributes to `AgentKind`, `Decision`, and `TaskStatus`.
2. `crates/ertangle-protocol/src/lib.rs`: add unit tests verifying deserialization of Web UI JSON payloads for `Spawn`, `Handoff`, `Permission`, and `TaskUpdate`.
3. `crates/ertangle-daemon/src/web/index.html` & `crates/ertangle-daemon/src/web.rs`: update `submitDetailedSession`, add Role selector in `#sheet-new-session`, fix `submitPermission` and `cycleTaskStatus`, and add test coverage.

## Acceptance criteria

- Web UI can spawn `opencode`, `claude`, `antigravity`, and `codex` sessions with or without an initial prompt, and with selectable `role`.
- `Request::Spawn`, `Request::Handoff`, `Request::Permission`, and `Request::TaskUpdate` deserialize cleanly from Web UI JSON payloads.
- `make pre-commit` and `ertangle check` pass.
