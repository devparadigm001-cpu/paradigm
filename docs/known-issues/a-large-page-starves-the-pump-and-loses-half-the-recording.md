# A large page starves the pump, and half the recording is lost

**Status:** FIXED 2026-08-22, hours after it was found. Kept because the
wrong diagnosis that preceded the fix is the instructive part, and because the
fix trades loss for a documented silence rather than removing the cost.
**Severity: HIGH.** Slightly over half of one recording never reached capture.
The result was 8 captured actions, 0 source links and 0 candidates from a
session in which the user transferred several rows. **A whole class of real page
cannot currently be recorded.**
**Where:** `capture::grid::read_element_position`, reached from `observe_key` on
every `Ctrl+C` and `Ctrl+V`, running synchronously inside the async pump.

## What was reported

```
[paradigm] grid sampling: key-down 26 calls, 1297233 us mean; key-up 26 calls, 3467 us mean
[paradigm] event census: 584 emitted, 289 handled, 295 LOST -- this recording is incomplete
[paradigm] confirmation candidates: 0 from 8 action(s)
[paradigm] source links: 0 pair(s), 0 distinct source position(s)
```

**1.297 SECONDS per key-down.** For comparison, on the same code:

| surface | key-down mean | named elements in the tree |
|---|---|---|
| Google Sheets | 12ms | ~56 (whole window) |
| OrderFlow dashboard | ~150ms | 46 |
| ChatGPT pricing | 202–274ms | — |
| **Wikipedia article** | **1297ms** | **3365** |

The Wikipedia tree was measured at **3365 named elements** by
`page_bounds_probe` *before* the recording, while reconnoitring the table. That
number is the cause. `read_element_position` performs three traversals, each
with its own 3000-node budget and three UI Automation property reads per node,
and on a page this size the budget is spent in full rather than exiting early.

26 key-downs at 1.297s is **33.8 seconds** during which the pump was inside
synchronous UI Automation calls.

## It is the documented Lagged path

`terminator-workflow-recorder` uses `broadcast::channel(1000)`
(`recorder.rs:310`), and its stream handles a lagging receiver by logging and
carrying on:

```rust
Err(RecvError::Lagged(skipped)) => {
    tracing::error!("⚠️ Event stream LAGGED! Skipped {} events ...");
    continue;
}
```

`tracing::error!` with no subscriber in this process, then `continue`. Silent.
This is the path `capture`'s census was built to expose, and 2026-08-22 is the
first time it has fired on real data. **The mechanism worked**: the loss was
reported at stop and shown to the user rather than discovered later as missing
steps.

## The arithmetic does not close, and that is a second finding

Lag can only drop messages once a receiver falls further behind than the channel
retains. Capacity is 1000. The census reports **584 emitted**. A receiver cannot
fall 1000 behind in a stream of 584, so on those numbers no eviction is possible
— and yet 295 events are provably missing.

The only reading that fits is that **the census undercounted too**: true
emissions were higher, and the "fast enough that it cannot lag" subscriber
lagged as well.

There is a mechanism for that. `observe_grid` does **synchronous** UI Automation
work inside an async task. A 1.3-second synchronous block does not yield, so it
starves the runtime worker it occupies; sustained across 26 keystrokes it can
stall the census task too, however little work that task does.

This is inference from arithmetic, not a measurement, and it is recorded as
such. What it means practically is stronger than the inference is precise:
**`events_lost` is a lower bound, not a count.** The honest reading of "295
LOST" is "at least 295".

## Consequences seen in the same recording

Every copy declined:

```
[paradigm] copy source: (none -- declined)   x7
```

That is not a second defect. `click_source_id` needs a click within five
seconds, and the `Click` events that would have armed it were among those
dropped — only 6 clicked element ids were read all session. The decline is the
last-click mechanism behaving correctly on a stream with holes in it, which is
the right failure mode: no link rather than a wrong one.

## What a fix must not do

**It must not lower the 3000-node budget.** A truncated walk sets
`WindowScan::truncated` and returns no position at all, so it trades a slow
correct answer for a fast absent one.

**It must not move the read off the pump without keeping it on the keystroke.**
The position has to be read while the user is still on the source.

