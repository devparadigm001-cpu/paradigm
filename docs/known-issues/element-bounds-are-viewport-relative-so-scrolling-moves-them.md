# Element bounds are viewport-relative, so scrolling gives one element two positions

**Status:** open, measured. Found 2026-08-21 in session record-f15e933d.
**Severity: HIGH for anything positional.** `element_bounds` was introduced as a
*positional identity* — the thing that distinguishes two elements saying the same
text. A value that changes when the user scrolls is not an identity.
**Where:** `capture::bounds_of` → `terminator::UIElement::bounds`, which returns
`rect.get_left(), get_top(), get_width(), get_height()` — screen coordinates of
what is currently displayed.

## The measurement

One recording, one element, two positions:

```
step 18  click  x136 y 462  w54   "$0"
step 23  click  x136 y 462  w54   "$0"
step 24  click  x136 y 462  w54   "$0"
step 28  click  x136 y-138  w54   "$0"
```

Same name, same width, same x. The y moved 600px because the page scrolled
between step 24 and step 28, and a negative y means the element had gone above
the top of the viewport.

Nothing about the element changed. Only the view did.

## Why this is worse than a tolerance problem

The 2-D clustering in `row_clustering` derives a record pitch from y values and
assigns `record = floor((y - y_min) / pitch)`. Feed it two positions for one
element and it places that element in two records. A scroll of one record's
pitch is enough to shift every subsequent element by a whole record.

It also undermines the reason bounds were chosen over the alternatives. The
2026-08-18 decision picked bounds *because* they were cheap and stable enough to
distinguish two elements with identical text
(`element-id-is-a-hash-of-the-text.md`). Stability across the recording was the
premise, and it does not hold.

## What is actually available, checked rather than assumed

* `terminator-rs` exposes **no scroll-position reader**. It has `scroll`,
  `scroll_with_state` and `scroll_into_view` — all actions that *perform*
  scrolling.
* It does use `UIScrollPattern` internally (`platforms/windows/element.rs:1429`,
  `:1457`) but only to drive scrolling, and `get_pattern` is not public, so
  paradigm cannot reach it.
* `UIElementAttributes::properties` is a generic map but only ever receives
  `AutomationId` (`:525`). No scroll state leaks through it.

So reading the scroll offset means taking `uiautomation` as a **new direct
dependency** of paradigm — the same conclusion the text-selection investigation
reached on 2026-08-19.

## The candidate directions, with what is unknown about each

**1. Read the scroll offset and store document-relative coordinates.**
Cleanest in principle: `y_document = y_viewport + scroll_offset`. Requires the
new dependency. Two things are unverified and would have to be measured before
committing: whether Chrome and Edge expose `ScrollPattern` for *web content* at
all, and — more awkward — that the pattern reports `VerticalScrollPercent`, a
**percentage**, not a pixel offset. Converting it needs the scrollable content's
total height, which is not obviously exposed. A percentage of an unknown height
is not a coordinate.

**2. Abandon pixels for document-order ordinal.** Scroll-invariant by
construction, and it is what `an-element-identity-mark-records-no-field-label.md`
already named as the real direction. But it is exactly the three-walk read that
`position-reads-dominate-an-ordinary-copy-paste-session.md` measured at ~432ms
and 14.8s per session. The two open defects squeeze positional identity from
opposite sides: the cheap signal is not stable, and the stable signal is not
cheap.

**3. Scroll-epoch partitioning.** Detect scroll *from the captured data itself* —
an element with the same name and width at a different y proves the view moved —
and refuse to compare positions across an epoch boundary. Needs no new
dependency and no extra reads, and it is honest. The cost is that it discards
comparisons rather than fixing them: a recording that scrolls between every
record leaves one element per epoch and nothing to cluster.

**4. One snapshot at stop.** Re-read the page once when recording ends, so every
position shares a single scroll state. Attractive until you notice it requires
re-finding each captured element by identity in that snapshot — which is the
problem positional identity was introduced to solve, so it is circular. It also
assumes the page is still open and unchanged at stop, which a recording that
ends by navigating away breaks.

## What a fix must not do

**It must not silently normalise with a guessed scroll delta.** Inferring the
offset from a pair of observations of one element works only when that element
was captured twice, and would produce a confident wrong coordinate when it was
not.

**It must not treat a negative y as invalid input.** A negative y is a correct
report about an element above the viewport, and the defect this doc records was
found *because* the value was faithfully stored. Clamping it would have hidden
the evidence.

**It must not be assumed to affect only long pages.** Any surface that scrolls
during a recording is affected, including a spreadsheet — the destination side is
addressed by cell reference today, so it is insulated by accident rather than by
design.

## Reproducing

1. Start Record Mode on a page taller than the viewport.
2. Copy a value, scroll, copy another, and paste both.
3. Save the recording and dump it:
   `cargo run --example axis_from_recording -- <id> %APPDATA%\com.amitj.paradigm --all`
4. Look for one element name appearing at two different y values, or any
   negative y.

## Related

* `docs/known-issues/position-reads-dominate-an-ordinary-copy-paste-session.md`
  — why direction 2 is not simply the answer.
* `docs/known-issues/an-element-identity-mark-records-no-field-label.md` — the
  other half of the identity problem.
* `docs/planning/Filtered-Post-Hoc-Confirmation.md` — the pipeline that depends
  on positional identity being stable, and which is blocked on this.
