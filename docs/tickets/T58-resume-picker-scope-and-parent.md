# T58 — Scope tabs and parent context in the session resume picker

Priority: P2　Scope: `ertangle-tui`, `ertangle` CLI　Depends on: —

Status: needs-triage

## Problem

The resume picker and `ertangle resume --list` aggregate every ended or archived session from the local daemon and from all configured remote hosts. On a long-lived daemon the list grows across repositories, so users scroll through sessions from unrelated projects to find the one they want.

Rows show only a status glyph, label, `kind:model`, age, and a resumability mark. A session spawned by a coordinator looks identical to an independent session, so the parent/child (母子) relationship the user remembers is not visible.

The TUI already has a project-scope concept (`scope_here` plus `home_project`, exposed as `:scope here|all`) for the session tree, Bus, and task board. The resume picker ignores it.

## Evidence

- `crates/ertangle-tui/src/menu.rs:529-566` — `start_resume_picker` walks every host in `app.hosts` and the local state and pushes all ended/archived sessions; there is no `project_root()` filter.
- `crates/ertangle-tui/src/menu.rs:574-605` — the row label contains glyph, `s.label`, `kind:model`, age, and `↻`/`–`; neither `s.parent` nor `s.project_root()` is read.
- `crates/ertangle-tui/src/menu.rs:612-622` — `Picker` is constructed with no scope field; `PickStage::Resume` is declared at `menu.rs:315`.
- `src/cmd/session.rs:240-282` — `cmd_resume --list` sorts all sessions by `created_ms` and prints `s.cwd`, with no scope switch and no parent column.
- `crates/ertangle-core/src/lib.rs:313` — `Session.parent: Option<SessionId>`; `project_root()` at `lib.rs:359-363` resolves worktrees back to `repo_root`.
- `crates/ertangle-tui/src/app.rs:261-263` — `home_project` and `scope_here`; `toggle_scope` at `app.rs:1300-1313`; the `:scope`/`:project` command at `commands.rs:586-591`.
- `crates/ertangle-tui/src/menu.rs:706-715` — `Tab`/`BackTab` currently move the picker cursor, so the scope toggle needs its own key or a Resume-stage-specific binding.
- `crates/ertangle-tui/src/draw.rs:525` — `draw_picker` renders the picker body and is where the tab headers would be drawn.

## Analysis / decisions

- Keep the scope local to the picker. Add a `resume_scope` field to `Picker`, initialized from `app.scope_here` when the picker opens, instead of mutating the global flag. Toggling tabs inside a transient picker must not silently change the main session tree, Bus, or task board. Not confirmed: whether users actually expect the toggle to be global; check during review.
- Render two tabs, `This project` and `All projects`, above the filter line in `draw_picker`, highlight the active tab, and show the toggle key in the title. Because `Tab` currently moves the cursor, bind the toggle to `Ctrl+T`, or make `Tab` toggle only in `PickStage::Resume` while Up/Down and Ctrl+N/Ctrl+P keep navigating items. Pick one during implementation and document it in the picker title and help.
- `This project` means `s.project_root() == app.home_project`, applied to local and remote candidates alike, including archived sessions. Remote hosts have their own default cwd, so a remote session may never match the local `home_project`; that is acceptable and is exactly why the `All projects` tab exists.
- Parent display: build an `id -> label` map from all candidates plus live sessions and archived sessions on every host, so a parent that is still running (and therefore absent from the ended list) can still be named. Append `↳ {parent_label}` to the row; if the id cannot be resolved, show the short id. Keep the chronological sort. Grouping children under their parent was considered but rejected: it hides recency and breaks substring filtering.
- Project display: prefix rows with `project_name(s.project_root(), &roots)` only in the `All projects` tab so the focused tab stays narrow. Reuse `app::project_name` and the existing project colors if convenient.
- An empty `This project` result must show an explicit message such as "no ended sessions in {project}; switch to All projects" rather than a silent empty list.
- CLI: add `--scope here|all` to `ertangle resume --list`, defaulting to `all` for backward compatibility, and append `↳ {parent_label}` after the existing note column to minimize output-format breakage. A JSON output mode is out of scope but worth noting as a future option.

## Implementation

- `crates/ertangle-tui/src/menu.rs`: extend `Picker` with the resume scope; filter candidates in `start_resume_picker`; build the parent map and the new label; handle the toggle key in `handle_pick_key` for `PickStage::Resume` by rebuilding items while preserving the filter text and clamping the cursor.
- `crates/ertangle-tui/src/draw.rs`: draw the tab header for the Resume stage in `draw_picker`.
- `src/cmd/session.rs`: add the `--scope` flag and the parent annotation.
- `crates/ertangle-tui/src/commands.rs` and `help.rs`: update `:resume` help text.
- Update `locales/ertangle.pot` and `locales/en.po` with `make i18n-extract` and fill in every new `msgstr`.

## Acceptance criteria

- The TUI resume picker opens in `This project` scope by default (matching the global default) and lists only sessions whose `project_root()` equals `home_project`, including archived ones.
- The scope toggle reveals sessions from every host and project again, and the active tab is visibly highlighted.
- Each row shows the parent label when `Session.parent` is set, resolving through live and archived sessions and falling back to the short id.
- Rows in `All projects` scope are prefixed with their project name; rows in `This project` scope are not.
- `ertangle resume --list --scope here` filters by project root, `--scope all` keeps the previous output, and both show parent annotations.
- An empty focused scope offers the `All projects` alternative instead of a blank picker.
- Unit tests cover project filtering, parent resolution (including a live parent and an unknown id), the scope toggle, and the CLI flag; `make pre-commit` and `make i18n-check` pass.
