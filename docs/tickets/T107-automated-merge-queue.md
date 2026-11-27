# T107 — Automated Merge Queue with verification gate

Priority: P2　Scope: `ertangle-daemon`, `ertangle-core`　Depends on: T56, T99, T101

## Problem

When multiple workers finish tasks in parallel worktrees, the coordinator or human still has to manually merge each branch one by one. `ertangle` already has `pre_merge_command`, `ertangle check`, and `merge_worktree_strategy`, but no serialized automated merge queue that verifies and merges completed worker branches automatically.

## Evidence

- `crates/ertangle-daemon/src/config.rs:515-525` — `GitCfg` has `merge_strategy` and global-only `pre_merge_command`, but no `auto_merge_queue` setting.
- `crates/ertangle-daemon/src/handlers/task.rs:286-298` — `handle_update` detects `now_done && !was_done` and unblocks dependent tasks.
- `crates/ertangle-daemon/src/handlers/worktree.rs:125-170` — `handle_merge` runs `merge_worktree_strategy` and cleans up the worktree on success.

## Analysis / decisions

- Add `[git] auto_merge_queue: Option<bool>` (default `false` for safety; configurable globally or per-repo).
- When `auto_merge_queue` is enabled and a task transitions to `Done` (`now_done && !was_done`), if its `assignee` is a worker with a dedicated worktree (`s.branch.is_some()`), enqueue the session ID onto a serialized merge queue (`tokio`/worker thread protected by a mutex so merges never race).
- For each queued session:
  1. Ensure the worker has committed or auto-commit clean changes via `merge_worktree_strategy(repo, sess, strategy, false)`.
  2. Note: `merge_worktree_strategy` already runs `pre_merge_command` (if configured) before merging into the target branch!
  3. On success: clean up worktree (`remove_worktree`), emit a status/bus notification to the parent coordinator (`MERGE_DONE <session> <summary>`), and continue to the next queued branch.
  4. On failure (e.g. `pre_merge_command` failed or merge conflict): do not force-merge; notify the assignee / parent (`MERGE_FAILED <session> <reason>`) so the worker or coordinator can fix it.

## Implementation

1. `crates/ertangle-daemon/src/config.rs`: add `auto_merge_queue: Option<bool>` to `GitCfg` (layered global + local).
2. `crates/ertangle-daemon/src/handlers/worktree.rs`: extract `auto_merge_session(shared, sid)` that runs `merge_worktree_strategy` + worktree cleanup + bus notification to parent/assignee, serialized via a static mutex.
3. `crates/ertangle-daemon/src/handlers/task.rs`: when `now_done && !was_done`, if `auto_merge_queue` is enabled for the assignee's repo, spawn `auto_merge_session` on a background thread.
4. `crates/ertangle-daemon/src/tests.rs`: unit tests for `auto_merge_queue` config parsing, disabled-by-default behavior, and automated merge on task `done` (both success and failure notification).

## Acceptance criteria

- `auto_merge_queue` defaults to `false` (no behavior change unless enabled).
- When enabled, marking a worker's task `done` automatically merges its worktree branch (respecting `merge_strategy` and `pre_merge_command`), cleans up the worktree, and notifies the parent.
- Merge or pre-merge failure leaves the main branch untouched and notifies the worker/parent.
- `make pre-commit` and `cargo test -p ertangle-daemon` pass.
