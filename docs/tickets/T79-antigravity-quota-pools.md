# T79: Antigravity multi-model quota false positive

Status: ready-for-human

## Problem

Antigravity reports shared Gemini and Argon quota windows alongside `3p-*` windows for third-party models. The third-party windows do not apply to Gemini or Argon, but agent-level quota decisions currently take the maximum across all reported windows. A high third-party weekly percentage can therefore cause `ertangle_whoami` and `QUOTA_WARN` to report that Antigravity is unavailable, blocking workers that use Gemini or Argon.

## Evidence

- `crates/ertangle-adapter/src/acp.rs:1237` parses Antigravity quota output and retains Gemini, Argon, and third-party windows in one `Quota`.
- `crates/ertangle-core/src/lib.rs:2174` previously computed `Quota::max_used()` across every window.
- `crates/ertangle-daemon/src/quota_watch.rs:19` previously computed live pressure across every non-reset window, and `:114-132` selected the warning window and reset hint from those windows.
- `crates/ertangle-daemon/src/mcp.rs:588-601` previously reported the maximum across every live window in `ertangle_whoami`.

## Decision

Treat Antigravity `3p-*` windows as inactive for agent-level quota decisions. Keep them visible in quota summaries as informational data. Exclude windows whose reset time is in the past from live quota calculations and spawn summaries. Other agents retain their existing window behavior.

## Tests

- Adapter sample: `3p-7d` at 90% and Gemini `7d` at 61% produce `max_used()` and `max_used_live()` of 61%, with `reached == false`.
- Core: inactive third-party windows and expired windows are excluded; third-party-named windows remain active for other agents, and summaries still show all windows.
- Daemon: spawn quota checking allows Antigravity when only its third-party window exceeds the warning threshold.
