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

**1. Read the scroll offset and store document-relative coordinates. MEASURED
AND DEAD (2026-08-21).**

The idea was `y_document = y_viewport + scroll_offset`. Probed against a tall
page in Edge, scrolling with real Page Down keystrokes between samples:

```
sample 0 (top)           percent=0.000  marker_y= 1209
sample 1 (+3x PageDown)  percent=0.000  marker_y=-1299
sample 2 (+3x PageDown)  percent=0.000  marker_y=-2327
```

The page moved more than 3500px. **`VerticalScrollPercent` stayed exactly 0.000
throughout.** The pattern IS exposed -- found at depth 11 on a `Pane` named
`Chrome Legacy Window` -- and both of its write methods, `scroll()` and
`set_scroll_percent()`, fail outright. It is a stub: present in the tree,
non-functional in both directions.

So the question was never the awkward one about converting a percentage to
pixels. There is no value to convert.

This is the trap this project keeps meeting in a new place: the pattern's
*presence* would have been taken as evidence that reading it works. Only
scrolling the page and watching the number not move settles it.

**The probe is not in the repository, and that is deliberate.** It needed
`uiautomation` as a dev-dependency, and adding one changes the lib test
binary's hash — which this machine's Application Control policy then blocked
permanently (`os error 4551`), leaving `cargo test --lib` unrunnable. A probe
that answers a question once is not worth an unrunnable suite, so both were
removed once the measurement was in hand. To redo it in about fifteen minutes:

1. add `uiautomation = { version = "0.22", features = ["pattern"] }` to
   `[dev-dependencies]`;
2. `auto.create_matcher().contains_name("<window title>").find_first()` — match
   the window by TITLE, not by focus, because running a probe returns focus to
   the terminal;
3. walk down with `auto.create_tree_walker()` until
   `get_pattern::<UIScrollPattern>()` succeeds;
4. read `get_vertical_scroll_percent()`, scroll the page by any external means,
   and read again.

Note for whoever does: `scroll()` and `set_scroll_percent()` on that pattern
both fail, so the scrolling has to come from outside — Page Down through
`WScript.Shell` worked.

**2. Abandon pixels for document-order ordinal.** Scroll-invariant by
construction, and it is what `an-element-identity-mark-records-no-field-label.md`
already named as the real direction. But it is exactly the three-walk read that
`position-reads-dominate-an-ordinary-copy-paste-session.md` measured at ~432ms
and 14.8s per session. The two open defects squeeze positional identity from
opposite sides: the cheap signal is not stable, and the stable signal is not
cheap.

**3. Scroll-epoch partitioning. SCOPED 2026-08-21, and it changed shape.**

The original sketch was pure inference: *an element with the same name and width
at a different y proves the view moved.* Two things came out of scoping it.

**First, that inference rule is not weak, it is wrong.** A vertical list of
records — the exact layout this pipeline exists for — routinely shows the same
text at the same x and different y in every record. Three rows reading `$0` are
not a scroll; they are the structure. The rule would read normal repetition as
motion and split one record set into several epochs, destroying the clustering
it was added to protect. The salvageable form is a **rigid-translation** test:
a scroll moves *every* element by the *same* delta, so a boundary needs two or
more re-observed elements agreeing on one offset. One element moving stays
ambiguous.

**Second, and more usefully: the scroll is already in the event stream,
unread.** `MouseEventType::Wheel` carries `scroll_delta`, and every filter that
would drop it is off in paradigm's config (`filter_mouse_noise: false`,
`performance_mode: Normal`, no rate limit, zero processing delay). Paradigm's
pump simply has no `WorkflowEvent::Mouse` arm, so it is discarded on arrival.

Measured, against a tall page in Edge:

```
  wheel #1   delta=(    0,   -1)  at ( 3383,  530)  under: Document "WHEELTEST"
  ... one event per notch, element under the cursor resolved
```

```
  4 notches  MARKER-0  -2930 -> -3330   =  -400px
  7 notches  MARKER-0  -3330 -> -4030   =  -700px
```

**Exactly 100px per notch, linear, and identical for every marker** — a rigid
translation, predicted to the pixel. `scroll_delta` is in **notches, not
pixels**, so the conversion is real but it is a per-surface constant.

So direction 3 splits cleanly, and both halves are worth having:

* **3a — partition on observed boundaries.** Needs only the boundary, not the
  magnitude. Cheap: one pump arm, no UI Automation reads, no new dependency.
* **3b — correct, where a magnitude exists.** `y_document = y + cumulative_px`,
  from wheel notches times a calibrated pixels-per-notch. Strictly better than
  discarding, and available only sometimes.

### What is measured, and what is still unknown

| Scroll gesture | Observable? | Magnitude? |
|---|---|---|
| Mouse wheel | yes, one event per notch | yes — 100px/notch **on the one surface measured** |
| Page Down / keyboard | yes, as key code 34 | **no** |
| Scrollbar drag, trackpad, touch | untested | untested |
| Programmatic (anchor jump, focus scroll-into-view) | **no input event at all** | no |

Page Down is measured, not assumed: two presses moved the page **1656px** and
produced **zero** wheel events. The keystroke says *that* the view moved and
says nothing about how far, because 828px is the viewport height, not a
property of the key.

The 100px/notch figure is one browser, one zoom level, one Windows
`SPI_GETWHEELSCROLLLINES` setting. Treating it as a constant would be the same
error as treating `VerticalScrollPercent`'s presence as evidence it worked.
**It must be calibrated per surface or not used.**

The last row is the hole that no observation closes. A page that scrolls itself
emits nothing. That is where the rigid-translation test earns its place — not
as the primary mechanism, but as the backstop for the cases direct observation
cannot see.

### What building it costs

Stamp each `CapturedAction` with `scroll_epoch: u32` and
`scroll_offset_px: Option<f64>` at capture time, where the ordering is already
known, and carry both into the stored payload. **Leave `element_bounds` raw.**
A consumer adds the offset when it is present and refuses to compare across
epochs when it is not — which keeps the rule in this doc that a negative y is a
correct report and must not be clamped away.

One caveat found while measuring, written up separately in
`wheel-and-mouse-events-are-dropped-until-the-first-mouse-move.md`: the
recorder discards wheel events entirely until it has seen a mouse *move*, so
the signal is gated on something a keyboard-driven user may never do.

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
* `docs/known-issues/wheel-and-mouse-events-are-dropped-until-the-first-mouse-move.md`
  — the gate that made the wheel signal look absent when it was only unarmed.
