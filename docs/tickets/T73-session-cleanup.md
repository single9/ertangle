# T73 — Session cleanup: auto-archive, retention, prune

Status: in-progress (daemon part done; TUI and Web parts pending)

## Problem

Finished worker sessions pile up in the session list and archived sessions grow without a time limit. The `1h` ended-session rule counted from creation time, not from when the session ended.

## Spec

### Daemon / core / protocol (done)

1. **Auto-archive agent-spawned workers** (`Session.parent` is `Some` and the session has a worktree; human-spawned sessions are never touched) when:
   - the branch is merged into the main branch (worktree gone, or `gc_block_reason` returns none: merged and no uncommitted changes), and
   - every task assigned to it is `done` (no tasks: the session must be idle/sleeping or ended, never busy or waiting for approval).
   Archive = stop the agent if alive, remove worktree and branch with the T03-safe path (`git branch -d`, no force), then `SessionRemoved` (which archives resumable sessions). If a safety check fails the session stays and gets a `SessionNote` ("not auto-archived: <reason>") when it is ended or has finished tasks; the note is only written when it changes.
2. **Periodic sweep** every 10 minutes (first pass 5 s after startup) runs the worker rule, the ended-session rule and archive retention. A successful non-dry-run `WorktreeMerge` also triggers a worker sweep right away.
3. **Ended-session rule**: `Session.ended_ms` (serde default, `None` in old logs) is set when a session stops being live and cleared when it becomes live again. Ended sessions without a worktree are archived 1 h after `ended_ms` (fallback: `created_ms`). Archiving now goes through `SessionRemoved` events so clients update.
4. **Retention**: `[sessions] archive_max` (default 100) and `archive_max_days` (default 30) in `config.toml`. Archived entries beyond the newest `archive_max` or older than `archive_max_days` (by ended time) are dropped via `Event::ArchivePruned { sessions }`. The hard cap inside `archive_session` was removed; the sweep enforces the limits.
5. **`Request::Prune { project }`** / `ertangle prune [--project <root>]`: archives every ended session that is merged or has no worktree, removing merged worktrees safely. Live sessions and unmerged work are never touched. Reply note: archived count plus the skipped list with reasons.
6. Clients learn about archiving through `SessionRemoved` (archived list is part of the state snapshot) and `ArchivePruned`.

### TUI (pending)

Ended sessions go into a collapsed "ended (N)" group at the bottom of the sidebar; `:prune` command and menu entry.

### Web (pending)

Ended sessions folded into a collapsed group, a "prune" button in the Sessions tab, and a collapsed archived list with resume.
