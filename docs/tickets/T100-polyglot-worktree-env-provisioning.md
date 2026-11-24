# T100 — Polyglot worktree environment provisioning (Node/Vite `node_modules`, Python `.venv`, and .NET Core env)

Status: ready-for-agent

Priority: P2　Scope: `ertangle-daemon`　Depends on: T46

## Problem

In T46, `ertangle` added `shared_cargo_target` so Rust worktrees share `<repo>/.ertangle/target` via `CARGO_TARGET_DIR`. However, in frontend (Vite / npm / yarn / pnpm / bun), Python (`uv` / `.venv`), and .NET Core (`.sln` / `.csproj`) projects, a newly created git worktree starts completely bare of untracked dependencies:

1. **Frontend (`node_modules`)**: `git worktree add` does not copy `node_modules/`. Every worker spawned into a worktree immediately fails on `vite build`, `tsc`, or `vitest` with `command not found` or `Cannot find module`, wasting 1–3 turns and hundreds of megabytes of disk space running `npm install` / `yarn` per worktree.
2. **Python (`.venv`)**: Worktrees lack `.venv/`, causing `pytest` or `python` imports to fail unless the worker discovers and activates the main repo's virtualenv.
3. **.NET Core (`dotnet`)**: Every `dotnet` invocation in a new session prints telemetry notices and welcome banners into the agent's tool output unless `DOTNET_NOLOGO=1` and `DOTNET_CLI_TELEMETRY_OPTOUT=1` are set.
4. **Untracked local environment files (`.env*`, `appsettings.Development.json`)**: Frontend (Vite `.env`, `.env.local`, `.env.development.local`) and .NET Core (`appsettings.Development.json`) projects store local config/secrets in gitignored files. A fresh worktree lacks these files, breaking local tests and dev servers immediately.

## Evidence

- `crates/ertangle-daemon/src/git.rs:8-25` — `make_worktree` runs `git worktree add -b ertangle/<id> <dir>` and returns immediately without linking dependencies.
- `crates/ertangle-daemon/src/handlers/session.rs:495-508` — `launch_session` only configures `CARGO_TARGET_DIR` for Cargo projects.
- `crates/ertangle-daemon/src/config.rs` — `[agent_spawn]` has `shared_cargo_target`, with no equivalent for Node, Python, or .NET environment provisioning.

## Analysis / decisions

- Decided: extend worktree provisioning in `make_worktree` and `launch_session` so polyglot projects work out-of-the-box with zero agent turns spent on setup:
  1. **Node.js / Frontend (`node_modules`)**:
     - When `make_worktree(repo, id)` creates `wt_dir`, if `repo.join("node_modules").is_dir()` and `!wt_dir.join("node_modules").exists()`, create a directory symlink (`std::os::unix::fs::symlink` on Unix, `symlink_dir` / junction on Windows) from `wt_dir/node_modules` -> `repo/node_modules`.
     - In `launch_session`, if `wt_dir/node_modules/.bin` exists, prepend it to the session's `PATH` so agents can run `vite`, `tsc`, `vitest`, `eslint` directly without `npx`.
     - Ignore symlinked `node_modules` in `git_status_summary`, `has_uncommitted`, and `compute_worktree_diff` if not already in `.gitignore` (or verify `git status` ignores it when `.gitignore` lists `node_modules`).
  2. **Python (`.venv`)**:
     - If `repo.join(".venv").is_dir()` and `!wt_dir.join(".venv").exists()`, symlink `wt_dir/.venv` -> `repo/.venv`.
     - In `launch_session`, if `.venv/bin` exists, set `VIRTUAL_ENV` and prepend `.venv/bin` to `PATH`.
  3. **.NET Core (`.sln` / `.slnx` / `.csproj`)**:
     - In `launch_session`, when the project contains `.sln`, `.slnx`, or `.csproj` (or unconditionally as safe defaults if unset in host env), inject `DOTNET_NOLOGO=1`, `DOTNET_CLI_TELEMETRY_OPTOUT=1`, and `DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1`, plus `CI=1` / `NO_COLOR=1` if appropriate.
  4. **Untracked local config files (`.env*`, `appsettings.Development.json`)**:
     - When `make_worktree(repo, id)` provisions `wt_dir`, scan the top-level of `repo` (and immediate web project directories for `appsettings.Development.json`) for untracked `.env`, `.env.local`, `.env.development`, `.env.development.local`, `.env.test`, and `appsettings.Development.json` files, and symlink (or copy) them into `wt_dir` if absent.
     - Ensure `git_status_summary`, `has_uncommitted`, `auto_commit_worktree`, and `compute_worktree_diff` never stage or commit these provisioned untracked files if they happen not to be covered by `.gitignore`.
- Decided: add `[agent_spawn] shared_worktree_deps = true` (default `true`, backward-compatible with `shared_cargo_target`) in `config.rs` so users can disable auto-symlinking if a specific project requires isolated `node_modules`.

## Implementation

1. `crates/ertangle-daemon/src/config.rs`:
   - Add `shared_worktree_deps: Option<bool>` (default `true`) under `AgentSpawnConfig`.
2. `crates/ertangle-daemon/src/git.rs`:
   - Add `provision_worktree_deps(repo: &Path, wt_dir: &Path)` called at the end of `make_worktree` when `shared_worktree_deps` is enabled.
   - Symlink top-level `node_modules` and `.venv` from `repo` into `wt_dir` if present in `repo` and absent in `wt_dir`. Log a warning and continue if symlink creation fails.
   - Ensure `remove_worktree` safely removes symlinked directories before or during `git worktree remove`.
3. `crates/ertangle-daemon/src/handlers/session.rs`:
   - In `launch_session`, detect `.venv/bin` and `node_modules/.bin` in `cwd` (or `repo_root`) and prepend them to `PATH` (and set `VIRTUAL_ENV`).
   - Inject `DOTNET_NOLOGO=1`, `DOTNET_CLI_TELEMETRY_OPTOUT=1`, and `DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1` when not already set in the environment.

## Acceptance criteria

- Creating a worktree in a repo with `node_modules/` or `.venv/` automatically creates a symlink in the worktree directory when `shared_worktree_deps` is true.
- `remove_worktree` cleanly removes the worktree without deleting the contents of the main repo's `node_modules/` or `.venv/`.
- Sessions launched in a worktree get `node_modules/.bin` / `.venv/bin` on `PATH` and `.NET` no-logo environment variables set.
- Unit tests verify symlink provisioning, safe removal (main repo files untouched!), and env injection; `make pre-commit` passes.
