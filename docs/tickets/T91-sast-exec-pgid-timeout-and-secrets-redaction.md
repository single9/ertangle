# T91 — SAST: kill process group on `handle_exec` timeout and expand `redact_secrets`

Priority: P1　Scope: `ertangle-daemon` (`handlers/session.rs`), `ertangle-core`　Depends on: T74

## Problem

1. **Orphan subprocesses on `handle_exec` timeout (`Medium`)**: `crates/ertangle-daemon/src/handlers/session.rs` (~L1070) calls `child.kill()` when the 60s timeout expires, which only kills the parent `sh` process and leaves background child processes alive.
2. **Limited `redact_secrets` patterns (`Low`)**: `crates/ertangle-core/src/lib.rs` (~L2420) only covers 6 secret prefixes and misses common tokens such as GitHub fine-grained/OAuth/app tokens (`github_pat_`, `gho_`, `ghu_`), GitLab (`glpat-`), Slack (`xoxb-`, `xoxp-`), and Google API keys (`AIza`).

## Implementation

1. `crates/ertangle-daemon/src/handlers/session.rs` (`handle_exec`):
   - On Unix, use `std::os::unix::process::CommandExt::process_group(0)` (safe, no `unsafe` required) when spawning `sh -c <command>` so it runs in its own process group.
   - On timeout, terminate the entire process group via `ertangle_adapter::signal_pgid(child.id(), "-KILL")` before `child.wait()`.
2. `crates/ertangle-core/src/lib.rs` (`redact_secrets`):
   - Add prefixes `github_pat_`, `gho_`, `ghu_`, `glpat-`, `xoxb-`, `xoxp-`, `AIza` to secret redaction.
3. Add unit tests in `ertangle-core` (new prefixes) and `ertangle-daemon` (`handle_exec` timeout cleanup).

## Acceptance criteria

- `handle_exec` spawns in a dedicated process group and kills the process group on timeout without `unsafe`.
- `redact_secrets` redacts `github_pat_...`, `gho_...`, `glpat-...`, `xoxb-...`, `AIza...` while preserving normal text.
- `make pre-commit` and `cargo test -p ertangle-core -p ertangle-daemon --lib` pass.
