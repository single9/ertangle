# termade — Terminal-Native Multi-Agent Development Environment

[English](README_EN.md) | [繁體中文](README.md)

> **Run multiple coding agents concurrently on the same git repository, communicating autonomously and collaborating safely inside your terminal.**

`termade` is a terminal-native orchestrator and development environment for modern coding agents (such as **OpenCode**, **Claude Code**, **Codex**, and **Antigravity**). Each agent operates in an isolated Git Worktree, communicates with peers across an MCP message bus and task board, and allows developers to inspect changes, conduct line-by-line code reviews, and merge branches safely through a keyboard-driven TUI.

---

## Why termade? (Terminal-Native vs. Heavy GUI IDEs)

GUI-based AI IDEs such as [Orca](https://github.com/stablyai/orca) integrate worktrees, embedded browsers, diff views, and terminals into a single desktop app. `termade` focuses solely on the **collaboration, scheduling, and review core**, leaving editing and execution to the terminal and each agent's native CLI:

| Differentiator | How termade does it |
| --- | --- |
| **Ultra-Lightweight & Fast** | Written in Rust + Ratatui; resident daemon memory footprint `< 20 MB`; instant launch; flawless performance over remote SSH |
| **Resident Daemon Architecture** | The daemon manages agent child processes, message bus, task boards, and worktree states; the TUI is a detachable, re-attachable interface |
| **Zero Conflicts via Isolation** | Each agent session automatically operates in its own `git worktree` and isolated branch (`termade/<session>`) |
| **Native Protocols with Zero Wrapper Overhead** | Prioritizes official structured protocols (ACP / stream-json / app-server); gracefully falls back to PTY pass-through if unavailable |
| **Structured Agent Collaboration** | Automatically injects an MCP server into agents, enabling them to message peers, delegate tasks, broadcast status, and wait for events |
| **Keyboard & Script Driven** | Full TUI keybindings, Helix-style which-key menu, `:` command-line, and a comprehensive, feature-complete `termade` CLI |

---

## Supported Agents & Protocols

Each agent communicates via its **official, native structured interface without external dependencies**:

| Agent | Native Protocol | Default Launch Command | Transport | Status & Capabilities |
| --- | --- | --- | --- | --- |
| **OpenCode** | ACP | `opencode acp` | `acp` | ✅ Full bidirectional structured mode (streaming, tool calls, permission forwarding, MCP injection) |
| **Antigravity** | ACP | `agy acp` | `acp` | ✅ Full bidirectional structured mode (ACP dialect compatible, MCP injection) |
| **Claude Code** | stream-json | `claude -p --input-format stream-json --output-format stream-json --verbose` | `stream-json` | ✅ Token streaming, tool calls, cost statistics, 5h/7d quota tracking, MCP injection |
| **Codex** | app-server | `codex app-server` | `app-server` | ⚠️ Handshake & thread verified; sending turns requires `codex login` on the local machine |
| **Universal Fallback** | PTY | Interactive agent CLI | `pty` | ✅ Automatically falls back to PTY pass-through when native protocols are missing or uninstalled |

> [!NOTE]
> - **Codex Authorization**: `codex app-server` protocol is fully implemented, but running turns requires a valid OpenAI login. If unauthorized, the session transitions to `failed` and prompts the user to run `codex login`.
> - **PTY Limitation**: In PTY mode, full-screen TUI outputs are stripped of ANSI escape sequences on a best-effort basis. Native structured protocols are strongly recommended.
> - Detailed verification logs and tested agent versions are available in [`docs/VERIFICATION.md`](docs/VERIFICATION.md); architecture design specs are in [`docs/DESIGN.md`](docs/DESIGN.md).

---

## Quick Start

### 1. Installation

**Option 1: Download a prebuilt binary** (from [Releases](https://github.com/single9/termade/releases/latest); Linux x86_64 / arm64 and macOS arm64 / x86_64 / universal)

```bash
# Linux x86_64 shown; swap the asset name for other platforms (termade-linux-arm64, termade-macos-arm64, ...)
curl -fsSL https://github.com/single9/termade/releases/latest/download/termade-linux-x86_64.tar.gz | tar -xz
install -m 755 termade-linux-x86_64 ~/.local/bin/termade
```

**Option 2: Install directly from git** (requires a Rust toolchain)

```bash
cargo install --git https://github.com/single9/termade

# Pin a specific version
cargo install --git https://github.com/single9/termade --tag v0.3.0
```

**Option 3: Build from source**

```bash
git clone https://github.com/single9/termade && cd termade
cargo install --path .        # installs to ~/.cargo/bin
# or use the Makefile to install to ~/.local/bin/termade
make install
```

**Shell completions** (bash / zsh / fish, with dynamic session label completion)

```bash
make install-completions      # run from the source checkout; detects your SHELL
# or generate manually: termade completions <bash|zsh|fish>
```

### 2. Launching Your First Agent Session

Inside any Git repository directory:

```bash
termade
```

- If the daemon is not already running, `termade` **automatically launches the daemon in the background** and starts the TUI.
- In the TUI, press **`n`** to spawn a new agent. Select the agent (e.g., `opencode`), choose a model, and type your prompt:
  ```
  Fix all failing unit tests and resolve any compiler errors
  ```
- The agent starts working inside its dedicated worktree (`termade/session-xxx`). Watch live Markdown streaming in the Overview tab, switch to the Diff view to inspect modifications, and merge safely with one keypress.

---

## Core Features & Workflow

### 1. Worktree Isolation & Review Loop

- **Dedicated Worktree & Branch**: Each session operates inside an independent `git worktree` and `termade/<session>` branch, eliminating file conflicts.
- **Real-Time Color Diff**: Press `Tab` in the TUI to switch to the Diff view; navigate hunks with `{` / `}`.
- **Inline Review Comments**: Press `c` on any diff line to submit inline feedback (e.g., "Add boundary checks here"). The comment is routed back to the agent for automated revision.
- **Safe Merge & Conflict Resolution**: Press `M` or run `termade merge`. The system runs pre-flight safety checks (supports global `pre_merge_command` gate verification), commits uncommitted worktree changes automatically, and merges into the main branch. On conflicts, use `:merge --resolve` (or Which-key `Space g r`) to keep conflict markers isolated in the worktree while keeping `main` clean, delegating conflict resolution to the agent.
- **Garbage Collection (GC)**: Press `G` or run `termade gc` to clean up merged and completed worktrees safely without deleting unmerged work.

### 2. Multi-Agent Bus & Task Board (MCP)

Every structured agent session is automatically injected with a stdio MCP server containing **14 collaboration tools**:

| Category | Tool Name | Description |
| --- | --- | --- |
| **Self & Peers** | `termade_whoami`, `termade_peers` | Inspect own session ID, branch, worktree path, and peer agent details |
| **Messaging** | `termade_send`, `termade_inbox`, `termade_wait`, `termade_ask` | Send direct/broadcast messages, non-blocking inbox reads, blocking wait, synchronous Q&A |
| **Task Board** | `termade_task_list`, `termade_task_create`, `termade_task_update` | Create tasks, assign owners, specify `depends_on` prerequisite dependencies |
| **Spawning & Output** | `termade_spawn`, `termade_artifact_share`, `termade_status` | Spawn sub-agents, share artifacts (files, PR links), broadcast status summaries |
| **Git Operations** | `termade_diff`, `termade_merge` | Query branch diff vs. main branch, request worktree merge |

- **Unread Nudge on Idle**:
  - Direct messages and broadcasts sent to busy agents are buffered in their mailbox **without interrupting active turns**.
  - Once the recipient finishes its turn and returns to `idle` (and pending prompt queues are drained), buffered unread messages are consolidated into a single gentle reminder. Each message is nudged **only once**.
  - Messages already retrieved via `termade_wait` / `termade_ask` or sessions exceeding budget limits are not nudged.
- **Task Dependencies**:
  - Tasks created with `--depends-on <id>` remain `blocked` until all prerequisites are marked `done`.
  - Once unlocked, the task transitions to `todo` and notifies the assigned agent automatically.
- **Mailbox Fallback**: All messages are simultaneously written to `<repo>/.termade/mailbox/<session>.jsonl`.

### 3. Quota Tracking, Intelligent Handoff & Session Resume

- **Quota Monitoring**:
  - Parses real-time quota data reported by agents (Claude 5-hour / 7-day rate limit windows and reset countdowns; Codex rate limits).
  - Title bar displays the tightest window (`≥ 70%` yellow, `≥ 90%` red). Run `:quota` for detailed status.
- **Intelligent Handoff**:
  - When an agent runs out of quota or rate-limited, or when requested manually (`:handoff <agent> [note]`):
  - The previous agent stops, and uncommitted worktree changes are committed automatically;
  - The daemon synthesizes a comprehensive briefing (goals, tasks, last output, full git diff);
  - The successor agent **takes over the exact same worktree and branch**;
  - Messages sent to the old session ID/label are **automatically forwarded to the successor**.
- **Session Resume**:
  - If an agent process crashes or the daemon restarts, press `Space s r` or run `termade resume <session> [prompt]` to resume the **exact same conversation**.
  - Leverages native resume mechanisms (Claude `--resume`, Codex `thread/resume`, ACP `session/load`), preserving session ID, transcript, and branch.

### 4. Resource Control, Budgeting & Idle Sleep

- **Cost & Token Limits**:
  - Configure per-session USD limits (`session_usd`), parent-child tree budget (`tree_usd`), and token caps (`session_tokens`), as well as maximum sub-agent depth.
  - When reaching threshold (default 80%), visual warnings trigger. Exceeding limits halts execution and holds pending prompts until budget is increased via `:budget <amount>`.
- **Idle Process Suspension (Sleep/Wake)**:
  - Press `z` or run `termade sleep <session>` to send `SIGSTOP`, freezing idle agent processes and freeing CPU. Press `z` or send a message to dispatch `SIGCONT` and wake them up.

### 5. Permission Policy & Rule Engine

- Cross-agent unified flag `--permission <ask|auto|bypass>`:
  - `ask`: Tool calls require manual approval in the TUI (`a` allow once / `A` always allow / `d` deny).
  - `auto` (default): Safe operations are approved automatically.
  - `bypass`: Skips permission confirmation.
- **Policy Engine**: Configured via `~/.config/termade/policy.toml` and `.termade/policy.toml` using `deny` (blacklist) → `allow` (whitelist) → session policy order.

### 6. Remote Connections, Tailscale Integration & Auto-Reconnect (T28)

- **Automatic Tailscale Detection**:
  - Start daemon with `termade daemon --listen tailscale` to auto-resolve your machine's peer-to-peer Tailscale IPv4 address via `tailscale ip -4`.
  - Automatically generates and securely stores auth tokens at `~/.local/state/termade/remote.token` (file permission `0600`).
- **Connection Info Quick Look**:
  - Run `termade daemon info` to print local and Tailscale addresses, token path, and copy-paste connection commands.
- **Resilient Auto-Reconnect**:
  - When laptops wake from sleep, networks switch, or brief disconnects happen, the TUI automatically reconnects in the background with exponential backoff.
  - Reconnects using `Request::Hello { since, epoch, token }` to seamlessly resume state and backfill missed events without interruption.

### 7. Multi-Host Aggregation in TUI (T29)

- **Unified Multi-Machine Dashboard**:
  - Configure `[remotes]` in `config.toml` or launch with `termade --remote workstation:7420 --remote gpu-box:7420`.
  - View, monitor, and interact with agent sessions running across multiple machines from a single unified TUI dashboard.
- **Transparent Routing & Filtering**:
  - Sessions are tagged with colored host badges (`[workstation]`, `[gpu-box]`).
  - Sending prompts, reviewing diffs, and approving tool executions are automatically routed to the owning host.
  - Switch host views anytime using `:host <target>` or `:host all`.

### 8. Web Interface & Mobile PWA (T30)

- **Mobile Review On The Go**:
  - Step away from your desk without losing review flow. Enable it directly from the TUI settings panel (`:settings` or `Space ,`) or by typing `:web on [addr]` (or via CLI `termade daemon --web tailscale`).
  - A clean **Unicode QR Code** can be displayed in your terminal with `:web qr` or `Space w` for instant camera scanning on mobile phones.
- **Zero-Dependency Native HTTP / WebSocket**:
  - Built directly into the single binary with pure Rust standard networking—no Node.js, npm, or heavy async dependencies required.
- **Thumb-Friendly Native Mobile PWA**:
  - **Approval Banner**: Pops up immediately when agents request tool execution permissions, offering one-tap Allow / Always / Deny.
  - **Conversational Turns**: Restructures raw transcripts into clean turns, grouping messages, thought folds (`💭`), and tool call results.
  - **Full Markdown & Code Copy**: Clean typography for headers, quotes, lists, and formatted code blocks with a one-tap copy button.
  - **Text & Image File Previews**: Tap interactive file pills to preview text/code files (with line jumping and highlights) or view images directly.
  - **Git Diff Review & One-Tap Merge**: Review branch diffs and merge back to main directly from your phone.

---

## TUI User Guide

### 1. Layouts and Views

- **Layout Modes**: Press `L` to cycle through "Two-pane" (Sessions + Center), "Tabs" (Top Session Tabs + Center), and "Three-pane" (Sessions + Center + Bus monitor).
- **Center Views**: Press `Tab` to cycle across the 4 main views:
  1. **Overview**: Real-time streaming transcript with Markdown rendering (headers, lists, tables, code blocks).
  2. **Diff**: Real-time colored Git Diff view with inline review support.
  3. **Worktrees**: List of all managed Git Worktree paths and disk states.
  4. **Tasks**: Collaborative task board grouped by status (doing, review, todo, blocked, done) with dependency markers (`⧗`). Press `Enter` to jump to the assignee's session.

### 2. Keybindings Cheat Sheet

#### Navigation, Menu & Core Actions

| Key | Action |
| --- | --- |
| `Space` | Open **Which-key menu** (`s` session, `g` git, `v` view, `m` message, `?` help) |
| `:` or `/` | Open **Command-line** (supports `Tab` completion) |
| `n` | Create new session (select Agent, Model, input Prompt; supports `Ctrl-R` for Role, `Ctrl-W` for Worktree, `Ctrl-F` for Effort) |
| `i` / `Enter` | Focus input box to send prompt (supports `@peer` and `#channel` routing) |
| `j` / `k` or `↑` / `↓` | Select previous/next session; move line cursor in Diff; select task in Tasks view |
| `[` / `]` | Jump to previous / next session |
| `Ctrl-K` | **Jump-Anywhere**: Fuzzy search and jump to any session |
| `Space ,` (or `:settings`) | Open **Settings panel** (toggle default Agent, Model, Effort, Worktree, Web version support, etc.) |
| `Space w` (or `:web qr`) | Display **Web version URL & QR Code** popup (scan and connect instantly) |
| `q` / `Q` | `q` exit TUI (daemon and agents keep running); `Q` stop daemon (same as `:shutdown`) |

#### Transcript, Search & Clipboard

| Key | Action |
| --- | --- |
| `t` | Toggle transcript mode: Markdown rendering ↔ Raw text |
| `T` (or `:thoughts`) | Toggle agent thought lines (`💭`, hidden by default for speed) |
| `O` (or `:tools`) | Fold / unfold tool calls (consecutive successful calls are folded by default) |
| `f` (or `:find <text>`) | **Incremental search** in transcript or diff (`Enter`/`↓` next, `↑` previous, `Esc` exit) |
| Mouse Drag | Select text in center pane; release to copy via **OSC 52** |
| `Y` (or `/copy`) | Copy current center pane content (excluding borders and sidebars) |
| `F2` | Toggle mouse capture mode (disable to use native terminal selection, or use `Shift`+Drag) |
| `PgUp` / `PgDn` | Scroll transcript up / down (pauses auto-scroll) |
| `Home` / `End` | Jump to top / jump to bottom and resume auto-following (`▼ follow`) |
| `P` | Toggle project scope: Current project only (`here`) ↔ All projects (`all`) |

#### Prompt Input & Queuing

| Key / Syntax | Description |
| --- | --- |
| `@filename` | **Fuzzy file attachment**: Type `@` to fuzzy search repo files; auto-attaches text (up to 256 KB) or images (up to 5 MB) |
| `Ctrl-V` | Paste image from system clipboard (requires `wl-paste` or `xclip` on Linux) |
| Drag & Drop | Drag files from file manager into terminal to auto-insert `@path` |
| `Ctrl-J` / `Shift-Enter` | Newline in prompt input (expands up to 8 lines) |
| `Alt-Enter` | **Queue prompt**: If session is busy, queues in daemon to send automatically when idle |
| `↑` / `↓` | Cycle command and prompt input history (persisted across restarts) |

#### Spawn Mode Shortcuts (Creating Sessions)

| Key | Description |
| --- | --- |
| `Ctrl-R` | Cycle role: `worker` ↔ `coordinator` ↔ `reviewer` (customizes injected prompt per role) |
| `Ctrl-W` | Toggle dedicated Git Worktree branch |
| `Ctrl-E` | Cycle permission policy: default (config) ↔ `ask` ↔ `auto` ↔ `bypass` |
| `Ctrl-F` | Cycle reasoning effort: default ↔ `low` ↔ `medium` ↔ `high` ↔ `max` |

#### Review, Git & Session Lifecycle

| Key | Action |
| --- | --- |
| `c` | (Diff view) Leave review comment on cursor line and route back to agent |
| `M` | Safely merge selected session's worktree branch into main (`:merge --dry-run` to preview, `:merge --resolve` to delegate conflicts) |
| `G` | Clean up (GC) completed and merged worktrees |
| `z` | Toggle session pause (`SIGSTOP`) and wake (`SIGCONT`) |
| `x` | Interrupt current agent turn (Cancel) |
| `K` | Force kill selected agent child process (Kill) |
| `a` / `A` / `d` | Permission request: Allow once / Always allow for this turn / Deny |

### 3. TUI Command-Line (`:command`)

Press `:` to open the command line with `Tab` completion:

- `:spawn [--role <role>] <agent>[:model] <prompt>` — Spawn a new agent session (e.g., `:spawn --role coordinator claude:opus "plan architecture"`)
- `:handoff <agent> [note]` — Hand over current session to another agent
- `:resume [prompt]` — Reconnect to an exited session and continue dialogue
- `:queue` / `:unqueue` / `:queue clear` — View, pop, or clear prompt queue
- `:rm` / `:clean` — Remove an exited session / purge all finished sessions
- `:budget <amount>` — View or increase session USD budget cap
- `:quota` — Display rate limit windows and reset countdowns for all agent accounts
- `:task <title>` / `:tasks` — Create task / switch to Tasks board view
- `:task-status <todo|doing|blocked|review|done>` — Update status of selected task
- `:host <target|all>` — Filter sessions by host in multi-host mode (T29)
- `:remote` — Display remote connection details, Tailscale IP, and Web version URL
- `:notify on|off|test` — Toggle desktop notifications or send a test alert
- `:shutdown` — Shut down daemon and all running agents (requires `:shutdown!` to confirm)

---

## CLI Reference

`termade` provides a comprehensive CLI mirroring TUI capabilities:

```bash
# Session Management
termade sessions [--json]                         # List all sessions (alias: termade peers)
termade whoami [--session <id>] [--json]          # Show current session metadata
termade spawn --kind <agent> [--label <label>] [--worktree] [--prompt <prompt>]
termade resume <session-or-label> ["prompt"]      # Resume an exited session
termade handoff <session-or-label> --to <agent> [--note "Handoff notes"]
termade sleep <session> ; termade wake <session>  # Suspend / wake agent processes
termade cancel <session> ; termade kill <session> # Interrupt turn / kill process
termade rm <session> [--force]                    # Remove exited session record

# Prompts & Queue
termade prompt [--queue] <session-or-label> "prompt text"
termade queue <session-or-label> [--clear]        # View or clear prompt queue

# Worktree & Code Review
termade worktrees                                 # List all managed git worktrees
termade diff <session-or-label>                   # View git diff for a session
termade review <session-or-label> --location <file>:<line> "Review comment"
termade merge <session-or-label> [--dry-run] [--resolve] # Merge session branch back to main
termade pr <session-or-label> --title "PR title"  # Create GitHub Pull Request via gh
termade gc [--session <id>]                       # Clean up merged worktrees safely

# Cross-Agent Messaging & Tasks
termade send <session-or-label> "message content" # Send message to target agent
termade inbox [--session <id>]                    # Read session inbox
termade tasks                                     # List tasks and dependency states
termade task "Task title" [--assignee <label>] [--depends-on <id>]
termade task-update <id> --status <status>        # Update task status (todo/doing/blocked/review/done)

# Cost, Quota & Event Replay
termade quota [--json]                            # Query account quotas and reset times
termade cost [--json]                             # Breakdown token and USD spend
termade budget <session-or-label> <amount>        # Set or raise budget cap
termade replay [--since <seq>] [--json]           # Audit and replay events.jsonl

# Remote Daemon, Multi-Host & Web Version Support
termade daemon --listen tailscale                 # Start daemon bound to Tailscale WireGuard IP
termade daemon --listen 127.0.0.1:7420            # Start daemon listening on TCP (auto-generates token)
termade daemon --web tailscale                    # Start Web version and print terminal QR code (default port 7421)
termade daemon --web tailscale --web-port 8080    # Start Web version with custom port (8080)
termade daemon info                               # Print current Tailscale IP, token, and connection examples
termade --remote tailscale                        # Connect to local Tailscale daemon
termade --remote workstation:7420 --remote gpu:7420 # Aggregate multiple remote hosts into a single TUI
termade completions <bash|zsh|fish>               # Generate shell completion script
```

---

## Configuration & Environment Variables

### Configuration File: `config.toml`

`termade` merges global settings (`~/.config/termade/config.toml`) with project-level overrides (`<repo>/.termade/config.toml`):

```toml
[tui]
layout = "two"                    # Default layout: "two" | "three" | "tabs"
notify = ["bell", "command"]      # Notification methods: "bell" | "osc9" | "command"
notify_command = "notify-send termade \"$TERMADE_NOTIFY_TEXT\""
notify_min_turn_secs = 10         # Turns shorter than this threshold do not notify

[web]
# Web version settings (global only; can also be toggled in TUI :settings)
enabled = true                    # Enable Web version
port = 7421                       # Custom port: default 7421 (can also be specified in addr, e.g. "127.0.0.1:8080")
addr = "tailscale"                # Listen address: "tailscale" (auto-detect IP) | "127.0.0.1" | "0.0.0.0"

[remotes]
# Multi-host aggregation (T29)
workstation = { addr = "100.64.0.5:7420", token_file = "~/.config/termade/workstation.token" }
gpu-box = { addr = "100.64.0.8:7420" }

[limits]
session_usd = 5.0                 # Max USD budget per session
tree_usd = 15.0                   # Max USD budget for parent-child tree
warn_percent = 80                 # Warning threshold percentage
max_children = 5                  # Max concurrent child sessions per parent
max_depth = 3                     # Max sub-agent spawn hierarchy depth

[handoff]
warn_percent = 90                 # Warn with :handoff suggestion at 90% quota
auto = false                      # Automatically hand off on quota exhaustion (default false)
fallback = ["codex", "claude:sonnet"]

[collab]
instructions = true               # Inject termade collaboration rules into agent system prompt
extra = "Follow guidelines in CONTRIBUTING.md closely"

[git]
# Git & worktree settings (read only from global config for security)
pre_merge_command = "cargo test"       # Pre-merge validation command; non-zero exits abort merge
```

> [!TIP]
> If `<repo>/.termade/instructions.md` exists (up to 8 KB), its content is appended to the agent's system prompt during initialization.

### Environment Variables

| Variable | Default Value | Description |
| --- | --- | --- |
| `XDG_RUNTIME_DIR` | `/tmp/termade-$USER/` | Directory storing daemon Unix domain socket |
| `XDG_STATE_HOME` | `~/.local/state/` | Path for persistent events log (`$XDG_STATE_HOME/termade/events.jsonl`) |
| `TERMADE_LOG` | `warn` | Tracing filter level (e.g., `TERMADE_LOG=debug`) |
| `TERMADE_TOKEN` | — | Authentication token for remote daemon connections (use SSH tunneling for security) |
| `TERMADE_REMOTE` | — | Default remote daemon address (equivalent to `--remote`) |

---

## Development & Pre-commit Verification

To ensure local changes pass CI reliably, install the pre-commit hook or run one-command verification:

```bash
make setup-hooks   # Automatically installs .git/hooks/pre-commit
make pre-commit    # Auto-formats (fmt), runs clippy, and checks i18n catalogs
```

---

## Architecture & Reference Documents

- [`docs/DESIGN.md`](docs/DESIGN.md) — Architectural design, event synchronization protocols, state machines, and MCP specifications.
- [`docs/VERIFICATION.md`](docs/VERIFICATION.md) — Verification results, test suites, and real-world agent compatibility logs.
- [`docs/MAINTAINING.md`](docs/MAINTAINING.md) — Maintenance checklist for synchronizing keys, CLI commands, and protocols.
- [`docs/tickets/`](docs/tickets/) — Improvement tickets, refactoring, and architectural evolution.

---

## License

This project is licensed under the [MIT License](LICENSE).

---

## Acknowledgements

`termade` was inspired by [Orca](https://github.com/stablyai/orca) (MIT licensed); its full-handoff and review-loop concepts informed this design. This is an independent implementation and does not copy its code.
