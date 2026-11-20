---
name: ertangle-ade
description: Use when running inside ertangle (ADE) and coordinating or working as part of multiple agent sessions via the ertangle_* MCP tools (ertangle_spawn, ertangle_task_create, ertangle_send, ertangle_wait, ertangle_merge). Covers the message protocol, token/quota economy, and the task board workflow.
---

# ertangle ADE collaboration

Use this when you have `ertangle_*` MCP tools. Skip it for plain single-agent work.

## Message protocol (English, terse)

All messages via `ertangle_send` / `ertangle_ask` use these forms only:

- `TASK_DONE <task_id> <commit_or_summary>`
- `TASK_BLOCKED <task_id> <reason>`
- `QUERY: <question>` / `ANSWER: <reply>`
- `ACK` (only when required)
- `QUOTA_WARN <agent> <used>%: ...` is sent by ertangle itself; obey it.

No pleasantries, thanks, or sign-offs. Every message wakes the receiver for a full turn.
After sending `TASK_DONE`, stop your turn immediately.

## Token and quota economy

1. Do small tasks inline; spawn the fewest workers that parallelism really needs.
2. Give workers self-contained prompts: exact files, line ranges, acceptance checks, so they do not re-explore.
3. Prefer cheaper default models/effort for mechanical work; do not self-select expensive ones.
4. Check quota with `ertangle_whoami` before spawning. Spawn can be rejected or switched to a fallback agent when quota is high.
5. On `QUOTA_WARN <agent>`: stop spawning `<agent>` workers (switch to another agent with available quota if needed) and have active `<agent>` workers commit and report.
6. Reuse an idle worker with `ertangle_task_create` instead of spawning a new session.
7. Wait with `ertangle_wait`; never poll.

## Coordinator workflow

1. Break the goal into tasks; `ertangle_task_create` with `depends_on` for ordering.
2. `ertangle_spawn` workers (each gets its own git worktree/branch) with self-contained prompts.
3. `ertangle_wait` for `TASK_DONE` / `TASK_BLOCKED`; answer `QUERY` with `ANSWER`.
4. Review with `ertangle_diff`, then `ertangle_merge` each worker branch.
5. Update the task board (`ertangle_task_update`) and report to your own parent or the user.

## Worker workflow

1. Set your task to `doing` with `ertangle_task_update`; keep a one-line `ertangle_status`.
2. Stay inside the assigned scope; do not spawn sessions unless told to.
3. Commit on your worktree branch (do not merge into main yourself). Never use `git stash` in a worktree: the stash stack is shared across all worktrees; use a WIP commit instead.
4. Mark the task `done`, send `TASK_DONE <task_id> <summary>` to the parent, stop.
