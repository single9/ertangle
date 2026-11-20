# T69 — Daemon restart that keeps sessions resumable

Status: done

## Problem

Swapping in a new `ertangle` binary required `ertangle shutdown` followed by a manual start, which killed every session and disconnected every client for good.

## Implementation

- Protocol: `Request::Restart` next to `Shutdown`. It goes through the same dispatch as every other op (unix socket, remote TCP token, web ws), so no new auth path.
- Daemon: `Shared::restart` shares the teardown of `shutdown` (kill agents, flush log, remove socket), then re-execs via `CommandExt::exec` with `current_exe()` (a trailing ` (deleted)` is stripped by `strip_deleted_suffix`) and `args_os().skip(1)`. On failure it logs and exits 1. No `unsafe`.
- The new daemon is started with `ERTANGLE_RESTARTED=1`; the orphan scan then auto-resumes in-flight sessions (Busy / WaitingApproval / Starting with a native id) even when `[agent_spawn] auto_resume` is off. Idle sessions keep the `:resume` hint.
- CLI: `ertangle restart` sends `Restart`, waits for the old socket to go away, then up to 10 s for the new daemon.
- TUI: `:restart` (needs `:restart!` while sessions are live) and menu `SPC … R`; the existing host reconnect loop reattaches.
- Web: "Restart daemon" button (with `confirm()`) in the session-manage sheet sends `{op:"restart"}`; the existing ws `onclose` retry reconnects.

## Acceptance criteria

- `ertangle restart` returns once the new daemon answers; sessions that were mid-turn resume automatically.
- TUI and web reconnect without manual action.
- `strip_deleted_suffix` and the TUI confirmation flow are unit-tested.
