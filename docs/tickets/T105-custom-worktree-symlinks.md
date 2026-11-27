# T105 — Custom worktree `symlink_paths` and `.worktreeinclude`

Priority: P2　Scope: `ertangle-daemon`　Depends on: T100

## Problem

`provision_worktree_deps` automatically symlinks well-known untracked dependencies (`node_modules`, `.venv`, `.env*`, `appsettings.Development.json`, `.godot/imported`), but projects with custom untracked files or asset directories (such as local secrets, certificates, or test fixtures) have no way to declare additional paths to link into new worktrees.

## Evidence

- `crates/ertangle-daemon/src/git.rs:44-174` — `provision_worktree_deps` hardcodes the list of provisioned paths and records them in `.ertangle/provisioned_links`.
- `crates/ertangle-daemon/src/config.rs:58-75` — `AgentSpawnCfg` has `shared_worktree_deps`, but no `symlink_paths` list.

## Analysis / decisions

- Support two declarative sources (merged and deduplicated):
  1. `[agent_spawn] symlink_paths = [...]` in config (can be set in repo-local `.ertangle/config.toml` as well as global config, since paths are strictly validated to stay inside `repo`).
  2. `.worktreeinclude` at the repo root (one relative path per line; blank lines and `#` comments ignored).
- Security/safety validation: reject empty paths, absolute paths, paths containing `..` (`ParentDir`), and `.git` paths. Skip any path that is git-tracked (`is_git_tracked`), missing in `repo`, or already exists in `wt_dir`.
- Record created symlinks in `.ertangle/provisioned_links` so `cleanup_provisioned_links` removes them cleanly on worktree removal.

## Implementation

1. `crates/ertangle-daemon/src/config.rs`: add `symlink_paths: Vec<String>` to `AgentSpawnCfg` and layer it (local extends/overrides if non-empty, or union? Wait: union of global + local so both global and repo-local entries apply).
2. `crates/ertangle-daemon/src/git.rs`: add `read_worktreeinclude(repo)` and `provision_custom_symlinks(repo, wt_dir, paths, provisioned)` called from `provision_worktree_deps_with_paths` (keeping `provision_worktree_deps` as a thin wrapper for existing callers/tests).
3. `crates/ertangle-daemon/src/handlers/worktree.rs`: pass `cfg.agent_spawn.symlink_paths` into worktree provisioning when `shared_worktree_deps` is enabled.
4. `crates/ertangle-daemon/src/git.rs` / `tests.rs`: unit tests for `.worktreeinclude`, `symlink_paths`, path-traversal rejection, and cleanup.

## Acceptance criteria

- Valid relative paths from `.worktreeinclude` and `[agent_spawn] symlink_paths` are symlinked into new worktrees and recorded in `.ertangle/provisioned_links`.
- Absolute paths, `..` traversal, `.git` paths, and git-tracked files are ignored without error.
- `make pre-commit` and `cargo test -p ertangle-daemon` pass.