The directions that remain are the ones
`position-reads-dominate-an-ordinary-copy-paste-session.md` already names —
share one traversal instead of three, and hold the `grid` mutex only for the
`pending_source` update — plus one this recording adds: **the synchronous read
should not run on a runtime worker**, because blocking there is what took the
census down with it.

## Reproducing

1. Open a large Wikipedia article. Confirm the tree size first:
   `cargo run --example page_bounds_probe -- <title-substring>` and read the
   `named elements with bounds` line. 3365 reproduced this.
2. Record a few copy-pastes from it into a spreadsheet.
3. Stop and read the log. A key-down mean above a second, with a non-zero
   `LOST`, is this issue.

## Related

* `docs/known-issues/position-reads-dominate-an-ordinary-copy-paste-session.md`
  — the same read measured at 202ms, with the explicit note that it "survives at
  the scale measured, not that it is safe at every scale". This is that scale
  arriving.

## The first diagnosis was wrong, and measurement is what caught it

This doc originally proposed sharing one traversal instead of three, following
`position-reads-dominate-an-ordinary-copy-paste-session.md`. That direction was
read off the code and it does not survive measurement.

`examples/read_cost_probe.rs`, against the real Wikipedia window:

```
  structure only (children)           3000 nodes   1163ms
  + name()  (x2: position, page id)     90 nodes     85ms
  + role() + name()  (no id)          3000 nodes   2450ms
  + id() + role() + name()  (nodes)   3000 nodes   3828ms

  saving from merging the walks:  169ms  (4%)
```

**The two "extra" walks visit 90 nodes and cost 85ms.** Both carry a
`depth > 14` limit and both early-exit as soon as they find what they want, so
neither was ever the problem. Merging them saves 4%, which would have taken
1297ms per key-down to roughly 1240ms and lost the recording anyway.

The cost is the third walk, and it is roughly thirds: **1163ms is the bare
traversal** — `children()` across 3000 nodes with no property reads at all —
with role+name adding ~1.3s and `id()` another ~1.4s. Three walks was the shape
of the code, not the shape of the cost.

## The fix: a time bound, not a node bound

`collect_nodes` now stops on **either** 3000 nodes or
[`WALK_TIME_BUDGET_MS`] = 400ms, and a walk stopped by the clock returns **no
position at all**.

A partial node list must never be used. `identity::tree` separates structural
from content by multiplicity, so a tree cut short reports repeated elements as
occurring once, reclassifies them as content, and yields a position that is
wrong rather than absent.

The threshold comes from measuring the walk itself on each surface, not from the
session means:

| window | nodes | full walk |
|---|---|---|
| OrderFlow dashboard | 78 | 113ms |
| Wikipedia article | 3000 (budget spent) | 3828ms |

The page-identity walk was also folded into the position walk, since the URL
comes from the same element, on the same `text(0)`, that the document id already
reads. That part is genuinely free — no extra node visited, early-exit condition
untouched — but it is a tidying, not the fix.

## The evidence, same page, before and after

```
before   grid sampling: key-down 26 calls, 1297233 us mean
         event census: 584 emitted, 289 handled, 295 LOST

after    grid sampling: key-down 18 calls,  194657 us mean
         event census: 101 emitted, 101 handled, 0 LOST
```

**6.7x faster per key-down, and zero events lost**, on the same Wikipedia
article. Total time inside position reads fell from 33.8s to 3.5s.

## What was bought, and what was paid

Bought: the recording survives. Paid: on a page this large the source position
is usually **absent**. Seven of nine copies in the verifying run reported
`(none -- declined)`.

Two of the nine did resolve, which is worth stating plainly: the walk sometimes
finishes inside 400ms on the same page, presumably as UI Automation warms. So
the outcome is **non-deterministic on a page near the threshold** — a link
sometimes, no link other times. Each individual answer is sound, because a
timed-out walk returns nothing rather than a partial tree, but the same gesture
twice may not produce the same record.

That is a real limitation and it is not resolved by tuning the number. It is
resolved by making the walk cheap enough that large pages finish, which needs an
API that reads properties in bulk — UI Automation cache requests — and
`terminator-rs` exposes none.

## Related

* `docs/known-issues/position-reads-dominate-an-ordinary-copy-paste-session.md`
  — where the "share one traversal" direction came from, now corrected there too.
