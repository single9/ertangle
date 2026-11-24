# T101 — `ertangle check`: polyglot low-token build, lint, and test verifier (Rust, Node/Vite, .NET Core, Python)

Status: done

Priority: P2　Scope: `ertangle-daemon`, `ertangle`　Depends on: T47, T100

## Problem

Running raw build and test commands (`cargo test`, `cargo clippy`, `npm test`, `vitest`, `vite build`, `tsc --noEmit`, `dotnet build`, `dotnet test`, `pytest`) inside an agent turn has two severe failure modes:

1. **Interactive watch-mode hangs**: Frontend test runners (`vitest`, `jest`, `npm test` in Create React App) default to interactive watch mode when `CI=1` or `--run` is omitted, blocking the agent turn until timeout.
2. **Massive noisy tool outputs**:
   - **Rust**: hundreds of `Compiling...` and `test ... ok` lines.
   - **Frontend (Vite / TypeScript / Vitest)**: ANSI escape codes, spinner frames, bundle chunk tables, and 100-line stack traces pointing inside `node_modules/`.
   - **.NET Core (MSBuild / xUnit)**: duplicate `CSxxxx` errors repeated across every dependent `.csproj` in a `.sln`, verbose absolute file paths, restore banners, and internal `System.Runtime.CompilerServices.TaskAwaiter` stack frames.

Per T47, large tool results (>10k chars) account for 30% of all tool output characters and are re-read on every subsequent turn.

## Evidence

- `docs/tickets/T47-agent-token-savings.md:47-51` — 15 large tool outputs accounted for 30% of total characters.
- `crates/ertangle-daemon/src/collab.rs:34-39` — `TOKEN_HINTS` advises agents to pipe to `head -n 50`, which frequently truncates the actual error summary at the bottom of test/build output.
- `src/main.rs` — no built-in `ertangle check` subcommand exists.

## Analysis / decisions

- Decided: implement `ertangle check` as a fast native Rust CLI subcommand (`ertangle check [--cwd <dir>] [--stack <auto|rust|node|dotnet|python>] [--quick] [--json]`) so agents can invoke it via shell with **zero permanent MCP schema token overhead** (and reference it in `TOKEN_HINTS`).
- Decided: auto-detect project stack(s) in the target directory and apply stack-specific non-interactive flags and output dehydrators:
  1. **Rust (`Cargo.toml`)**:
     - Run `cargo clippy --all-targets --message-format=json -- -D warnings` (and `cargo test --no-fail-fast -- --format=terse` unless `--quick`).
     - Parse JSON diagnostics to emit only `file:line:col: [level] code: message` (deduplicated) and failed test panic summaries.
  2. **Frontend / Node (`package.json`)**:
     - Detect package manager from lockfile (`pnpm-lock.yaml` -> `pnpm`, `yarn.lock` -> `yarn`, `bun.lock`/`bun.lockb` -> `bun`, else `npm`).
     - Always run with `CI=1`, `NO_COLOR=1`, `FORCE_COLOR=0`.
     - Run typecheck (`tsc --noEmit --pretty false` if `tsconfig.json` exists), test (`vitest run` or `jest --watchAll=false` if configured, unless `--quick`), or `vite build` if requested.
     - Strip ANSI codes, collapse `node_modules/` stack trace frames, and cap output to actionable `src/` errors.
  3. **.NET Core (`*.sln`, `*.slnx`, `*.csproj`)**:
     - Run `dotnet build --nologo -v quiet -clp:ErrorsOnly;NoSummary` (or `dotnet test --nologo --logger "console;verbosity=minimal"` unless `--quick`) with `DOTNET_NOLOGO=1` and `DOTNET_CLI_TELEMETRY_OPTOUT=1`.
     - Parse MSBuild diagnostic lines: strip trailing `[.../Project.csproj]`, convert absolute paths to workspace-relative paths, deduplicate identical `(file, line, col, code)` diagnostics across multi-project solutions, and filter internal async runtime frames from xUnit/NUnit stack traces.
  4. **Python (`pyproject.toml` / `pytest.ini`)**:
     - Run `pytest -q --tb=short --no-header` (and `ruff check --output-format=concise` if available).
- Decided: when all checks pass, print a single-line summary:
  `PASS [rust]: clippy (0 warnings), test (142 passed, 0 failed) in 1.8s`
  When checks fail, print a structured, deduplicated list of errors (capped at e.g. 50 lines with an omission counter) and exit with a non-zero status code.

## Implementation

1. Create `src/cmd/check.rs` (or a reusable module in `crates/ertangle-core` / `crates/ertangle-daemon` exposed via `src/cmd/check.rs`):
   - Implement stack detection (`detect_stacks(cwd: &Path) -> Vec<ProjectStack>`).
   - Implement ANSI stripping, relative path normalization, MSBuild diagnostic deduplication, `node_modules` stack-frame collapsing, and Cargo JSON message parsing.
   - Implement runner for `Rust`, `Node`, `DotNet`, and `Python` with a configurable line cap (default 60 lines).
2. `src/main.rs`:
   - Add `Cmd::Check { cwd: Option<PathBuf>, stack: Option<String>, quick: bool, max_lines: usize, json: bool }`.
3. `crates/ertangle-daemon/src/collab.rs`:
   - Mention `ertangle check` (and `ertangle check --quick`) in `TOKEN_HINTS` as the preferred low-token verification command.

## Acceptance criteria

- `ertangle check` detects Rust, Node/Vite/TS, .NET Core, and Python projects from marker files.
- MSBuild output parser deduplicates repeated `CSxxxx` errors across projects and normalizes absolute paths to relative paths.
- Frontend output parser strips ANSI escape sequences, enforces `CI=1`, and filters out `node_modules/` stack frames.
- Cargo JSON parser extracts compiler/clippy errors into single-line `file:line:col: error: msg` entries and summarizes passing runs in one line.
- Unit tests cover output parsers/dehydrators for Rust JSON, TypeScript/Vitest logs, and .NET MSBuild/xUnit logs; `make pre-commit` passes.
