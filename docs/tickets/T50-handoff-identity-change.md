# T50 — Change a session's identity (role and parent) through a handoff

Priority: P2　Scope: `termade-protocol`, `termade-daemon`, `termade-tui`, `termade` CLI　Depends on: T49

Status: ready-for-agent

## Problem

A handoff can change the agent kind, model, and effort, but always copies the role and parent of the old session. Two situations cannot be expressed:

- A. The user starts a session directly and it becomes a `worker` by default, although it talks to the user and should coordinate.
- B. Mid-collaboration, the user wants to promote a worker to coordinator so it directs the rest of the work.

See [ADR 0001](../adr/0001-identity-change-via-handoff.md) for why identity changes go through a handoff and not an in-place `SetRole`. Terms follow `CONTEXT.md` (identity = role + parent).

## Evidence

- `handle_handoff` copies `role: src.role.clone()` and `parent: src.parent.clone()` (`crates/termade-daemon/src/handlers/session.rs:986`, `:991`) and copies `permission: Some(src.permission)`.
- `Request::Handoff` has no role or parent field (`crates/termade-protocol/src/lib.rs:130`).
- No request changes the role of an existing session.
- Sessions the user creates directly default to `worker`: CLI `--role` default (`src/main.rs:102`), TUI resets `spawn_role` to `Role::Worker` (`crates/termade-tui/src/menu.rs:662`). MCP also defaults to worker (`crates/termade-daemon/src/mcp.rs:714`).
- The handoff brief always says "you are taking over unfinished work" (`crates/termade-daemon/src/handoff.rs`).
- Messages to a handed-off session follow the successor chain (`crates/termade-daemon/src/bus.rs:232`).

## Analysis

Decisions (from a design session on 2026-11-19):

1. Identity changes only through a handoff the user triggers. The quota auto-handoff (`quota_watch.rs:334`) never changes identity.
2. New `[defaults] role`, factory value `coordinator`, applies only to sessions without a parent. Agent-created sessions (MCP) still default to `worker`. The TUI settings page exposes it.
3. `handoff --role <role> --parent <session|none>`. Without `--parent`: becoming a coordinator clears the parent; other roles keep the old parent; a session without a parent stays without one.
4. When the role changes, permission, model, and effort that are not given explicitly are resolved for the new role (`roles.<role>.*`). A manual bypass on the old session is not inherited. The handoff reports the permission that was finally applied.
5. Promotion of a worker that has a parent (situation B): open tasks assigned to the old session return to `todo` without an assignee, and the old parent gets a `TASK_BLOCKED <task> promoted to coordinator by user` message.
6. Demoting a coordinator that still has running children is refused unless `--force` is given. With `--force`, the brief and the TUI list those children with a warning.
7. When the role changes, the brief opens with the new role and parent ("Your role changed from worker to coordinator; you now report to the user") and marks the original task, last output, unread messages, and diff as background rather than work to resume.
8. The TUI handoff picker adds a role step after the agent step, defaulting to the current role.

Not confirmed: how agents react to a brief that marks work as background; verify on a real handoff.

## Implementation

- Protocol: `Request::Handoff` gains `role: Option<Role>`, `parent: Option<ParentChange>` (keep / none / session), `force: bool`, all `#[serde(default)]`. `SpawnRequest.role` becomes `Option<Role>`.
- Config: `Defaults.role`; `handle_spawn` resolves the role as request → (`[defaults] role` when there is no parent, else `worker`).
- Daemon `handle_handoff`: compute the new identity, check the demotion guard, reassign tasks and notify the old parent on promotion, skip copying permission when the role changes, pass role/parent to `handle_spawn`.
- Brief: `build_handoff_brief` takes the old and new identity and changes the header when they differ.
- CLI: `termade handoff --role --parent --force`; CLI `spawn --role` drops its default.
- TUI: role step in the handoff picker; `spawn_role` starts from `[defaults] role`; settings item for the default role.
- i18n catalogs updated.

## Acceptance criteria

- Unit tests: default role with and without parent; handoff to coordinator clears the parent; handoff keeps the parent for other roles; demotion with running children is refused without `--force`; promotion returns tasks to `todo` and notifies the old parent; permission is re-resolved when the role changes; the brief header reflects the role change; auto-handoff keeps the identity.
- `make pre-commit` passes.
