# Changelog

All notable changes to this project will be documented in this file.
## [0.8.0] - 2026-11-19

### 🚀 Features

- *(adapter,tui)* Report Antigravity quota via agy -p /quota (T51)
- *(tui,web)* Show remaining quota percentages and improve readability
- *(tui,web)* Add quota agent tabs, clearer labels, and pre-select resume target
- *(settings)* Configurable worker agent order with sectioned settings panel

### 🐛 Bug Fixes

- *(tui)* Sort agents with reported quota ahead of unreported ones
- *(tui)* Check PATH for opencode and agy in quota popup
- *(security)* Harden WebSocket frame limit, token comparison, file serving, XSS, and temp files (T52)
- *(daemon)* Scope QUOTA_WARN notifications and instructions to the affected agent (T53)
- *(codex)* Support mcp elicitation approval and enable network in bwrap sandbox
- *(codex)* Pass daemon socket to mcp and fall back from broken bwrap sandbox
- *(codex)* Use TOML dotted path for mcp_servers.termade.env.TERMADE_SOCKET

### 📚 Documentation

- *(tickets)* Add T52 for SAST security hardening
- *(tickets)* Add T53 for agent-scoped QUOTA_WARN and mark T52 done
- *(tickets)* Mark T53 as done

### 🧪 Testing

- *(adapter)* Verify is_installed checks binary presence per agent

### ⚙️ Miscellaneous Tasks

- Retry apt-get when installing gettext in i18n job

## [0.7.1] - 2026-11-19

### 🚀 Features

- *(web)* Compact, readable tool call list

### 🐛 Bug Fixes

- *(procs)* List and kill only processes started by agent tools
- *(web)* Show every tool call in the expanded tool list

## [0.7.0] - 2026-11-19

### 🚀 Features

- *(core)* Count tool calls per session and show them in the TUI
- *(collab)* Add [collab] token_hints setting and TUI toggle (T47)
- *(tui)* Compact session sidebar, copyable input, normal-mode hint
- *(tui)* Add resume picker for previously opened sessions (T47)
- *(tui)* Add native clipboard backend chain with tmux passthrough (T48)
- *(session)* Support archiving and resuming removed historical sessions
- *(spawn)* Resolve agent kind from request, role, parent, and defaults
- *(adapter,tui)* Report ACP rate-limit errors as quota reached and clarify empty quota rows (T51)
- *(handoff)* Allow role and parent identity change via handoff

### 🐛 Bug Fixes

- *(adapter)* Stop reporting in-progress ACP tool updates as failures
- *(core)* Drop content-free JSON from tool summaries
- *(tui)* Expand picker width to 72 columns for resume labels (T47)
- *(tui)* Prefer native clipboard tools in local tmux sessions
- *(tui)* Make Tab completion match the command hint popup
- *(adapter)* Clear ACP rate-limit quota after a successful turn (T51)
- *(web)* Read flattened event records so live updates apply
- *(handoff)* Only treat role changes as promotion and permission reset (T50)
- *(render)* Render markdown tables on web and wrap long TUI table cells
- *(transcript)* Label prompts sent by agents and termade apart from the user
- *(web)* Stop the chat transcript flickering while agents stream

### 📚 Documentation

- *(tickets)* Add T47 agent token savings plan
- Add domain glossary, ADR 0001 and tickets T49/T50
- *(tickets)* Mark T49 ready-for-agent
- *(tickets)* Add T51 for Antigravity quota reporting
- *(tickets)* Note account constraint on T51

### ⚡ Performance

- *(tui)* Optimize transcript rendering for long sessions with incremental cache

### ⚙️ Miscellaneous Tasks

- *(release)* Dispatch release workflow when tag is pushed with GITHUB_TOKEN
- *(release)* Read RELEASE_TOKEN via env in step condition
- *(i18n)* Use 自動完成 for completion terminology

### I18n

- *(settings)* Shorten English settings labels

## [0.6.0] - 2026-11-18

### 🚀 Features

