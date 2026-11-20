# T66 — Make the TUI question picker usable

Priority: P1　Scope: `ertangle-tui`　Depends on: T63

Status: done

## Problem

The TUI picker for answering an agent's interactive question was wrong and hard to use:

- Header, full question and key hints were crammed into one title line, truncating long questions; option descriptions were inlined as `label (desc)`.
- Typed text was both the filter and the custom answer: a custom answer partially matching an option was swallowed; in multi-select Space toggled, so custom text could not contain spaces.
- Enter in single-select preferred the cursor option over typed text; multi-select ignored custom text once anything was checked; free text was accepted even when `custom == false`.
- `App::selected_question` picked a question in HashMap order, and only one question of a `group` (which must be answered together) could be handled.

## Decisions / implementation

- Multi-line layout: bold header + full question wrapped to width + hint line; descriptions shown dim under each option; selected row highlighted; `[x]` / `[ ]` for multi-select; options numbered 1-9.
- No type-to-filter for questions. Only when `q.custom` is true, a final "✎ custom answer…" row enters a text mode where all input (including spaces) is the answer; Esc returns to the options.
- Digits 1-9: single-select submits that option, multi-select toggles it; out-of-range is ignored.
- Enter: single-select submits the cursor option; multi-select submits the checked set (option order) plus custom text; with nothing checked it submits the cursor option.
- Esc does not answer; the question stays pending.
- Grouped questions are walked in id order with a `(1/N)` title, then one `AnswerQuestion` is sent per question. `s` (or empty Enter on a free-form-only question) skips with `answers: []`; if every question was skipped, the last one is refused and the picker stays open.
- `selected_question` sorts by id, so the choice is deterministic.

## Acceptance criteria

- Custom answers with spaces can be sent and are not swallowed by option filtering.
- With `custom == false` there is no custom row and no free text.
- Grouped questions are sent only after walking the whole group; optional free-form fields can be skipped.
- `make pre-commit` and `cargo test -p ertangle-tui` pass.
