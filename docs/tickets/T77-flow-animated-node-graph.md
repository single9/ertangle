# T77 — Animated node graph for the Flow view

Status: ready-for-agent

Priority: P3　Scope: `ertangle-tui`　Depends on: T76

## Problem

T76 shipped the Flow view as a text node board plus an edge list. It is correct but reads like a table; the user wants the dashboard to feel alive: see the session tree as a graph and watch messages travel.

## Design

- **Graph mode is the default** when it fits; the T76 text layout stays as the fallback (too many siblings, too short) and can be forced with `l` inside Flow (`App.flow_list`).
- **Deterministic tidy-tree layout** (`flow/graph.rs::layout`): parent links are rebuilt from the tree-ordered `FlowModel::nodes` depths; each subtree gets horizontal space proportional to its leaf count and the node is centered over it; one row band per tree level. Boxes are 4 rows tall, 10–24 columns wide, with at least 1 row between levels (so a 3-level tree fits 80×24). No physics, no jitter.
- **Rendering**: a Braille `Canvas` underneath draws tree links (dim), message edges with traffic in the window (colored by `MsgKind`), and dependency edges (red, dashed). Node boxes are drawn on top: rounded border colored by status, double border for the cursor, label line plus badge line.
- **Animation**:
  - A packet `◆` with a short Braille trail travels from sender to receiver for `FLOW_PACKET_MS` (1.5 s) after a message's `ts`; broadcast sends one packet to every other node. Channel messages have no geometric target and only appear in the ribbon.
  - Busy nodes "breathe": the border cycles through green shades; `WaitingApproval` blinks yellow.
  - The event loop redraws every 60 ms while the Flow view is open and a packet is in flight (cheap check on the newest message), otherwise the existing 120 ms busy / 1 s idle cadence applies.
- **Ribbon**: up to 4 newest edges below the graph (T76 `edge_lines`), so channel/broadcast/preview text is still visible.
- Everything is computed from `FlowModel::now_ms`, so frames are deterministic in tests.

## Acceptance criteria

- With a coordinator and workers, `F` shows boxes connected parent→child; `l` switches to the T76 list and back.
- A new message animates a packet from sender to receiver box within ~1.5 s, then stops redrawing at 60 ms.
- Busy nodes visibly pulse; the cursor node has a double border; `j`/`k`/`Enter`/`Esc` keep T76 behavior.
- Falls back to the list automatically when the tree does not fit; never panics at any size.
- Tests: layout (centering, no overlap, fit/no-fit), packet visible mid-flight and gone after `FLOW_PACKET_MS`, fallback, `l` toggle. `make pre-commit` and `cargo test -p ertangle-tui` pass.

## Verification

Live tmux check like T76; record in `docs/VERIFICATION.md`.
