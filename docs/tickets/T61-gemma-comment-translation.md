# T61 — Translate legacy code comments to English with gemma4:std

Priority: P3　Scope: all Rust crates and `src/`　Depends on: T60

Status: needs-triage

## Problem

Legacy inline comments and Rustdoc are mostly Traditional Chinese. Agents that read the code spend extra tokens decoding them and mix languages in their reasoning. The user approved drafting the translations with the local `opencode` + `gemma4:std` model to improve readability and reduce token usage.

## Evidence

- `crates/ertangle-tui/src/menu.rs:531,539,553,567,573,739,753,770`.
- `crates/ertangle-daemon/src/handlers/session.rs:1,28,40,71,89`.
- `crates/ertangle-core/src/lib.rs:342,345,350,354,360,372`.
- `src/cmd/session.rs` doc comments around `cmd_resume`.
- T60 records the English-first convention this ticket applies retroactively.

## Analysis / decisions

- Use the `local-llm-delegate` skill: run `opencode` with `gemma4:std` in its own worktree and give it one small batch of files at a time with an exact file list.
- The worker may translate only comments and Rustdoc. It must not change code, string literals, `tr!` msgids, identifiers, or formatting. `tr!` msgids belong to the i18n workflow (T27) and are explicitly out of scope.
- Proposed batches, ordered by how often agents read the files: (1) `crates/ertangle-core/src/lib.rs`, (2) `crates/ertangle-daemon/src/handlers/session.rs` and `config.rs`, (3) `crates/ertangle-tui/src/{menu,app,commands,draw}.rs`, (4) remaining crates and `src/`.
- Treat model output as a draft. The coordinator reviews each diff before merging: no logic changes, no touched `tr!` call, terminology consistent with `CONTEXT.md`, and the translated comment still matches what the code does.
- Do not touch markdown docs, locale catalogs, or user-facing strings. Not confirmed: whether `gemma4:std` is available in this environment; verify before starting and fall back to manual translation for any batch the local model cannot finish.

## Implementation

- For each batch, spawn the local-model worker with the file list and the comments-only rule, review its diff, and merge the worktree branch.
- Run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` after each batch. Comment-only edits should be no-ops, but the checks guard against accidental code changes.

## Acceptance criteria

- All inline comments and Rustdoc in the agreed batch files are English; no Chinese comment remains in those files.
- Every diff is comment-only: no changed string literal, `tr!` msgid, or executable line.
- Terminology follows `CONTEXT.md` and the translated comments remain accurate.
- `make pre-commit` passes after each batch.
- Translation of Traditional Chinese documentation and UI strings is explicitly out of scope.
