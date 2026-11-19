# T51 — Show the quota of Gemini (Antigravity) sessions

Priority: P2　Scope: `termade-adapter`, `termade-daemon`, `termade-tui`　Depends on: —

Status: needs-info

## Problem

The header quota summary and the `:quota` popup only ever show Claude (and Codex when it runs). Gemini, which runs as the Antigravity agent kind (`agy acp`), never reports a quota, so its row stays empty. Quota-driven features (the quota warning, auto-handoff, the spawn fallback in `check_spawn_quota`) are blind for Antigravity sessions.

## Evidence

- Quotas reach the daemon only as `AgentEvent::Quota` from an adapter (`crates/termade-adapter/src/lib.rs:62`, mapped to `Event::QuotaUpdated` in `crates/termade-daemon/src/lib.rs:643`).
- Only two adapters emit it: Claude parses `rate_limit_event` (`crates/termade-adapter/src/claude.rs:246`) and Codex parses `rate_limits` (`crates/termade-adapter/src/codex.rs:576`).
- Antigravity runs through the generic ACP adapter (`crates/termade-adapter/src/acp.rs`, command `agy acp` from `crates/termade-core/src/lib.rs:104`), which has no quota handling.
- The `:quota` popup lists every structured agent installed locally (`crates/termade-tui/src/quota.rs:18`), so Antigravity shows up with no data.
- `agy` tracks quota internally: its log shows `quota_manager.go: doRefreshQuota` entries (`~/.gemini/antigravity-cli/cli.log`). `agy --help` (local version) offers no `quota`/`usage` subcommand.

## Analysis

Possible sources, none confirmed yet:

1. **ACP stream**: `agy acp` may send quota or usage data in a vendor extension or `_meta` field of `session/update`. Needs a capture of a real ACP session.
2. **A CLI command or slash command** that prints usage, polled periodically. None found in `agy --help`; check slash commands inside an interactive session and `agy changelog`.
3. **The Google quota API** that the Gemini CLI uses (`retrieveUserQuota` returning per-model `remainingFraction` and `resetTime`), called with the user's OAuth credentials. This duplicates `agy` internals, depends on an undocumented endpoint, and needs careful handling of the credential files. Last resort.
4. **Rate-limit errors**: at minimum, map a `RESOURCE_EXHAUSTED` / 429 error from the ACP stream to `Quota { reached: true }` so auto-handoff works even without percentages.

Not confirmed: whether accounts that authenticate through a GCP project (Vertex) have a per-user percentage quota at all; project-level limits may only surface as 429 errors. Source 4 covers that case.

## Implementation

1. Research: capture an `agy acp` session (including one that hits a limit) and check sources 1 and 2. Record findings in this ticket under `## Comments`.
2. Implement the cheapest working source in `acp.rs` (or an Antigravity-specific hook), emitting `AgentEvent::Quota` with `agent = "antigravity"` and window names matching what the source reports.
3. Always implement source 4.
4. In the `:quota` popup, show "no quota data reported" for an installed agent with no data instead of an empty row.

## Acceptance criteria

- An Antigravity session that reports usage shows up in the header summary and the `:quota` popup.
- An Antigravity session that hits a rate limit marks the quota as reached and can trigger auto-handoff.
- Unit tests parse captured ACP samples (fixtures checked in, with personal data removed).
- `make pre-commit` passes.
