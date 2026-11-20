# T60 — Adopt English-first tickets and code comments

Priority: P3　Scope: contributor documentation　Depends on: —

Status: needs-triage

## Problem

T49 and T50 are already written in English, but `docs/agents/issue-tracker.md` still prescribes Chinese section headings, and most older tickets plus inline code comments are in Traditional Chinese. Mixed-language context costs agents extra tokens and makes review harder. The repository's Taiwanese writing rules should apply only when content is intentionally zh-TW, such as user documentation and translated UI strings.

## Evidence

- `docs/agents/issue-tracker.md:9` mandates the headings `優先度／範圍`, `## 問題`, `## 證據`, `## 分析邏輯`, `## 實作方式`, `## 驗收條件`.
- Tickets T01–T46, T56, and T57 use those Chinese headings; T49 and T50 use `Priority:`, `## Problem`, `## Evidence`, `## Analysis`, `## Implementation`, `## Acceptance criteria`.
- Inline comments are Chinese across the crates, for example `crates/ertangle-tui/src/menu.rs:531`, `crates/ertangle-daemon/src/handlers/session.rs:1,28,40`, and `crates/ertangle-core/src/lib.rs:342,360`.
- `AGENTS.md` and `.agents/skills/taiwanese-tech-writing/SKILL.md` govern Traditional Chinese text only; they do not require new tickets or comments to be Chinese.

## Analysis / decisions

- From T58 onward, every new ticket uses English headings and English prose: `Priority / Scope / Depends on`, `## Problem`, `## Evidence`, `## Analysis / decisions`, `## Implementation`, `## Acceptance criteria`. Older Chinese tickets stay as historical records; do not rewrite them.
- Inline comments and Rustdoc in new or changed code are written in English. Existing comments are translated incrementally and file by file in T61, not as drive-by edits inside unrelated feature changes.
- User-facing strings and their translations keep using the gettext workflow (`tr!` plus `locales/`). This ticket does not change UI language behavior.
- Update `docs/agents/issue-tracker.md` and `docs/tickets/README.md`; audit other contributor instructions for conflicting guidance.

## Implementation

- Rewrite the ticket-convention section of `docs/agents/issue-tracker.md` with the English headings and add a rule that new inline comments are English.
- Add a short language-convention note to `docs/tickets/README.md`.
- No source code changes in this ticket.

## Acceptance criteria

- `docs/agents/issue-tracker.md` lists the English section names and states the English-first rule for tickets and comments.
- `docs/tickets/README.md` documents the same convention.
- No Traditional Chinese documentation or UI string is introduced by this ticket; `make pre-commit` passes.
