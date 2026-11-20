# T70 — Persist inbox cursors; warn on dead task assignee

Status: done

## Problem

1. `inbox_cursors` lived only in daemon memory, so after a daemon restart `ertangle_wait` / `ertangle_inbox` redelivered messages that were already read.
2. `ertangle_task_create` / `ertangle_task_update` with an assignee whose session was failed/exited (for example interrupted by a restart) silently stalled: the handoff message went to nobody.

## Implementation

- Core: `State.inbox_cursors` (session -> last delivered message id) and `Event::InboxRead { session, msg }`. The event goes through the normal log, so replay and snapshots rebuild the cursors. The event is only emitted when the cursor actually moves (once per delivering call, never per message).
- Daemon: `Shared::emit_locked` / `advance_inbox_cursor` let bus paths that already hold the state lock emit the event. The old in-memory `Inner.inbox_cursors` is gone.
- Tasks: `notify_assignee` auto-resumes a non-live assignee through `resume_session`, using the assignment text as the resume prompt. If that is impossible, the reply note gets an extra `warning: assignee <id> is <status> and could not be resumed (...); reassign the task or :resume it` line. The MCP tools expose it as a `warning` field in their result.

## Tests

- Core: `InboxRead` apply and serde round trip.
- Daemon: cursor survives restore from log and a second unread read returns nothing; unchanged cursor emits no event; dead unresumable assignee yields a warning and no inbox message; live assignee yields no warning.
