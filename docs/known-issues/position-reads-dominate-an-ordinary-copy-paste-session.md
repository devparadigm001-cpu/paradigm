# Position reads dominate an ordinary copy-paste session on a non-spreadsheet page

**Status:** open, measured in real use. Found 2026-08-21.
**Severity: MEDIUM.** No event loss has been observed — two consecutive sessions
reported `0 LOST` — but a single recording spent **10.5s and 14.8s** inside
position reads, the cost scales with the number of fields transferred, and it is
paid on the ordinary `Ctrl+C`/`Ctrl+V` path that every transfer uses.
**Where:** `capture::grid::read_position` → `read_element_position`, reached from
`observe_key` on every `Ctrl+C` and `Ctrl+V`.
**Distinct from** `a-source-mark-costs-half-a-second-off-the-spreadsheet-path.md`.
That records the same read costing ~432ms per `Ctrl+Shift+M`, which is a rare
deliberate keypress. This is the same read on the path used by every copy and
every paste, so the cost multiplies by the size of the task rather than by how
often someone chooses to press a marker.

## The measurement

Two sessions on the ChatGPT pricing page, three tiers into three spreadsheet
rows, nine fields each:

```
session A: grid sampling: key-down 60 calls, 172052 us mean; key-up 57 calls, 4189 us mean; total 117 calls, 10561971 us
session B: grid sampling: key-down 72 calls, 202087 us mean; key-up 72 calls, 3076 us mean; total 144 calls, 14771776 us
```

**202ms per key-down. 14.8 seconds in one recording.**

For contrast, the same counter measured **12,089us** — 12ms — on a Google Sheets
recording after the 2026-08-18 optimisation. This is roughly seventeen times
that, on the same code, because a different surface takes a different branch.

The counters confirm where it went:

```
off-key position reads: clipboard 0 calls, 0 us mean; mark 0 calls, 0 us mean
```

Zero on both off-key paths, so every read happened inside `observe_key`, on the
clipboard keystrokes, and is therefore inside the key-down figure.

The arithmetic agrees. Roughly 30 clipboard keys among 72 key-downs at ~432ms
each, with the remaining ~42 at ~12ms, predicts a mean near 187ms; measured 202ms. Nothing unexplained.

## Why it is expensive here and cheap on a spreadsheet

`read_position` branches on what the surface is:

* a **spreadsheet** resolves through the Name Box and returns early — a single
  walk that stops as soon as it has the cell and the document id, documented at
  ~50ms;
* **anything else** falls through to `read_element_position`, which performs
  **three** separate traversals, each with its own 3000-node budget:
  `collect_position` failing to find a Name Box (no early exit, because it never
  finds both things it stops for), `collect_page_identity` hunting a URL, and
  `collect_nodes` building the document-order node list with three UI Automation
  property reads per node and no early exit at all.

Google Sheets takes the first branch. Every other page takes the second. So the
cost is not a property of the recording's size alone — it is a property of the
surface, and the surface the generalisation work exists to support is precisely
the expensive one.

## Why it has not caused loss yet, and why that is not reassurance

Both sessions reported `event census: 0 LOST` — 732/732 and 915/915. The reads
happen on the capture pump, which is downstream of the user's input, so nothing
here delays typing.

What it does consume is pump throughput and the `grid` mutex, which is held for
the duration of each read. The pump reads a `tokio::broadcast` of capacity 1000
and a receiver that falls behind loses events silently. At the rates measured
there is headroom. A faster typist, a busier page, or a longer session eats it.

The honest statement is that this survives at the scale measured, not that it is
safe at every scale — and the same census that proves the first cannot prove the
second.

## What a fix must not do

**It must not cache the tree between reads.** The page changes between copies —
that is the entire point of copying several records — and a stale node list
attributes a position to the wrong record, which is the silent-wrong-target class
this project keeps finding.

**It must not lower the 3000-node budget.** A truncated walk sets
`WindowScan::truncated`, which routes to `PositionRead::Inconclusive` and returns
no position at all. Cheaper and wrong, and now worse than before: since
2026-08-21 a failed read correctly clears `pending_source`, so a truncated walk
turns a real link into no link.

**It must not move the read off the keypress.** The position must be read while
the user is still on the source; resolving later resolves against wherever they
went next.

The obvious direction is to **share one traversal**. `collect_position` and
`collect_nodes` visit the same nodes for different reasons, and
`identity::tree::locate` and `records` already share a single walk for exactly
this reason — the precedent is in the codebase. Whether `page_identity` can join
them depends on whether the URL is reachable in the same pass.

A second, smaller change is to hold the `grid` mutex only for the
`pending_source` update rather than across the whole read; the read needs the
`Desktop` handle, not the watcher.

## Reproducing

1. Start Record Mode. Open any page that is not a spreadsheet.
2. Copy and paste several fields into a sheet, using `Ctrl+C` and `Ctrl+V`.
3. Stop, and read `paradigm-dev10.log`:

```
[paradigm] grid sampling: key-down N calls, M us mean; ...
[paradigm] off-key position reads: clipboard N calls, ...; mark N calls, ...
```

A key-down mean in the hundreds of milliseconds with zeros on both off-key
counters is this issue. The same recording made against Google Sheets as the
source will show roughly 12ms instead.

## Related

* `docs/known-issues/a-source-mark-costs-half-a-second-off-the-spreadsheet-path.md`
  — the same read measured at ~432ms on the marker path, with the three-walk
  breakdown and the lock-contention note.
* `docs/known-issues/an-element-identity-mark-records-no-field-label.md` — what
  that expensive walk returns, and why it is often unlabelled.

## The scale arrived, 2026-08-22

This doc closed by saying the cost "survives at the scale measured, not that it
is safe at every scale — and the same census that proves the first cannot prove
the second."

Both halves were confirmed on the same recording. A Wikipedia article, 3365
named elements in its tree, produced **1297ms per key-down** — six times the
202ms recorded here — and slightly over half the session's events were lost.

The second half held too: the census reported 295 lost, and the arithmetic shows
even that number is a lower bound, because the census itself appears to have
been starved by the same synchronous blocking. See
`a-large-page-starves-the-pump-and-loses-half-the-recording.md`.

The "share one traversal" direction proposed above is now the fix for a
recording that fails outright, not an optimisation.

## The "share one traversal" direction was wrong, measured 2026-08-22

This doc proposed sharing one traversal, on the reasoning that
`collect_position`, `collect_page_identity` and `collect_nodes` visit the same
nodes for different reasons. That was read off the code and it is wrong.

Measured with `examples/read_cost_probe.rs`: the two extra walks visit **90
nodes and cost 85ms**, because both have a `depth > 14` limit and both
early-exit. Merging them saves **4%**. The cost is `collect_nodes`, and a third
of that is the bare `children()` traversal with no property reads at all.

The fix that worked was a **time bound** on the walk, returning no position
rather than a slow one. See
`a-large-page-starves-the-pump-and-loses-half-the-recording.md`.

The merge was done anyway, because the address bar's URL can be captured on the
same visit that reads the document id — same element, same `text(0)`. That part
is free. It is not what made the difference.
