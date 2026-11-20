# T75 — Emoji-free animated thinking display with folded preview

Status: done

## Problem

Users could not tell whether an agent was still alive while it was thinking:

- The TUI defaults `show_thoughts` to `false` and dropped thought lines entirely, leaving a silent gap between the prompt and the first answer token.
- The web transcript collapsed thoughts into a static `思考過程 (N 行)` line with no hint of activity or content.
- `StreamBuf` buffered long single-paragraph reasoning indefinitely until a newline or kind change arrived, so nothing streamed for a long time.
- All of these displays used the `💭` emoji, which the user asked to remove.

## Spec

### 1. Adapter (`crates/ertangle-adapter/src/stream.rs`)

- `StreamBuf::decorate("think", line)` now prefixes with `⋯ ` (U+22EF MIDLINE HORIZONTAL ELLIPSIS plus space) instead of `💭 `.
- `StreamBuf::push` flushes an early line once a thought accumulates 120 characters without a newline. The split prefers the last whitespace near the limit so words stay intact, and consumes that whitespace so the next line never starts with a space. Unit tests cover both the char-boundary and word-boundary cases.

### 2. Shared recognition (`app::strip_thought_prefix`)

- `strip_thought_prefix(line)` recognizes both `⋯ ` and the legacy `💭 ` prefix, so old logs keep rendering correctly.
- `markdown.rs` renders either prefix with the emoji-free `· ` marker in the thought color (dark gray italic).

### 3. TUI

- With `show_thoughts == false`, consecutive thought lines fold into one line: `⋯ 思考 ({n} 行) · {last}` (or `⋯ 思考 ({n} 行)` when no non-empty preview exists). `preview` is the last thought in the run truncated to 60 characters with `…`. The folded line starts with `⋯ `, so it is styled as a thought automatically. Runs are flushed before any non-thought line, before active-tool lines, and at the end.
- `find_safe_boundary` no longer chooses a boundary between two consecutive thought lines, so incremental markdown caching cannot split a folded run.
- `toggle_thoughts` status text no longer mentions the emoji.
- `draw_sessions` shows a spinning Braille frame (green, like the busy status) instead of a static glyph for busy sessions. `lib.rs` redraws every 120ms while any session is busy.
- `draw_transcript` appends an animated `⠋ 思考中...` line (or `⠋ 思考中 · <preview>` when a thought tail is available) while the selected session is busy with no active tool and the view follows the latest output. When the viewport is full it replaces the last row. This stays in `draw_transcript` so `md_cache` and unit tests remain deterministic.

### 4. Web UI

- `groupTranscriptIntoTurns` recognizes both `⋯ ` and `💭 ` and strips either prefix.
- The collapsed thought `<details>` summary now uses an animated `<span class="think-ind">⋯</span>`, shows `思考中` on the last turn of a busy session (otherwise `思考過程`), the line count, and a `<span class="think-preview">· <latest thought></span>` preview.
- While a session is busy with no active tool, a live `<div class="chat-thinking-live">` row with animated dots and `思考中` is shown at the bottom of the chat transcript, even before the first token arrives.
- CSS keyframes animate the indicator and dots; no emoji remains in the thinking templates. String-level regression coverage lives in `web.rs::thinking_display_is_animated_and_emoji_free`.

## Verification

- `make i18n-extract` and `make i18n-check` with English translations in `locales/en.po`.
- `make pre-commit` (format, clippy, i18n).
- `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false cargo test --workspace`.
