# Issue tracker: Local Markdown (`docs/tickets/`)

Issues for this repo live as markdown files in `docs/tickets/`, following the convention already used for T01–T25.

## Conventions

- One ticket per file: `docs/tickets/TNN-<slug>.md`, numbered sequentially (`T26`, `T27`, …). Never combine several tickets in one file.
- Every ticket has a row in the table in `docs/tickets/README.md` (`| [TNN](TNN-slug.md) | title | priority | dependencies |`). Add the row when the ticket is created.
- New tickets are written in English. Ticket body sections, in order: `Priority / Scope / Depends on` line, `## Problem`, `## Evidence` (file:line references), `## Analysis / decisions` (list what was *not* confirmed), `## Implementation`, `## Acceptance criteria`. Older Chinese tickets (`優先度／範圍`, `## 問題`, and so on) remain valid as historical records; do not rewrite them.
- Inline comments and Rustdoc in new or changed code are written in English. See T60 for the convention and T61 for incremental translation of legacy comments.
- Priority: P0 data loss or corrupted state; P1 security or regression guard; P2 stability and maintainability; P3 cleanup.
- Triage state is a `Status:` line under the title (see `triage-labels.md` for the role strings). When a ticket is done, append ` (已完成)` to its title cell in the README table; the `Status:` line can then be dropped.
- Larger features that need a spec first: put the spec at `docs/tickets/TNN-<slug>.md` as well and split implementation tickets into separate `TNN` files that reference it.
- Comments and conversation history append to the bottom of the ticket under a `## Comments` heading.
- Manual / real-machine verification results go in `docs/VERIFICATION.md`, not in the ticket.

## When a skill says "publish to the issue tracker"

Create the next `docs/tickets/TNN-<slug>.md` and add its row to `docs/tickets/README.md`.

## When a skill says "fetch the relevant ticket"

Read `docs/tickets/TNN-*.md`. The user will normally pass the ticket number (e.g. `T24`) or the path.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a file with one **child** file per ticket.

- **Map**: `docs/tickets/<effort>/map.md` (the Notes / Decisions-so-far / Fog body).
- **Child ticket**: `docs/tickets/<effort>/NN-<slug>.md`, numbered from `01`, with the question in the body. A `Type:` line records the ticket type (`research`/`prototype`/`grilling`/`task`); a `Status:` line records `claimed`/`resolved`.
- **Blocking**: a `Blocked by: NN, NN` line near the top. A ticket is unblocked when every file it lists is `resolved`.
- **Frontier**: scan `docs/tickets/<effort>/` for files that are open, unblocked, and unclaimed; first by number wins.
- **Claim**: set `Status: claimed` and save before any work.
- **Resolve**: append the answer under an `## Answer` heading, set `Status: resolved`, then append a context pointer (gist + link) to the map's Decisions-so-far in `map.md`.
