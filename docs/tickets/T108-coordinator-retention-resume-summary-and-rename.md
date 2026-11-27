# T108 — Retain ended coordinators, show summary/recency in resume picker, and add session rename

Priority: P2　Scope: `ertangle-core`, `ertangle-protocol`, `ertangle-daemon`, `ertangle-tui`　Depends on: T58, T73

## Problem

1. **Coordinators vanish from the sidebar's "Ended" list after restart**: `Shared::prune_ended_sessions` (`crates/ertangle-daemon/src/sweep.rs:159-176`) archives any ended session without a worktree (`s.branch.is_none()`) once it has been ended for more than 1 hour (`KEEP_ENDED_MS = 3600 * 1000`). Because coordinators run in the main repo (`branch == None`) while their spawned workers have dedicated worktrees (`branch.is_some()`), an ended coordinator is archived and disappears from the sidebar's "Ended" group while its ended workers stay visible—leaving orphaned workers in the sidebar and forcing the user to hunt for the coordinator in `:resume`.
2. **`:resume` picker rows only show opaque default labels and creation age**: Default labels are auto-generated as `<role>/<4-char-hex>` (e.g. `coordinator/08ca`, `worker/4192`). Although `State::archive_session` (`crates/ertangle-core/src/lib.rs:1126-1141`) already populates `s.status_note` with the last user prompt when archiving, `rebuild_resume_items` (`crates/ertangle-tui/src/menu.rs:1365-1484`) does not render `status_note` (or a prompt snippet for non-archived ended sessions), and sorts by `s.created_ms` instead of `s.ended_or_created_ms()`. Users see a list of opaque hex labels sorted by creation time rather than most-recently-active time, with no title or summary to tell them apart.
3. **No TUI command to rename/title a session**: `Event::SessionLabel { session, label }` already exists in `ertangle-core` (`crates/ertangle-core/src/lib.rs:878, 1294`), but there is no `Request::SessionLabel` in `ertangle-protocol` or `:rename` / `:label` command in `ertangle-tui` (and `State::apply` only updates `self.sessions`, not `self.archived_sessions`).

## Evidence

- `crates/ertangle-daemon/src/sweep.rs:159-176` — `prune_ended_sessions` checks `!s.status.is_live() && s.branch.is_none() && now_ms.saturating_sub(s.ended_or_created_ms()) > KEEP_ENDED_MS`, regardless of whether `s.role == Role::Coordinator` or whether any active/ended child session in `inner.state.sessions` still has `child.parent.as_deref() == Some(s.id.as_str())`.
- `crates/ertangle-tui/src/menu.rs:1440-1483` — `rebuild_resume_items` sorts by `std::cmp::Reverse(a.created_ms)`, computes age from `s.created_ms`, and formats the label without any `status_note` or transcript prompt summary.
- `crates/ertangle-core/src/lib.rs:878-882, 1294-1298` — `Event::SessionLabel` exists, but `crates/ertangle-protocol/src/lib.rs` lacks `Request::SessionLabel`, `crates/ertangle-daemon/src/handlers/session.rs` lacks `handle_label`, and `crates/ertangle-tui/src/commands.rs` lacks `:rename` / `:label`.

## Analysis / decisions

1. **Keep ended coordinators and parents of unarchived children in the sidebar**:
   - In `Shared::prune_ended_sessions` (`crates/ertangle-daemon/src/sweep.rs:159-176`), do **not** auto-prune an ended session if:
     - `s.role == Role::Coordinator`, OR
     - any remaining session in `inner.state.sessions.values()` references it as parent (`other.parent.as_deref() == Some(s.id.as_str())`).
   - Manual `:prune` (`Shared::prune_sessions`) and `:rm` / `:clean` remain unchanged so users can still explicitly archive ended coordinators when done.
