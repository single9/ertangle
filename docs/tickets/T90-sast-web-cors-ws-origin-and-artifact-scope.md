# T90 — SAST: restrict CORS, enforce WebSocket Origin check, and tighten artifact file scope

Priority: P1　Scope: `ertangle-daemon` (`web.rs`)　Depends on: T83

## Problem

SAST audit identified three web server security issues in `crates/ertangle-daemon/src/web.rs`:

1. **Permissive CORS (`High`)**: `/file` and `/api/file` responses include `Access-Control-Allow-Origin: *`, allowing any third-party origin to read workspace and agent files if reachable.
2. **Missing WebSocket `Origin` check (`High`)**: `/ws` upgrade does not validate the `Origin` header against the request `Host` header, exposing local/unauthenticated sessions to Cross-Site WebSocket Hijacking (CSWSH).
3. **Over-broad artifact `allowed_roots` (`Medium`)**: T83 added `~/.ertangle`, `~/.claude`, and `~/.codex` directly to `allowed_roots`, which would allow reading sensitive config/credential files like `~/.ertangle/config.toml`.

## Implementation

1. `crates/ertangle-daemon/src/web.rs`:
   - Remove `Access-Control-Allow-Origin: *` from `/file` / `/api/file` responses (rely on same-origin).
   - Add `is_allowed_ws_origin(origin: Option<&str>, host: Option<&str>) -> bool`:
     - If `Origin` header is absent (non-browser CLI/native clients), allow.
     - If `Origin` header is present, parse its host:port and require it to match `Host` (or `localhost` / `127.0.0.1` / `[::1]` when `Host` is loopback). Reject mismatched cross-site origins with `403 Forbidden`.
   - Tighten agent home directory access in `serve_file`:
     - Keep `.gemini/antigravity-cli/brain` allowed for artifact/image files.
     - For `.ertangle`, `.claude`, and `.codex`, only allow image extensions (`png`, `jpg`, `jpeg`, `gif`, `webp`, `svg`, `ico`, `bmp`, `avif`) so `config.toml`, tokens, and JSON credentials cannot be served via `/file`.
2. Add unit tests in `crates/ertangle-daemon/src/web.rs` covering CORS header removal, `is_allowed_ws_origin` (same-origin, missing origin, cross-origin rejection), and blocking `~/.ertangle/config.toml` while allowing `*.png`.

## Acceptance criteria

- `/file` and `/api/file` no longer send `Access-Control-Allow-Origin: *`.
- `/ws` rejects cross-site `Origin` headers with 403.
- `~/.ertangle/config.toml` is rejected by `serve_file` even when `~/.ertangle` exists; image files in artifact dirs remain readable.
- `make pre-commit` and `cargo test -p ertangle-daemon --lib web::tests` pass.
