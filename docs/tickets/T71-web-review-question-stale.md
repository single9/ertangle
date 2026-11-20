# T71 — Web: question card stays in Review after answering

Status: done

## Problem

On mobile Safari, after answering an `ertangle_question` from the web, the card stayed in the Review tab even though the daemon had emitted `question_resolved`.

## Root cause

The T65 rework of the banner block in `updateAllViews()` (`web/index.html`) dropped the `const pendingKeys = ...` declaration but kept using it in the `else if (pendingKeys.length > 0)` branch. When no question is pending, evaluation reaches that branch and throws `ReferenceError: pendingKeys is not defined`. `ws.onmessage` wraps `handleFrame` in a try/catch that only logs "Frame parse error", so the exception was swallowed and the view update aborted before the "nothing pending" branch could reset `#review-approval-slot`. The state itself was correct (the question was deleted); only the render was skipped. Permission requests were broken the same way.

## Repro

Throwaway Playwright script (outside the repo) with a fake `WebSocket`: feed `snapshot` -> `question_requested` -> `question_resolved` and read `#review-approval-slot`. Before the fix the console shows the ReferenceError and the slot keeps the question card (`sig=review:q1:1`); after the fix it shows the empty text (`sig=none`) from both the chat and review tabs. The reconnect/snapshot path goes through the same `updateAllViews()` and was affected identically.

## Fix

Declare `const pendingKeys = inScopeFirst(state.pending_permissions);` next to the question key computation. Regression test `index_html_declares_pending_keys_before_use` in `web.rs`.
