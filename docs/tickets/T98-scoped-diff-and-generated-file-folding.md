# T98 — Scoped `ertangle_diff` (`stat_only`, `paths`, `max_lines`) and auto-folding for giant lock/designer files

Status: done

Priority: P2　Scope: `ertangle-protocol`, `ertangle-daemon`, `ertangle`　Depends on: T47

## Problem

When a coordinator or reviewer calls `ertangle_diff` (or `ertangle diff <session>`), `compute_worktree_diff` always returns the entire unified diff for every tracked and untracked file in the worktree.

In polyglot repositories, a single worker branch frequently modifies or generates thousands of lines of machine-generated lockfiles, snapshots, or designer files:
- Rust: `Cargo.lock`, `locales/*.po`
- Frontend (npm / yarn / pnpm / Vite): `package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `bun.lock`, `*.snap`, `*.min.js`
- .NET Core (C# / EF Core): `*.Designer.cs`, `*ModelSnapshot.cs`, `packages.lock.json`, `project.assets.json`
- Python / Go: `uv.lock`, `poetry.lock`, `go.sum`

Dumping a 20,000-character diff into the coordinator's context consumes thousands of tokens on every subsequent turn (see T47: top 15 large tool results accounted for 30% of total characters). Coordinators also lack a way to inspect a diff summary (`stat_only`) or filter to specific paths (`paths`) before requesting full hunks.

## Evidence

- `crates/ertangle-daemon/src/mcp.rs:458-470`, `962-978` — `ertangle_diff` accepts only `session`.
- `crates/ertangle-protocol/src/lib.rs:321` — `Request::WorktreeDiff { session: SessionId }` carries no filter or truncation options.
- `crates/ertangle-daemon/src/git.rs:57-112` — `compute_worktree_diff(cwd: &Path)` runs raw `git diff HEAD` with no path filtering, auto-folding, or line cap (only untracked files are capped at 200 lines).
- `crates/ertangle-daemon/src/handlers/worktree.rs:55-79` — `handle_diff` forwards `compute_worktree_diff` directly.

## Analysis / decisions

- Decided: keep the TUI Diff view (`handle_diff` with default options when called by TUI) intact so human reviewers can still browse full files in the TUI file tree, while MCP `ertangle_diff` (and CLI `ertangle diff` flags) support scoped queries and safe defaults.
- Decided: introduce `DiffOptions` in `ertangle-protocol`:
  - `stat_only: bool` (default `false`): return changed file list with `+added -removed` line counts and status (`M`/`A`/`D`) without unified hunks.
  - `paths: Vec<String>` (default empty): limit diff to matching relative paths or prefixes.
  - `max_lines: Option<usize>` (default `Some(800)` for MCP `ertangle_diff`, `None` for TUI): truncate output cleanly at a line boundary and append a summary of omitted lines/files.
  - `fold_generated: bool` (default `true` when `paths` is empty, `false` when specific `paths` are requested): replace diff hunks of known giant lock/generated files (`package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `Cargo.lock`, `*.Designer.cs`, `*ModelSnapshot.cs`, `packages.lock.json`, `uv.lock`, `poetry.lock`, `go.sum`, `*.snap`, `*.min.js`, `*.min.css`) with a one-line summary (`<path> | +N -M (generated/lock file folded; pass paths=["<path>"] to expand)`).
- Not confirmed: whether TUI Diff view should also fold generated files by default or keep them collapsible in the file tree (T35 already provides per-file collapse in TUI, so TUI can pass `fold_generated: false, max_lines: None`).

## Implementation

1. `crates/ertangle-protocol/src/lib.rs`:
   - Add optional fields (`#[serde(default)]`) to `Request::WorktreeDiff`: `stat_only: bool`, `paths: Vec<String>`, `max_lines: Option<usize>`, `fold_generated: bool`.
2. `crates/ertangle-daemon/src/git.rs`:
   - Add `DiffOptions` struct and `compute_worktree_diff_with_options(cwd: &Path, opts: &DiffOptions) -> Result<String>`.
   - Implement `is_generated_or_lock_file(path: &str) -> bool` covering Rust, Node/Vite, .NET (`*.Designer.cs`, `*ModelSnapshot.cs`, `packages.lock.json`), Python, and Go lock/generated patterns.
   - Implement `stat_only` mode, `paths` filtering (for both tracked `git diff HEAD -- <paths>` and untracked files), per-file folding when `fold_generated` is true, and `max_lines` truncation with a trailing hint.
   - Keep `compute_worktree_diff(cwd)` as a convenience wrapper with default unrestricted options for TUI compatibility.
3. `crates/ertangle-daemon/src/mcp.rs`:
   - Expand `ertangle_diff` `inputSchema` with `stat_only` (`boolean`), `paths` (`array of string`), and `max_lines` (`integer`, default 800).
   - Enable `fold_generated: paths.is_empty()` by default in MCP `ertangle_diff`.
4. `src/main.rs` & `src/cmd/worktree.rs`:
   - Add `--stat`, `--max-lines`, and trailing `[PATHS]...` arguments to `ertangle diff`.

## Acceptance criteria

- `ertangle_diff` with `stat_only: true` returns a compact file-by-file `+N -M` summary without unified hunks.
- Modifying `package-lock.json`, `Cargo.lock`, `20261124_Init.Designer.cs`, or `AppDbContextModelSnapshot.cs` folds their hunks into a one-line summary in `ertangle_diff` when `paths` is omitted, and expands them when explicitly listed in `paths`.
- `max_lines` caps the returned diff and states how many lines were truncated.
- Existing TUI diff rendering and hunk discard tests continue to pass.
- Unit tests cover `is_generated_or_lock_file`, `stat_only`, `paths` filtering, `fold_generated`, and `max_lines`; `make pre-commit` passes.
