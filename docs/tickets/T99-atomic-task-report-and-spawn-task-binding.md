# T99 — Atomic task completion reporting in `ertangle_task_update` and task-bound `ertangle_spawn`

Status: done

Priority: P2　Scope: `ertangle-protocol`, `ertangle-daemon`　Depends on: T18, T37, T47

## Problem

1. **Multi-turn worker sign-off**: To finish or report a blocker on an assigned task, a worker currently has to make at least two separate MCP tool calls:
   - `ertangle_task_update(id, status="done")`
   - `ertangle_send(to=parent, body="TASK_DONE <id> <summary>")`
   Evidence from T47 shows 94%–96% of agent tool turns issue only a single tool call per turn. Splitting task status update and parent notification across two turns forces an extra full-context re-read (40k–128k tokens) and risks protocol drift (workers forgetting to update the task board or sending non-standard prose).
2. **Multi-turn coordinator dispatch**: When a coordinator spawns a worker for a task, it typically calls `ertangle_task_create` in one turn and `ertangle_spawn` in another turn (or vice versa, because the assignee session ID is not known until `ertangle_spawn` returns).

## Evidence

- `crates/ertangle-daemon/src/mcp.rs:306-331`, `798-825` — `ertangle_task_update` updates task fields but has no parent-reporting capability.
- `crates/ertangle-daemon/src/mcp.rs:333-379`, `827-872` — `ertangle_spawn` spawns a session without creating or linking a board task.
- `crates/ertangle-daemon/src/handlers/task.rs` — `handle_update` unblocks downstream `depends_on` tasks when a task becomes `Done`, but does not notify the session's parent.
- `crates/ertangle-daemon/src/collab.rs:25`, `66-68` — instructs workers to call both `ertangle_task_update` and `ertangle_send`.

## Analysis / decisions

- Decided: keep existing tools rather than adding new MCP tool names, keeping MCP `tools/list` schema overhead minimal.
- Decided: add optional `summary: Option<String>` (or `report: Option<String>`) to `ertangle_task_update`:
  - When `status` is `"done"` and `summary` is provided (or if the caller is a worker with a `parent`), the daemon/MCP layer atomically updates the task status AND sends `TASK_DONE <task_id> <summary>` to the calling session's `parent` (if any).
  - When `status` is `"blocked"` and `summary` is provided, it atomically updates the task status AND sends `TASK_BLOCKED <task_id> <summary>` to the `parent`.
- Decided: add optional `task_title: Option<String>` and `depends_on: Vec<String>` (or `task_id: Option<String>` to bind an existing task) to `ertangle_spawn`:
  - If `task_id` is passed, automatically assign that task to the newly spawned session and set its status to `doing` (or keep `blocked` if prerequisites are unfinished).
  - If `task_title` is passed, atomically create the task assigned to the newly spawned session and include the created `task_id` in the `ertangle_spawn` response JSON (`{ "ok": true, "id": "ses_...", "task_id": "task_...", "note": "..." }`) and in the initial prompt context.

## Implementation

1. `crates/ertangle-daemon/src/mcp.rs`:
   - In `ertangle_task_update` schema and handler: add optional `report` (`string`, description: `"Concise summary to automatically report TASK_DONE or TASK_BLOCKED to your parent session in the same call"`). When non-empty and the session has a `parent`, dispatch `Request::BusSend` with formatted `TASK_DONE <id> <report>` (when `status == Done`) or `TASK_BLOCKED <id> <report>` (when `status == Blocked`).
   - Also fix `ertangle_spawn` line 869 where `.find(|t| t.starts_with("task_"))` was accidentally looking for `task_` instead of `ses_` when extracting the spawned session ID!
   - In `ertangle_spawn` schema and handler: add optional `task_id` (`string`, bind existing task) and `task_title` (`string`, create and bind a new task to the spawned session). Return both `id` (`ses_...`) and `task_id` (`task_...`) in the JSON output.
2. `crates/ertangle-daemon/src/collab.rs`:
   - Update worker and coordinator collaboration instructions to mention the one-call `ertangle_task_update(id, status="done", report="...")` and `ertangle_spawn(..., task_title="...")` shortcuts.

## Acceptance criteria

- Calling `ertangle_task_update` with `status: "done"` and `report: "implemented feature X"` marks the task `done`, unblocks dependent tasks, and delivers `TASK_DONE <id> implemented feature X` to the worker's parent session in a single tool call.
- Calling `ertangle_spawn` with `task_title` returns both the new session `id` (`ses_...`) and the created `task_id` (`task_...`) assigned to that session.
- `ertangle_spawn` JSON `id` field accurately returns the `ses_...` ID.
- Unit tests cover atomic reporting, task binding on spawn, and session ID extraction; `make pre-commit` passes.
