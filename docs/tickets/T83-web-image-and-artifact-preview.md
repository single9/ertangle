# T83 — Web UI: Markdown image syntax and agent artifact file preview

Priority: P1　Scope: `ertangle-daemon` (`web.rs`, `web/index.html`)　Depends on: —

## Problem

Users cannot view or open image files (such as screenshots or agent-generated artifacts) in the Web UI:

1. `renderMarkdown` in `crates/ertangle-daemon/src/web/index.html` only matches `[label](url)` and does not handle `![alt](url)` image syntax, leaving a literal `!` before a file pill instead of rendering/previewing the image.
2. `/api/file` in `crates/ertangle-daemon/src/web.rs` restricts `allowed_roots` strictly to `default_cwd`, `s.cwd`, and `s.repo_root`. Agent artifacts and screenshots stored in agent data directories (e.g. `~/.gemini/antigravity-cli/brain`, `~/.ertangle`, `~/.claude`, `~/.codex`) are rejected with 403 Forbidden.

## Evidence

- `crates/ertangle-daemon/src/web/index.html:3036-3048` (`renderMarkdown` link regex).
- `crates/ertangle-daemon/src/web.rs:393-455` (`handle_file_request` `allowed_roots` check).

## Implementation

1. `crates/ertangle-daemon/src/web/index.html` (`renderMarkdown`):
   - Parse `![alt](url)` before `[label](url)`:
     - For `file://...` or relative/absolute local image paths, render an inline clickable image preview (e.g. `<img src="/api/file?path=..." ...>` wrapped in a clickable container that opens `openFileActionSheet` for full-size view/download, with an `onerror` fallback).
     - For `http://` / `https://` image URLs, render `<img src="..." alt="..." ...>` safely (escaped).
2. `crates/ertangle-daemon/src/web.rs` (`handle_file_request`):
   - Extend `allowed_roots` to also include known agent artifact/state directories under `$HOME` (when they exist and canonicalize): `.gemini/antigravity-cli/brain`, `.ertangle`, `.claude`, `.codex`.
   - Keep canonicalization + `starts_with` prefix checks intact so path traversal (`..`) and arbitrary system paths remain strictly blocked.
3. Add unit tests in `crates/ertangle-daemon/src/web.rs` verifying `INDEX_HTML` handles `![` image syntax and `handle_file_request` / path validation allows artifact directories while rejecting out-of-root paths.

## Acceptance criteria

- `![alt](file:///.../shot.png)` renders as an inline image preview in the Web UI and can be clicked to inspect/download.
- `/api/file` serves files inside `~/.gemini/antigravity-cli/brain/...` (and other allowed artifact dirs) while still blocking `/etc/passwd` and `..` traversal.
- `make pre-commit` and `cargo test -p ertangle-daemon --lib` (with `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false`) pass.
