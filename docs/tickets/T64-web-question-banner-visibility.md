# T64 — Show the web question banner outside tab views

Priority: P1　Scope: `ertangle-daemon`　Depends on: T63

Status: done

## Problem

The pending-question banner is rendered inside `#view-chat`, which is hidden unless the Chat tab is active. With Overview active by default, a pending question produces no visible web banner even though the DOM contains the options. The user reported the result as broken text/layout.

## Analysis / decisions

- `#view-chat` uses `.tab-view { display: none; }` until `switchNavTab("chat")` adds `.active`.
- `renderState()` sets `urgentBanner.style.display = "block"`, but an ancestor with `display: none` still hides the banner completely.
- The review panel has its own question card, but it is also inside a hidden tab by default.
- Move the shared banner out of all tab views and make it a direct child of `<main>` so it is visible regardless of the selected tab.
- Make the banner sticky at the top of `<main>` so it stays visible while scrolling long overview or session lists.

## Implementation

- `ertangle-daemon/src/web/index.html`: relocate `#urgent-approval-banner` to `<main>` before the first `.tab-view`; add sticky positioning to `.urgent-banner`.
- `ertangle-daemon/src/web.rs`: add a regression test asserting the banner markup appears between `<main>` and the first `.tab-view`.

## Live verification

- Daemon rebuilt and restarted with the fixed binary; web UI loaded at the Tailscale address.
- With the Overview tab active, a pending question rendered the banner as a direct child of `<main>`: `position: sticky`, `top: 0`, bounding box `965 x 85.5` at `y = 50`.
- The compact banner showed the header, question text, and an `回答` button. Clicking it switched to the Chat tab and rendered the full question card with three numbered options and a custom-answer field; all option rows had non-zero width and height.
- `make pre-commit` and `cargo test -p ertangle-daemon question_banner_is_outside_tab_views` pass.

## Acceptance criteria

- A pending question is visible on the web UI while the Overview tab is active.
- The banner remains visible while scrolling within `<main>`.
- Answering a question from any tab hides the banner and reaches the originating session.
- `make pre-commit` and `cargo test -p ertangle-daemon` pass.
