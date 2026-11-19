# T49 — Resolve the agent kind of a spawn from config and the parent instead of hard-coding opencode

Priority: P2　Scope: `termade-protocol`, `termade-daemon`, `termade` CLI　Depends on: —

Status: in-progress

## Problem

When a spawn request does not name an agent kind, the session always runs opencode. The `[defaults] agent` and `[roles.<role>] agent` settings are parsed and editable in the TUI settings page, but spawn never reads them. A user who sets the default agent to claude still gets opencode, and a claude coordinator that spawns a worker without naming a kind gets an opencode worker.

## Evidence

- CLI: `#[arg(long, default_value = "opencode", ...)]` on `spawn --kind` (`src/main.rs:90`).
- MCP: `args["kind"].as_str().unwrap_or("opencode")` (`crates/termade-daemon/src/mcp.rs:703`).
- `SpawnRequest.kind` is a required `AgentKind` (`crates/termade-protocol/src/lib.rs:40`), so the daemon cannot tell "not specified" from "opencode".
- `Config::resolve` takes the kind as input and only uses the role to pick model, effort, and permission (`crates/termade-daemon/src/config.rs:781`).
- `Defaults.agent` and `RoleCfg.agent` exist (`config.rs:17`, `config.rs:41`) and the TUI settings page writes `defaults.agent` (`crates/termade-tui/src/settings.rs:415`).

## Analysis

Resolution order, first match wins:

1. Kind named in the request (`--kind`, MCP `kind`).
2. `[roles.<role>] agent`: the most specific intent ("workers always use codex").
3. The parent's kind, only for sessions created by an agent (`parent` is set). A worker then runs the same CLI as its coordinator, which is known to be installed and logged in.
4. `[defaults] agent`: a global preference, mainly for sessions the user creates directly.
5. `opencode`.

Layer 3 sits above layer 4 because inside one collaboration the coordinator's agent is a better signal than a global preference.

The model is not inherited. It keeps the existing order (request → `roles.<role>.model` → `agents.<kind>.model`). An Opus coordinator therefore does not make every worker run Opus.

`roles.<role>.model` can belong to a different agent kind than the resolved one (for example `model = "haiku"` while the kind is inherited as codex). There is no catalog that maps every model to a kind, so the rule is: the role's model applies only when the role does not pin an agent or pins the same agent as the resolved kind. Otherwise `agents.<kind>.model` is used.

The quota fallback (`check_spawn_quota`) runs after kind resolution and works on the resolved kind, as it does today.

Not confirmed: whether any third-party client sends a spawn request without `kind`. Old clients always send it, so making the field optional stays compatible.

## Implementation

- `SpawnRequest.kind` becomes `Option<AgentKind>` with `#[serde(default)]`.
- CLI `spawn --kind` drops its default value; the completions test follows.
- MCP `termade_spawn` sends `None` when `kind` is omitted and documents the resolution order.
- Daemon: add `Config::resolve_kind(requested, role, parent_kind)` and call it at the top of `handle_spawn`; apply the role-model compatibility rule inside `Config::resolve`.
- TUI always picks an agent before spawning; behavior is unchanged.

## Acceptance criteria

- Unit tests cover each of the five layers and the incompatible role model case.
- `termade spawn` without `--kind` honors `[defaults] agent`.
- An agent-created session without `kind` runs the parent's agent kind.
- `make pre-commit` passes.