- *(tui,daemon)* Support role selection in tui and modernize collab prompt with role scope
- *(observability)* Add TUI subagent & background process visibility with process group control
- *(security)* Forward Claude approvals and redact secrets across MCP tools
- *(git)* Add pre-merge validation, conflict resolution loop, and auto-review gatekeeper (T34)
- *(collab)* Add inter-agent English protocol and zero pleasantries policy (T37)
- *(tui,daemon,protocol)* Add diff file navigation, single-file folding, and hunk/file discard (T35)
- *(core,daemon,tui,cli)* Add multi-agent playbooks and Markdown session export (T36)
- *(tui)* Show project name in terminal window title
- *(tui)* Show session status glyph in terminal window title
- *(tui)* Show :web status in popup instead of status bar
- *(web)* Project-scoped mobile companion with overview tab and fuller event mirroring
- *(tui)* Add Project right panel and Grid center view
- *(daemon)* Quota-aware spawn, QUOTA_WARN bus alerts and installable termade-ade skill
- *(tui)* Add inject_skill settings toggle and :skill command (T42)
- *(web)* Fetch model lists from daemon via list_models (T44)
- *(tui)* Show :quota as a popup with per-window progress bars (T45)
- *(collab)* Warn worktree agents that git stash is shared across worktrees
- *(daemon)* Auto-resume on restart, shared cargo target for worktrees, early quota notice (T46)

### 🐛 Bug Fixes

- *(web)* Fix QR code format bits, interleaving, and terminal rendering; support query token and auth prompt
- *(web)* Fix QR encoder so codes are scannable
- *(adapter)* Avoid procps kill misparsing -<pgid> as -1
- *(web)* Replace stale model catalog with TUI's Claude list (T43)
- *(adapter)* Treat Claude allowed_warning rate limit as not reached
- *(web)* Extend QR encoder to v9 for project-scoped URLs
- *(web)* Distinguish user prompts from agent replies in transcript

### 📚 Documentation

- Update README, in-app help and tickets for role selection and pre-commit checks
- *(tickets)* Add T32-T36 implementation specs
- *(tickets)* Add T37 ticket for token-efficient inter-agent communication
- *(readme)* Add Coordinator and Worker collaboration walkthrough
- *(tickets)* Add T39-T43 and index T37
- *(tickets)* Close T32 and link the T39 regression
- *(tickets)* Mark implemented T33-T36 as done

### 🎨 Styling

- *(tui,daemon)* Format codebase and sync i18n catalogs

### ⚙️ Miscellaneous Tasks

- *(ci)* Add pre-commit verification checklist and setup-hooks script
- *(makefile)* Add pre-commit target for one-command verification
- *(release)* Add prepare-release workflow and fix BSD sed in release-bump
- *(release)* Fix empty bumped version in prepare-release and use English messages

## [0.5.0] - 2026-11-17

### 🚀 Features

- *(tui)* Show session model and effort below transcript
- *(tui)* Scope bus messages and tasks to the current project

### 🐛 Bug Fixes

- *(tui)* Copy transcript message by index and improve clean/bus behavior
- *(tui)* Show sub-agent sessions and reduce sidebar crowding
- *(tui)* Make clean remove all ended sessions and honor force
- *(daemon)* Allow removing ended sessions sharing cwd with live ones
- *(tui)* Map session clicks by two-line row height
- *(tui)* Do not count restored bus history as unread
- *(daemon,i18n)* Restore blocking web sockets on macOS and sync translations
- *(daemon)* Use mock agent binary in integration tests

### ⚙️ Miscellaneous Tasks

- *(tui)* Simplify session meta warning color
- Set job timeouts to prevent runner hangs

## [0.4.0] - 2026-11-16

### 🚀 Features

- *(tui,daemon)* Allow configuring and launching web companion from TUI
- *(ui,daemon)* Allow user to customize web version port
- *(release)* Integrate git-cliff for changelog generation and automated releases

### 🚜 Refactor

- *(ui,daemon)* Rename mobile companion to web version support

## [0.3.0] - 2026-11-16

### 🚀 Features

- *(daemon,tui)* Implement T28 Tailscale remote integration, token persistence, and auto-reconnect
- *(tui)* Implement multi-host aggregated view and remote routing (T29)
- *(daemon)* Implement mobile companion web interface and terminal QR code (T30)
- *(web)* Redesign mobile companion for native thumb-first mobile experience
- *(web)* Replace excessive emojis with SVG icons and expand model picker and session controls
- *(web)* Replace native dropdowns with segmented pickers and selectable model lists
- *(web)* Restructure transcript into conversational turns grouping text, thoughts, and tools
- *(web)* Add project badges, top session switcher sheet, and disable double-tap zoom
- *(tui,web)* Support direct link clicks in TUI and interactive file pills with markdown rendering in web
- *(web)* Support mobile preview of text and image files with line anchor jumping

### 🐛 Bug Fixes

- *(tui)* Show Ctrl-F effort hint when spawning session
- *(tui)* Show and open markdown links in transcript
- *(web)* Fix WebSocket field mapping and complete transcript action controls
- *(web)* Restructure header into two-tier layout to prevent crowded session info
- *(web)* Redesign session cards and grid to prevent layout crowding on mobile
- *(web)* Polish circular close button with SVG icon and add bottom cancel option for sheets
- *(ci)* Resolve clippy type complexity warnings, format code, and complete i18n translations

