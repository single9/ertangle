# T92 — Perf & safety: Web Markdown turn cache (`$` fix) and stack-allocated Flow elbows

Status: ready-for-agent

Priority: P2　Scope: `ertangle-daemon` (`web/index.html`), `ertangle-tui` (`flow/graph.rs`)　Depends on: T89

## Problem

1. **Web Markdown CPU & `$` replacement bug**: `renderChatTranscriptNow` in `crates/ertangle-daemon/src/web/index.html` re-runs ~20 regex passes on every turn on every event without caching, and `safe.replace(..., blockHtml)` in step 12 interprets `$` sequences (`$1`, `$&`, `$$`) inside code blocks as regex replacement patterns.
2. **Per-frame `Vec` allocations in Flow graph**: `elbow_points` in `crates/ertangle-tui/src/flow/graph.rs` allocates a heap `Vec` for every tree link, edge, and in-flight packet on every 60ms animation frame.

## Implementation

1. `crates/ertangle-daemon/src/web/index.html`:
   - In `renderMarkdown` step 12 (`codeBlocks`), use a function replacer `() => blockHtml` (and `() => html` in step 13/14) so `$` inside code/tables/links is never treated as a regex backreference.
   - Add a bounded LRU-like `Map` cache (`mdRenderCache`, max 400 entries keyed by `${activeSession}\0${src}`) in `renderSimpleMarkdown` / `renderMarkdown` (careful: only cache when `activeSession` matches or key includes `activeSession` since `fileUrl` embeds `activeSession`).
2. `crates/ertangle-tui/src/flow/graph.rs`:
   - Change `elbow_points(a, b)` to return a stack-allocated fixed array + length (e.g. `([(f64, f64); 4], usize)` or `struct Elbow { pts: [(f64, f64); 4], len: usize }`) with zero heap allocation per link/packet.
3. Update/verify unit tests in `crates/ertangle-tui/src/flow/graph.rs` and `crates/ertangle-daemon/src/web.rs`.

## Acceptance criteria

- Code blocks containing `$1`, `$&`, or `$$` render verbatim in Web UI Markdown.
- Repeated turns hit `mdRenderCache` instead of re-running regexes.
- `elbow_points` performs zero heap allocations per call.
- `make pre-commit` and `cargo test -p ertangle-tui --lib` pass.
