# T68 — Send images with prompts from the web UI

Priority: P2　Scope: `ertangle-daemon`　Depends on: T65

Status: done

## Problem

The web composer always sent `attachments: []`, although the `prompt` ws op (`Request::Prompt`) already accepts `Attachment { name, mime, data (base64), size }` and the daemon inlines images for the agent. Web users therefore could not send screenshots or photos.

## Implementation

- `ertangle-daemon/src/web/index.html` only (no daemon change).
- Composer: attach button with a hidden `<input type="file" accept="image/png,image/jpeg,image/gif,image/webp" multiple>`, paste of images into the textarea, and drag-and-drop onto the chat view.
- Thumbnail chips above the textarea (name, size, × remove). Unsupported types and images still above 5 MB (`MAX_IMAGE_BYTES`) are rejected with a toast.
- Client-side downscale: when the longest side exceeds 2000 px or the file exceeds 5 MB, re-encode through a canvas to JPEG (quality 0.85); otherwise the original bytes are kept.
- `sendPrompt()` may send images with empty text; base64 is read via `FileReader` with the `data:` prefix stripped, `size` is the decoded byte length, pasted images are named `pasted-<ts>.png`. Chips are cleared after sending.
- Transcript: the daemon's attachment summary on the user turn is relied upon; no local echo was added.

## Acceptance criteria

- Images can be attached via button, paste, and drag-and-drop, previewed, removed, and sent with or without text.
- Oversized or unsupported files are rejected with a toast; large images are downscaled.
- `make pre-commit` and `cargo test -p ertangle-daemon` pass.
