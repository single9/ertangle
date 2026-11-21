# T80: Prevent silent delivery to unavailable peers

Priority: P1 / Scope: daemon bus and MCP peers / Depends on: T24

## Problem

A message sent to an idle peer whose reported quota was already exhausted could trigger a wakeup that failed immediately. The message remained in the mailbox, but the sender received no indication that the peer could not read it. The peers listing also omitted quota availability.

## Evidence

`Shared::deliver_message_noted` considered an idle session wakeable without checking its reported quota. `ertangle_peers` returned session status but no quota state. A failed agent wakeup therefore looked like a successful delivery to MCP callers.

## Analysis / decisions

The original incident involved an idle session whose quota had reached its limit. Quota recovery behavior is owned by existing quota reporting; this ticket only prevents wake attempts while reported quota is exhausted.

## Implementation

- Treat a session as unavailable for direct message routing when it is no longer live or its quota is exhausted.
- Route to a usable handoff successor when one exists; otherwise retain the message in the mailbox and explain that it will not be read until quota recovers.
- Skip quota-exhausted idle sessions during wakeup and add a warning to the send response.
- Expose quota usage, quota exhaustion, and unavailability reason in `ertangle_peers`.

## Acceptance criteria

- Exhausted idle peers are not woken, and their messages remain in the mailbox.
- Send responses warn MCP callers when an exhausted peer cannot be woken.
- A usable successor receives messages addressed to an exhausted original session.
- `ertangle_peers` exposes quota and availability fields for every session.
- Daemon formatting, lint, tests, and pre-commit checks pass.
