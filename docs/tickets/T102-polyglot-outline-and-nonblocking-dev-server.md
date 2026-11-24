# T102 — `ertangle outline` (polyglot code skeleton for Rust, TS/JS, C#, Python) and `ertangle dev` (non-blocking dev server manager)

Status: done

Priority: P2　Scope: `ertangle-daemon`, `ertangle`　Depends on: T32, T47, T100

## Problem

1. **High token cost of code navigation (`outline`)**:
    collaboration instructions tell coordinators to give workers self-contained prompts with *exact files and line ranges* (`collab.rs:84`). Currently, to find symbol line ranges in a 1,000-line file (`.rs`, `.ts`/`.tsx`, `.cs`, `.py`), an agent must either read the entire file in chunks (consuming 5,000–15,000 tokens) or run multiple `rg` commands that only show start lines without end lines or structural hierarchy.
2. **Dev server port conflicts and blocking turns (`dev`)**:
   When working on Frontend (`vite`, `next dev`, `npm run dev`), .NET Core (`dotnet run` with Kestrel), or Rust web services in parallel worktrees:
   - Running a dev server in the foreground blocks the agent's tool call indefinitely until timeout.
   - Multiple worktrees reading the same `vite.config.ts` or `Properties/launchSettings.json` collide on `:5173`, `:3000`, or `:5000`.

## Evidence

- `crates/ertangle-daemon/src/collab.rs:84` — `"Give self-contained prompts with exact files/line ranges so workers do not re-explore."`
- `crates/ertangle-daemon/src/bg_proc.rs` (T32) — daemon already tracks background child processes (`BgProc`) per session and supports `Request::KillBgProc` (`ertangle ps` / `ertangle bg-kill`).
- `src/main.rs` — no `outline` or `dev` subcommands exist.

## Analysis / decisions

- Decided: implement both as zero-MCP-overhead `ertangle` CLI subcommands (`ertangle outline` and `ertangle dev`).
- Decided for **`ertangle outline <path>...`**:
  - Fast, zero-heavy-AST-dependency line scanner (brace/indent-aware state machine ignoring comments and string literals) supporting:
    - **Rust (`.rs`)**: `mod`, `pub struct`, `enum`, `trait`, `type`, `impl`, `fn` with exact `L<start>-L<end>` ranges.
    - **TypeScript / JavaScript / Vue (`.ts`, `.tsx`, `.js`, `.jsx`, `.vue`)**: `import` summary count, `export interface`, `export type`, `export class`, `export function`, `export const` / top-level components & hooks with `L<start>-L<end>`.
    - **C# / .NET Core (`.cs`)**: `namespace`, `[Route]` / `[HttpGet|Post|Put|Delete]` attributes, `class`, `interface`, `record`, `struct`, `enum`, and method/constructor signatures with `L<start>-L<end>`.
    - **Python (`.py`)**: `class`, `def` / `async def` with `L<start>-L<end>`.
  - If `<path>` is a directory, list a compact file tree with symbol counts and top exported types per file.
- Decided for **`ertangle dev`**:
  - Subcommands: `ertangle dev start [--port <port>] [--timeout-secs <n>] [-- <cmd>...]`, `ertangle dev status`, `ertangle dev stop`.
  - Auto-detects default dev command if `-- <cmd>` is omitted:
    - Node/Vite (`package.json` with `scripts.dev`): `<pm> run dev -- --port <port>` and sets `PORT=<port>`, `VITE_PORT=<port>`.
    - .NET Core (`*.csproj` with `Microsoft.NET.Sdk.Web`): `dotnet run --no-launch-profile` with `ASPNETCORE_URLS=http://127.0.0.1:<port>`.
    - Rust (`Cargo.toml`): `cargo run` with `PORT=<port>`.
  - Automatically picks a free local TCP port (`TcpListener::bind("127.0.0.1:0")`) when `--port` is omitted.
  - Spawns the server in a new process group (`setsid` / process group) inside the session's `cwd`, streams output to `.ertangle/dev-<port>.log`, polls until the port accepts TCP connections (or stdout prints a listening URL) within `timeout_secs` (default 25s), and **returns immediately** with `{ "ok": true, "pid": <pid>, "port": <port>, "url": "http://127.0.0.1:<port>", "log": ".ertangle/dev-<port>.log" }`.
  - Because it runs as a child of the session (or via daemon), `ertangle ps` and `ertangle bg-kill` automatically observe and clean it up when the session ends.

## Implementation

1. Create `src/cmd/outline.rs`:
   - Implement brace/indentation-aware symbol and line-range extraction for `.rs`, `.ts`/`.tsx`/`.js`/`.jsx`/`.vue`, `.cs`, and `.py`.
   - Support single-file detailed outline (`L12-L48: pub fn ...`) and directory summary mode.
2. Create `src/cmd/dev.rs`:
   - Implement free TCP port allocation, stack dev-command detection (Vite/Node, ASP.NET Core `ASPNETCORE_URLS`, Cargo), background daemonized spawn with `.ertangle/dev-<port>.log`, readiness probe (TCP connect + early-exit check), and `stop`/`status` via `.ertangle/dev.json` PID tracking.
3. `src/main.rs` & `crates/ertangle-daemon/src/collab.rs`:
   - Register `Cmd::Outline` and `Cmd::Dev`.
   - Reference `ertangle outline <file>` in `TOKEN_HINTS`.

## Acceptance criteria

- `ertangle outline` accurately reports `L<start>-L<end>` and signatures for Rust, TypeScript/TSX, C# (including `[Http*]` / `[Route]` attributes), and Python files.
- `ertangle dev start` allocates a free port, injects `PORT`/`VITE_PORT`/`ASPNETCORE_URLS`, waits for the server to listen (or fails fast if the process exits with an error), and returns JSON/text with PID, URL, and log file path without blocking the caller.
- `ertangle dev stop` terminates the server process group cleanly.
- Unit tests cover symbol outline extraction across all four languages and `ertangle dev` lifecycle (start, readiness detection, early crash detection, stop); `make pre-commit` passes.
