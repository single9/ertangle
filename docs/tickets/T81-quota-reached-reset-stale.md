# T81 — Stale `Quota::reached` after rate-limit window resets

Priority: P1　Scope: `ertangle-core`, `ertangle-daemon`, `ertangle-tui`, `web`　Depends on: —

## Problem

When an agent (e.g. Claude) hits a rate limit on a short window (`5h`), `Quota::reached` is set to `true` and persisted in daemon state. Once the `5h` window's `resets_at_ms` passes (`<= now_ms`), no new `rate_limit_event` arrives if no session of that agent is running. As a result:

- `Quota::max_used_live(now)` reports the remaining live window (e.g. `7d 51%`), and `Quota::summary(now)` prints `5h 已重置 · 7d 51% (重置 3d5h)` — yet still appends `✗額度已滿` because `self.reached` is stale.
- TUI header (`quota_spans`), TUI `:quota` popup (`quota.rs`), Web UI (`renderQuota()`), `ertangle_whoami` / `ertangle_peers` (`mcp.rs`), `bus.rs` (`quota_exhausted`), `quota_watch.rs` (`check_spawn_quota`, `pick_fallback`, `warn_if_quota_high`), `config.rs` (`resolve_worker_agent`), and `lib.rs` (`auto_resume_eligible`) all treat the agent as exhausted.

## Evidence

- `crates/ertangle-core/src/lib.rs:2155-2176` (`Quota::summary` checks `self.reached` directly).
- `crates/ertangle-tui/src/quota.rs:110` (`agent_lines` checks `q.reached`).
- `crates/ertangle-tui/src/draw.rs:1140` (`quota_spans` colors red if `q.reached`).
- `crates/ertangle-daemon/src/web/index.html:3813` (`renderQuota` checks `q.reached`).
- `crates/ertangle-daemon/src/quota_watch.rs:118,162,311` (`check_spawn_quota`, `pick_fallback`, `warn_if_quota_high`).
- `crates/ertangle-daemon/src/config.rs:951` (`resolve_worker_agent` checks `q.reached`).
- `crates/ertangle-daemon/src/lib.rs:1166` (`auto_resume_eligible` checks `q.reached`).
- `crates/ertangle-daemon/src/mcp.rs:595,618` (`ertangle_whoami` returns `q.reached`; `ertangle_peers` checks `q.reached || q.max_used() >= 100.0`).
- `crates/ertangle-daemon/src/bus.rs:11` (`quota_exhausted` checks `quota.reached || quota.max_used() >= 100.0`).

## Analysis / decisions

Add a time-aware `Quota::is_reached(&self, now_ms: u64) -> bool` method on `ertangle_core::Quota`:

- If `self.windows.is_empty()`: return `self.reached` (error-only rate limit without window timestamps, e.g. ACP `RESOURCE_EXHAUSTED`).
- Otherwise:
  - If any active (`!self.is_inactive_window(w)`) live (`w.resets_at_ms.is_none_or(|r| r > now_ms)`) window has `w.used_percent >= 100.0`: return `true`.
  - If `self.reached` is `true` and **no** active window has reset (`!self.windows.iter().any(|w| !self.is_inactive_window(w) && w.resets_at_ms.is_some_and(|r| r <= now_ms))`): return `true`.
  - Otherwise: return `false` (the exhausted window has reset and remaining live windows are `< 100%`).

Apply the same logic in `crates/ertangle-daemon/src/web/index.html` (`renderQuota`) so the Web UI matches `Quota::is_reached(now_ms)`. Also replace `q.max_used()` in `bus.rs` and `mcp.rs` with `q.max_used_live(now)` (or `q.is_reached(now)`).

## Implementation

1. `crates/ertangle-core/src/lib.rs`: add `pub fn is_reached(&self, now_ms: u64) -> bool`; use `self.is_reached(now_ms)` in `Quota::summary`.
2. `crates/ertangle-tui/src/quota.rs` and `crates/ertangle-tui/src/draw.rs`: use `q.is_reached(now)`.
3. `crates/ertangle-daemon/src/quota_watch.rs`, `config.rs`, `lib.rs`, `mcp.rs`, `bus.rs`: use `q.is_reached(now_ms)` (passing `now_millis()` where needed, e.g. `auto_resume_eligible`, `quota_exhausted`, `ertangle_whoami`, `ertangle_peers`).
4. `crates/ertangle-daemon/src/web/index.html`: compute `isReached` in `renderQuota()` using live vs reset windows.
5. Add unit tests in `ertangle-core`, `ertangle-daemon`, and `ertangle-tui` covering a `reached: true` quota whose `5h` window has reset while `7d` is `< 100%` (and when `7d` is `100%`).

## Acceptance criteria

- `Quota::is_reached(now)` returns `false` when the `5h` window has reset (`resets_at_ms <= now`) and `7d` is `51%`, and `true` if another live active window is `>= 100%` or no active window has reset yet.
- `Quota::summary`, TUI header, TUI `:quota`, Web UI, `ertangle_whoami`, `ertangle_peers`, `bus.rs`, and `quota_watch.rs` all agree and no longer show `✗額度已滿` or block spawns/messages after reset.
- `make pre-commit` and `cargo test --workspace --lib` (with `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false`) pass.