2. **Show summary / title and sort by recency (`ended_or_created_ms`) in `:resume` picker**:
   - In `rebuild_resume_items` (`crates/ertangle-tui/src/menu.rs:1440-1484`):
     - Sort `ended` by `std::cmp::Reverse(a.ended_or_created_ms())` (most recently ended/active first) and compute `age = format_compact_age(now_ms, s.ended_or_created_ms())`.
     - Extract a concise summary snippet (up to 48 characters, single-line, trimmed) for each session:
       - Prefer `s.status_note` if present, non-empty, and not just the generic restart boilerplate (i.e. `!n.contains(":resume")`).
       - Otherwise, look up the session's transcript in `state.transcripts.get(&s.id)` (checking the host's state or `app.state`) and find the last prompt line via `ertangle_core::parse_prompt_line(line).map(|(_, t)| t)`.
       - Append `  — <snippet>` to the end of the picker item label when a non-empty snippet is found. Because `Picker::visible()` filters on the item label (`label.to_ascii_lowercase().contains(&f)`), users can immediately search/filter by summary or prompt text in `:resume`.
3. **Add `Request::SessionLabel` and TUI `:rename` (alias `:label`) command**:
   - In `crates/ertangle-core/src/lib.rs:1294-1298`: update `Event::SessionLabel` in `State::apply` to also update `self.archived_sessions.get_mut(session)` if the session is archived.
   - In `crates/ertangle-protocol/src/lib.rs`: add `Request::SessionLabel { session: SessionId, label: String }`.
   - In `crates/ertangle-daemon/src/handlers/mod.rs` & `session.rs`: dispatch `Request::SessionLabel` to `session::handle_label(shared, reply, session, label)`. Validate that `label.trim()` is non-empty, resolve `sid` (checking both `sessions` and `archived_sessions` via `resolve_session_id` or `archived_sessions`), emit `Event::SessionLabel { session: sid, label: trimmed }`, and reply with `Frame::ok_note(tr!("已重新命名為 {label}", label = trimmed))`.
   - In `crates/ertangle-tui/src/commands.rs`: add `"rename"` in `COMMANDS` (`"<新名稱>  重新命名選取的 session (亦可用 :label)"`) and handle `"rename" | "label"` in `execute_command`, sending `Request::SessionLabel` for the selected session (or returning usage hint `用法：:rename <新名稱>` if empty).

## Implementation

1. `crates/ertangle-daemon/src/sweep.rs`: update `prune_ended_sessions` to skip `Role::Coordinator` and sessions that still have children in `inner.state.sessions`.
2. `crates/ertangle-core/src/lib.rs`: update `Event::SessionLabel` handler in `State::apply` to check both `self.sessions` and `self.archived_sessions`.
3. `crates/ertangle-protocol/src/lib.rs`, `crates/ertangle-daemon/src/handlers/mod.rs`, `crates/ertangle-daemon/src/handlers/session.rs`: add `Request::SessionLabel` and `handle_label`.
4. `crates/ertangle-tui/src/menu.rs`: update `rebuild_resume_items` to sort and format age using `ended_or_created_ms()`, and append `  — <snippet>` from `status_note` or last transcript prompt.
5. `crates/ertangle-tui/src/commands.rs`: add `:rename` / `:label` command and `COMMANDS` entry.
6. Update i18n catalogs (`make i18n-extract`, fill `locales/en.po`, verify with `make i18n-check`) if any new `tr!` / `tr_noop!` strings are added.
7. Add unit tests in `crates/ertangle-daemon/src/tests.rs` and `crates/ertangle-tui/src/tests.rs`.

## Acceptance criteria

- Ended `Role::Coordinator` sessions and ended parent sessions with remaining children in `state.sessions` are not auto-archived after 1 hour by `prune_ended_sessions`, while ordinary ended workers without worktrees or children still are.
- `rebuild_resume_items` sorts by `ended_or_created_ms()` descending, displays age from `ended_or_created_ms()`, and appends `  — <snippet>` from `status_note` or the last prompt in the transcript.
- `:rename <new_label>` (and `:label <new_label>`) renames the selected session via `Request::SessionLabel` and `Event::SessionLabel`.
- `make pre-commit` and `cargo test` pass.
