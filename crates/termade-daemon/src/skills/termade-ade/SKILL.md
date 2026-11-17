---
name: termade-ade
description: Use when running inside termade (ADE) and coordinating or working as part of multiple agent sessions via the termade_* MCP tools (termade_spawn, termade_task_create, termade_send, termade_wait, termade_merge). Covers the message protocol, token/quota economy, and the task board workflow.
---

# termade ADE collaboration

Use this when you have `termade_*` MCP tools. Skip it for plain single-agent work.

## Message protocol (English, terse)

All messages via `termade_send` / `termade_ask` use these forms only:

- `TASK_DONE <task_id> <commit_or_summary>`
- `TASK_BLOCKED <task_id> <reason>`
- `QUERY: <question>` / `ANSWER: <reply>`
- `ACK` (only when required)
- `QUOTA_WARN <agent> <used>%: ...` is sent by termade itself; obey it.

No pleasantries, thanks, or sign-offs. Every message wakes the receiver for a full turn.
After sending `TASK_DONE`, stop your turn immediately.

## Token and quota economy

1. Do small tasks inline; spawn the fewest workers that parallelism really needs.
2. Give workers self-contained prompts: exact files, line ranges, acceptance checks, so they do not re-explore.
3. Prefer cheaper default models/effort for mechanical work; do not self-select expensive ones.
4. Check quota with `termade_whoami` before spawning. Spawn can be rejected or switched to a fallback agent when quota is high.
5. On `QUOTA_WARN`: stop spawning, tell workers to commit and report, then finish inline or wait for reset.
6. Reuse an idle worker with `termade_task_create` instead of spawning a new session.
7. Wait with `termade_wait`; never poll.

## Coordinator workflow

1. Break the goal into tasks; `termade_task_create` with `depends_on` for ordering.
2. `termade_spawn` workers (each gets its own git worktree/branch) with self-contained prompts.
3. `termade_wait` for `TASK_DONE` / `TASK_BLOCKED`; answer `QUERY` with `ANSWER`.
4. Review with `termade_diff`, then `termade_merge` each worker branch.
5. Update the task board (`termade_task_update`) and report to your own parent or the user.

## Worker workflow

1. Set your task to `doing` with `termade_task_update`; keep a one-line `termade_status`.
2. Stay inside the assigned scope; do not spawn sessions unless told to.
3. Commit on your worktree branch (do not merge into main yourself).
4. Mark the task `done`, send `TASK_DONE <task_id> <summary>` to the parent, stop.
