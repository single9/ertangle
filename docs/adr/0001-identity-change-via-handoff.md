# 0001. Change a session's identity through a handoff, not in place

Status: accepted (2026-11-19)

## Context

Users want to change a session's role and parent, for example to promote a session they started directly from `worker` to `coordinator`, or to promote a worker mid-collaboration so it can direct the rest of the work.

Two designs were considered:

1. **In place**: add a `SetRole` request that rewrites the role and parent of the running session and keeps its id.
2. **Through a handoff**: stop the session and start a successor with the new identity, the same working directory, and a handoff brief.

## Decision

Identity changes go through a handoff (`termade handoff --role ... --parent ...`). There is no in-place role change.

## Reasons

- Role instructions and the permission policy are injected when a session is spawned. A running agent would not learn about an in-place change unless it were told separately, and its permission policy would stay stale.
- Handoff already stops the old agent, auto-commits the worktree, builds a brief, forwards unread messages, and routes later messages through the successor chain. Identity change reuses all of it.
- A handoff can change the agent kind at the same time (for example, an opencode worker becomes a claude coordinator).

## Consequences

- Changing identity restarts the agent and costs one new conversation.
- The session id changes; anything addressed to the old id follows the successor chain.
- The automatic handoff on quota exhaustion never changes identity. Only an explicit user action does.
