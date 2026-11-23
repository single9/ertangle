# T93 — Include git commit hash in `ertangle` / `ert` version display

Priority: P3　Scope: `ertangle-core`, `ertangle-daemon`, `ertangle` CLI　Depends on: —

## Problem

`ert --version` and the TUI status bar (`local pid=... v1.2.0`) only show `CARGO_PKG_VERSION`, making it hard to tell which development commit is currently installed or running in the daemon.

## Implementation

1. `crates/ertangle-core/build.rs`:
   - Query `git rev-parse --short HEAD` (from `CARGO_MANIFEST_DIR`, falling back cleanly if `.git` is unavailable).
   - Emit `cargo:rustc-env=ERTANGLE_VERSION_WITH_HASH={pkg_version}+{hash}` (or `{pkg_version}` if git fails) and `cargo:rerun-if-changed=../../.git/HEAD`.
2. `crates/ertangle-core/src/lib.rs`:
   - Export `pub const VERSION_WITH_HASH: &str = env!("ERTANGLE_VERSION_WITH_HASH");`.
3. `src/main.rs`:
   - Use `version = ertangle_core::VERSION_WITH_HASH` in `#[command(...)]` so `ert --version` / `ertangle --version` includes the commit hash.
4. `crates/ertangle-daemon/src/lib.rs`:
   - Set `pub(crate) const VERSION: &str = ertangle_core::VERSION_WITH_HASH;` so `Frame::Ready` and the TUI status bar show `v1.2.0+<hash>`.
5. Add a unit test in `crates/ertangle-core/src/lib.rs` verifying `VERSION_WITH_HASH` starts with `env!("CARGO_PKG_VERSION")`.

## Acceptance criteria

- `ertangle --version` and the TUI status bar display `<semver>+<short_hash>` when built inside git.
- Builds cleanly even without `.git` (falls back to `<semver>`).
- `make pre-commit` and `cargo test -p ertangle-core -p ertangle-daemon --lib` pass.
