# T59 — Open a project from any directory instead of starting ertangle inside it

Priority: P2　Scope: `ertangle` CLI, `ertangle-tui`　Depends on: —

Status: needs-triage

## Problem

The daemon fixes `default_cwd` at startup, the TUI derives `home_project` from its own process cwd, and TUI-created sessions always send that same process cwd as `SpawnRequest.cwd`. Working on a second repository therefore means leaving the TUI, `cd`-ing into that repository, and starting a new ertangle there. Passing `--cwd` to individual CLI spawns is not enough: the TUI project scope, `:stop`, notifications, and the project panel still point at the original repository.

## Evidence

- `crates/ertangle-daemon/src/server.rs:474-476,545` — `default_cwd = std::env::current_dir()` at daemon start; policy and config are loaded from it.
- `crates/ertangle-daemon/src/handlers/session.rs:24-37` — spawn cwd resolves as `spawn.cwd`, then the parent session's project, then `default_cwd`.
- `crates/ertangle-tui/src/app.rs:395` — `home_project: home_project(&cwd)`; the helper is at `app.rs:1249-1261`.
- `crates/ertangle-tui/src/commands.rs:1231-1235` — TUI spawn hard-codes `std::env::current_dir()` as `cwd`.
- `crates/ertangle-tui/src/commands.rs:586-591` — `:project` only toggles scope; it accepts no path.
- `src/main.rs:28-35,39-41,515` — the top-level CLI has global `--remote` and `--token`, while `Cmd::Tui` takes no arguments.
- `crates/ertangle-daemon/src/config.rs:166-179` — `TuiCfg` has layout, language, and notify settings but no project list.
- `crates/ertangle-daemon/src/handlers/session.rs:52-65` — a per-project `.ertangle/config.toml` is already loaded from the spawn cwd, so the daemon needs no change for project-specific config.

## Analysis / decisions

- Introduce an explicit "active project" in the TUI: `App.active_project: PathBuf`, initialized from `home_project(&cwd)`. It becomes the single source for (1) `home_project`, (2) `scope_here = true`, and (3) the `cwd` of newly spawned sessions. Do not change the shared daemon's `default_cwd`: one daemon serves many projects, and changing it would break other clients.
- Entry points:
  1. `ertangle --project <path>` as a global flag on `Cli` (`src/main.rs:28-35`), forwarded to `ertangle_tui::run(...)`, so the TUI can be launched from anywhere.
  2. `:project open <path>` in the TUI. Keep `:project here|all` and bare `:project` exactly as they are today; the new form only applies when the first argument is `open`/`set`.
  3. A project picker listing roots observed from `Session::project_root()` plus a configured `tui.projects` list.
- Path handling: canonicalize and require an existing directory; resolve the git toplevel with the existing `home_project()` helper (`app.rs:1249-1261`), falling back to the directory itself when it is not inside a repo. A missing path produces a clear status message and leaves state unchanged. Worktrees already resolve through `repo_root` (`crates/ertangle-core/src/lib.rs:359-363`).
- State transition on switch: update `active_project` and `home_project`, set `scope_here = true`, reset `selection` and `md_cache`, and refresh the session list. Everything already keyed off `home_project` — `:stop`, the `:shutdown` warning, notifications, and the project panel title — follows automatically.
- Persistence: store the last active project in the global config (`TuiCfg.last_project`) so a restart reopens it. Precedence is launch flag > persisted value > process cwd. Not confirmed: whether users want a persisted value to override the directory they launched from; verify this during review.
- Out of scope: web parity. The web UI already has its own project scope from T40; align the two in a follow-up ticket if needed.

## Implementation

- `src/main.rs`: add the global `--project` flag and pass it into the TUI entry point.
- `crates/ertangle-tui/src/lib.rs`: accept `Option<PathBuf>` and seed `App.active_project`.
- `crates/ertangle-tui/src/app.rs`: add the field and a `set_active_project` helper; use `home_project` as the spawn cwd instead of the process cwd.
- `crates/ertangle-tui/src/commands.rs`: extend `:project` parsing, add the picker, and keep the scope toggle behavior unchanged.
- `crates/ertangle-daemon/src/config.rs`: add `TuiCfg.projects` and `TuiCfg.last_project` (global config only).
- Update CLI help, README, and `locales/ertangle.pot` / `locales/en.po`.

## Acceptance criteria

- `ertangle --project /path/to/repo` sets the project panel and title to that repo, and sessions spawned from the TUI get a `cwd` inside it even when the TUI process cwd is elsewhere.
- `:project open <path>` switches at runtime, filters the session list to the new project, and is used as the cwd for the next spawn.
- A missing or non-directory path reports a clear error and changes nothing.
- The project picker lists both roots observed in sessions and entries from `tui.projects`.
- Precedence is flag > config > process cwd, verified by tests.
- `:project here|all` and `:scope` keep their current behavior.
- Unit tests cover path resolution, precedence, spawn cwd, and command parsing; `make pre-commit` and `make i18n-check` pass.
