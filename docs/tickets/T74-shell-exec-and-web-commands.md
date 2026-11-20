# T74 — Shell command execution and web command palette

Status: done

## Problem

There is no way to run a quick shell command in a session's working directory without sending it to the agent as a prompt. The TUI only exposes agent prompts and `:` commands, the CLI has no equivalent of `sh -c` scoped to a session, and the web UI has no unified command entry point: native agent `slash_commands` are invisible unless the user already knows their names, and ertangle's own web actions (merge, diff, resume, ...) have no slash-command shortcuts.

## Spec

### Part A — Daemon, protocol, CLI, TUI (T74a, done)

1. **Protocol**: `Request::Exec { session: SessionId, command: String }` (`op: "exec"`), with a serde roundtrip test.
2. **Daemon** (`handlers/session.rs::handle_exec`):
   - An empty (trimmed) command is rejected with `Frame::error`.
   - The session is resolved by id or label; archived sessions are accepted. A missing session or a cwd that is not a directory produces an error frame.
   - `Event::UserPrompt { text: "! <command>", from: None }` is emitted so the command shows in the transcript.
   - A background thread runs `sh -c <command>` with `stdin(null)`, piped stdout/stderr, `current_dir(session.cwd)`, and `ERTANGLE_SESSION=<session id>`. Both pipes are drained on separate reader threads to avoid deadlock, and the child is killed after 60 seconds.
   - Combined output is capped at 64 KB / 200 lines (`...(truncated)` marker). The transcript receives a fenced `text` code block plus `[exit <code>]`; the reply note carries the same output and exit code so `ertangle exec` can print it directly.
3. **CLI**: `ertangle exec <session> <command...>` (alias `sh`, `trailing_var_arg`) sends `Request::Exec` and prints the returned note.
4. **TUI**: `!<command>` in the prompt input, `:exec <command>` / `:sh <command>` / `:!<command>`, and the `!` key in Normal mode (prefills `!` in Prompt mode). `:exec` is listed in `COMMANDS`, the `Space m` menu and `?` help. Unit tests cover each entry point.

### Part B — Web UI (T74b, done)

1. `applyRecord` tracks `Event::SessionCommands` into `state.sessions[session].slash_commands`.
2. The composer gets a `/` command palette combining built-in web commands (`/exec`, `/merge`, `/diff`, `/cancel`, `/resume`, `/sleep`, `/wake`, `/handoff`, `/model`, `/prune`, `/clear-queue`, `/restart`) with the active session's native slash commands. Keyboard navigation (arrows, Tab/Enter, Escape) and click selection are supported, plus `/ 命令` and `! Shell` toolbar buttons.
3. `sendPrompt()` routes `!<cmd>` and `/exec <cmd>` to `{ op: "exec", session, command }`; `//cmd` is escaped as a literal `/cmd` prompt; built-in web actions run their action; unknown `/cmd` is forwarded to the agent as a native slash command. The placeholder mentions `/命令` and `!Shell`.
4. String-level regression tests in `web.rs` (`web_supports_command_palette_and_shell_exec`), including the palette markup, built-in command table, native command badge, `session_commands` event handling, and both shell-exec routes..

## Verification

- (`T74a`) `cargo test -p ertangle-protocol -p ertangle-daemon -p ertangle-tui` with git signing disabled for test commits.
- (`T74b`) `cargo test -p ertangle-daemon web::`.
- `make pre-commit` (format, clippy, i18n catalogs).
