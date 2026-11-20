# T65 — Make the web pending-question card usable

Priority: P1　Scope: `ertangle-daemon`　Depends on: T64

Status: done

## Problem

The web pending-question UI (multiple-choice questions) was nearly unusable, worst in the Chat tab:

- Options, the custom input and the submit button were crammed into one horizontal `.btn-row`; `.btn-pill {flex:1}` stretched the submit button across the row.
- Every state event (agent output streams constantly) rebuilt `innerHTML`, wiping checked options and typed custom text.
- The sticky banner lived in `<main>`; with `.tab-view {height:100%}` it pushed the Chat composer off-screen, and long option lists covered the whole screen.
- Only the first question of a `group` was shown, with no "1/N" hint.
- The daemon resolved every grouped sibling with `[]` after the first answer, so answers to the 2nd..Nth questions of a multi-question request were lost.

## Implementation

- Vertical layout: header and question text wrap (no `word-break: break-all`); options are full-width tappable rows (bold label, description on its own line, selected highlight, >= 44px touch target); custom input on its own line; submit button right-aligned and not stretched.
- Re-render is gated by a signature (`setCardHtml` compares `dataset.sig`), so selections and custom text survive state events.
- In the Chat tab, when the question belongs to the active session, it renders inline in `#chat-question-slot` (between transcript and composer, `max-height: 50vh` with internal scroll) and the global banner is hidden. Elsewhere the banner is a one-line summary plus an "Answer" button that switches to that session's chat.
- Multiple questions show "1/N"; questions sharing a `group` render together and one submit sends one `answer_question` per question.
- At least one question in a group must be answered; unanswered siblings are sent as `answers: []` (skip). The daemon no longer auto-clears grouped siblings: each question is resolved only by its own `answer_question`.
- Single-select still requires the submit button (avoids mis-taps); Enter in the custom input submits. Option labels are submitted and the daemon maps them to values.

## Acceptance criteria

- Options stack vertically and are easy to tap on mobile.
- Selections and custom text survive streaming output.
- In the Chat tab the card never pushes the composer off-screen.
- Every answer of a multi-question group reaches the agent.
- `make pre-commit` and `cargo test -p ertangle-daemon` pass.