### 📚 Documentation

- *(tickets)* Add T28-T30 for Tailscale remote reconnect, multi-host TUI, and mobile web companion
- *(agents)* Enforce English Conventional Commits in guidelines

### ⚙️ Miscellaneous Tasks

- Release v0.3.0

## [0.2.0] - 2026-11-14

### 🚀 Features

- Runtime model/effort switching, settings panel, agent-spawn guardrails, native slash commands
- *(i18n)* Add gettext .po infrastructure and translate settings panel
- *(i18n)* Translate TUI strings via tr!
- *(i18n)* Translate daemon, adapter and core user messages
- *(i18n)* Translate CLI messages; parse ask message id by msg_ prefix
- *(i18n)* Translate CLI --help via generated tr_noop list; disable msgmerge fuzzy matching

### 📚 Documentation

- *(tickets)* Add T27 plan for English UI via gettext .po
- *(i18n)* English translations, translation guide, T27 status

### 🎨 Styling

- Use half-width parentheses in zh-TW text per taiwanese-tech-writing

### ⚙️ Miscellaneous Tasks

- Release v0.2.0

## [0.1.1] - 2026-11-13

### 🐛 Bug Fixes

- *(tui)* List all Claude model versions in model picker

### 📚 Documentation

- *(readme)* Add prebuilt binary and cargo install --git instructions

### ⚙️ Miscellaneous Tasks

- Release v0.1.1

## [0.1.0] - 2026-11-13

### 🚀 Features

- M0 skeleton + M1 native agent adapters
- M2 collaboration core (ade mcp server, bus inbox/wait/ask, guardrails)
- M3 workspace/review, M4 polish/policy/sleep/replay, M5 remote TCP
- *(tui)* Markdown rendering, transcript scrolling, diff gutter, responsive layout
- *(tui)* Echo prompts, add copy, mouse scroll and cursor editing
- *(daemon)* Make mcp surface english
- *(tui)* Drag-select copy, hide thoughts by default, cut redundant redraws
- Config.toml and per-agent model selection (T11)
- *(daemon)* Harden remote mode and socket dir (T05)
- Session permission in state, parent cap, role/agent permission config (T12)
- *(policy)* Daemon is the single approval decision point (T04)
- *(tui)* Add three/two/tabs layout modes (T13)
- *(tui)* Ctrl-E permission picker with bypass confirmation, show !bypass (T12)
- Event log rotation with snapshot and write-failure reporting (T08)
- *(worktree)* Support dry_run on merge (T02)
- *(adapter)* Wire bin field from config.toml to adapter spawn (T11)
- *(tui)* Persist layout mode preference in config.toml (T13)
- *(tui)* Show log failure indicator in header and propagate health (T08)
- *(policy)* Forward Claude approvals through ade_approve MCP tool (T04)
- *(adapter)* Select model for ACP agents via session/set_config_option (T11)
- *(adapter)* Pass model and sandbox to codex thread/start (T11, T12)
- Remove sessions (ade rm / SessionRemove) and clean up failed spawns
- *(tui)* Space which-key menu, ':' command line, agent/model picker
- *(tui)* Command hints popup and live model lists for opencode/agy
- *(tui)* List Codex models via 'codex debug models'; make failed-spawn test independent of installed agents
- Reasoning effort for spawn (CLI, config, MCP, TUI picker)
- File and image attachments in prompts
- *(tui)* Fuzzy project file search for @ attachments
- Detect quota/rate limits and hand a session off to another agent
- Show agent account quota (Claude rate_limit_event, Codex account/rateLimits)
- Quota warning notes and opt-in automatic handoff
- Show per-session context usage
- *(tui)* Help panel and slimmer chrome
- *(tui)* Fold consecutive tool calls in transcript (O to expand)
- *(tui)* Input history (up/down, persisted) and per-session prompt queue
- *(tui)* Multi-line prompt input (Ctrl-J/Shift-Enter, soft wrap, growing box)
- *(tui)* Show which project each session belongs to; P/:scope to filter
- *(tui)* Project chip at row start and grouping when showing all projects
- *(tui)* Confirm shutdown, :stop for this project, survive daemon disconnect
- *(tui)* Notify on turn end, pending approval, failure and quota limit (T15)
- *(daemon)* Unblock tasks when their dependencies finish (T18)
- *(tui)* Task view grouped by status with assignee jump and :task-status (T18)
- *(tui)* Search in transcript and diff views (T16)
- *(cli)* Add shell completions and dynamic session completion (T21)
- Resume ended sessions in their original agent conversation (T14)
- Inject collaboration instructions into spawned agents (T20)
- *(budget)* Enforce session/tree cost and child agent limits (T17)
- *(daemon)* Move prompt queue from TUI to daemon (T19)
- *(adapter)* Surface agent errors in transcript and status note (T23)
- *(tui)* Default to two-column layout (Bus hidden); add T24 ticket
- *(bus)* Forward messages for handed-off sessions to their successor (T24)
- *(bus)* Nudge unread messages when recipient returns to idle; judge over_budget on successor (T25)

