# A source mark costs ~432ms off the spreadsheet path, and holds the grid lock

**Status:** open, measured. Found 2026-08-19 in the first live test of the
`Ctrl+Shift+M` source marker, using instrumentation added the same day for
exactly this purpose.
**Severity: MEDIUM.** No failure has been observed — the session it was measured
in reported `0 LOST` — but a third of a second of blocking work per mark, taken
while holding a lock the capture pump needs, is a real hazard under heavier
input rather than a theoretical one.
**Where:** `capture::grid::read_position` → `read_element_position`, reached
from `note_marked_source` and `note_clipboard_copy`.

## The measurement

```
[paradigm] off-key position reads: clipboard 0 calls, 0 us mean; mark 3 calls, 312812 us mean
```

Three marks, **312.8ms mean**, 0.94s in total. One of the three was a
spreadsheet, read through the Name Box, which is documented at roughly 50ms.
Backing that out leaves the two element-identity reads at **~432ms each**.

Those numbers exist only because `note_clipboard_copy` and `note_marked_source`
were wrapped in their own counters that day. `GRID_CALLS`/`GRID_MICROS` wrap
`observe_key` alone, so before that both paths were invisible: the menu-copy
session on the same date performed one of these reads and it appears in none of
that session's figures.

## Why it costs that much: three walks, not one

An element-identity mark walks the window tree **three separate times**, each
with its own 3000-node budget:

| | where | what it does | early exit? |
|---|---|---|---|
| 1 | `read_position` → `collect_position` (`grid.rs:570`) | decides spreadsheet vs not, by looking for a Name Box and an address bar | stops once it has **both** — so on a non-spreadsheet it finds neither and runs to completion |
| 2 | `read_element_position` → `page_identity` → `collect_page_identity` (`grid.rs:910`) | finds a URL | stops on the first URL — but a window with no URL runs to completion, then falls back to the title |
| 3 | `read_element_position` → `collect_nodes` (`grid.rs:615`) | builds the document-order node list for `locate` | **never** — and reads three properties per node (`id`, `role`, `name`) |

So the worst case, a titled window with no URL and no Name Box, is three full
traversals of the same tree, one of them at three UI Automation round trips per
node. Nothing is cached between them, and walks 1 and 3 collect overlapping
information from the same nodes.

## What this validates

It settles a decision taken earlier the same day. `CapturedAction` records the
acted-on element's **bounds** — one UI Automation call — rather than a true
document-order ordinal, precisely because the ordinal would have required this
walk *per action*. At 20–50 actions in a session that is 10–20 seconds of pump
time.

That trade-off was argued from an estimate at the time. It is now measured.

## The contention, which is the part that could actually bite

`CaptureSession::mark_source` takes the `grid` mutex and holds it for the whole
read:

```rust
self.grid.lock().map(|mut g| g.note_marked_source(timestamp_ms))
```

The capture pump locks the **same** mutex in `observe_grid`, for every event it
processes. So a mark can stall the pump for the duration of the read.

The pump consumes a `tokio::broadcast` of capacity 1000, and a receiver that
falls behind loses events silently — the defect that
`CaptureReport::events_lost` was built to expose. A ~432ms stall does not
approach that on its own; a burst of marks during heavy input might.

**No loss has been observed.** The session that produced these numbers reported
`148 emitted, 148 handled, 0 LOST` despite 0.94s of blocking reads. That is
evidence it is survivable at this scale, not evidence it is safe at every scale.

Also worth noting: the mark runs on the global-shortcut handler thread, so a
third of a second passes before the user gets any acknowledgement — and there is
no on-screen acknowledgement at all today, only a log line.

## What a fix must not do

**It must not skip the walk by caching the tree across marks.** The page changes
between marks — that is the entire point of marking several records — and a
stale node list would attribute a mark to the wrong record, which is the
silent-wrong-target failure this project keeps finding.

**It must not drop the budget below what a real page needs.** A truncated walk
sets `WindowScan::truncated`, which routes to `PositionRead::Inconclusive` and
returns no position at all. Cheaper and wrong.

**It must not move the read off the keypress.** Resolving later means resolving
against whatever is focused later; the read is synchronous at the press
deliberately, and that is why the marker path does not round-trip through the
frontend the way the Record Mode shortcut does.

The obvious direction is to **share one traversal**. Walks 1 and 3 visit the
same nodes for different reasons, and `identity::tree::locate` and `records`
already share a single walk for exactly this reason — the precedent exists in
the codebase. Whether `page_identity` can join them depends on whether the URL
is findable in the same pass.

Reducing the lock hold is a separate and smaller change: the read needs the
`Desktop` handle, not the whole watcher, so the expensive part could happen
outside the lock and only the `pending_source` update inside it.

## Reproducing

1. Start Record Mode.
2. Press `Ctrl+Shift+M` on a spreadsheet cell, then on a page with no Name Box.
3. Stop, and read the `off-key position reads` line in `paradigm-dev10.log`.

The `mark` mean across a mixed set understates the element-identity case; press
only on non-spreadsheet surfaces to measure that path alone.

## Related

* `docs/known-issues/an-element-identity-mark-records-no-field-label.md` — the
  other finding from the same session, including the honest note that a *failed*
  mark has never been observed live.
* `docs/known-issues/element-id-is-a-hash-of-the-text.md` — what `collect_nodes`
  is gathering, and why the ids it collects are weaker than they look.