### 🐛 Bug Fixes

- *(daemon)* Wake idle workers for broadcasts and task assignments
- *(tui)* Render markdown tables with display width
- *(daemon)* Safe merge and gc for session worktrees (T02, T03)
- *(daemon)* Restore state from event log on restart (T01)
- *(daemon)* Validate socket dir and capture errors in ensure_daemon
- *(cli)* Restore 'ade spawn --kind/--prompt', parse kind:model, show permission in sessions
- *(adapter)* Stop status/cost flicker from claude adapter
- Readable transcript tool lines and prune stale sessions on restart
- *(tui)* Render tool lines raw, unwrap MCP text results, clearer live count, prune ended sessions after 1h
- *(daemon)* Tighten own socket dir to 0700 instead of refusing to start
- *(tui)* Pick model and effort separately; effort optional (Ctrl-F)
- Spawn in the caller's project (not the daemon cwd); default to this project's sessions
- *(config)* Ignore repo-level notify_command (T15)
- *(daemon)* Keep shared worktree until the last session is removed; tolerate missing branch (T22)
- *(bus)* No duplicate nudge after handoff; nudge after raising budget (T25)

### 🚜 Refactor

- *(daemon)* Extract git, paths, client modules and move tests (T09, part 1)
- *(daemon)* Extract logwriter and server modules (T09, part 2)
- *(tui)* Extract draw and tests modules (T09, part 3)
- *(daemon)* Extract bus module (T09, part 4)
- *(daemon)* Extract handlers module (T09, part 5)
- *(tui)* Extract app, diff, clipboard, mouse, input, commands modules (T09, part 6)
- *(cli)* Extract subcommands to src/cmd/ (T09, part 7)

### 📚 Documentation

- Add improvement tickets from code review
- Sync DESIGN/README with implementation and make Makefile portable (T10)
- Research agent model selection and approval forwarding
- Add MAINTAINING.md listing what must be updated together
- Unify termade description
- Document O (tools fold)
- Help and README for multi-line input
- Scope default
- *(tickets)* Add T14–T21
- *(tickets)* Mark T14–T21 done
- *(tickets)* Add T22 (shared worktree remove) and T23 (surface agent errors)
- Record T22/T23 manual verification
- Record T24 manual verification
- *(tickets)* Mark T01–T13 done; add T25 (unread nudge on idle)
- Record T25 manual verification
- Rewrite README, add English version, and enforce Taiwanese tech writing rules
- Configure agent skills (issue tracker, triage labels, domain docs)
- Add MIT LICENSE, repository metadata, and Orca acknowledgement

### ⚡ Performance

- *(tui)* Throttle transcript markdown rebuild while streaming
- Move log IO out of lock, bound client queues, tolerate poisoned locks (T07)

### 🎨 Styling

- Apply rustfmt across workspace

### 🧪 Testing

- *(tui)* Cover tables/links/strike; add Q to shut down daemon
- Add CI workflow, fake-claude adapter and daemon e2e tests (T06)
- Mark fake_claude.sh executable in git
- *(tui)* Fix wire-format assertion in hidden bus test
- Flush log before restore in prune/restart tests
- *(completions)* Canonicalize temp dir for macOS symlinked /var

### ⚙️ Miscellaneous Tasks

- Rename ade to termade (crates, binary, env vars, paths, docs)
- Ignore .fuse_hidden files
- Add release workflow (build binaries and publish GitHub Release on v* tag)
- *(release)* Add macOS universal2 binary
- Fix release version check; make fake_codex.sh executable

### Build

- Add Makefile and ignore dist/
- Make project directly installable via cargo install

### Merge

- T05 remote mode security
- T10 docs and Makefile
- T13 TUI layout modes
- T07 and T08

### Termade

- Changes from t15-notify
- Changes from t21-compl

### Tui

- Trim hint text in titles and status bar

<!-- generated by git-cliff -->
