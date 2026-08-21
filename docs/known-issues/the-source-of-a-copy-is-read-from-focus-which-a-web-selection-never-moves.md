# The source of a copy is read from focus, which a web selection never moves

**Status:** FIXED 2026-08-21, same day it was found. Kept because the
limitations below are live constraints, not history: the fix declines in four
situations, and each decline is a link that does not exist.
**Was:** measured and reproduced in session record-fe88fb0d.
**Severity: HIGH.** It is the direct cause of `SourceDidNotAdvance` on every
non-spreadsheet page tested so far, and it silently collapses every source in a
session into one.
**Where:** `capture::grid::read_position` (`grid.rs:603`) takes the source of a
copy to be `Desktop::focused_element()`, and `read_element_position`
(`grid.rs:658`) asks only where *that* element sits.

## What the session recorded

Nine pastes, nine different values, one source:

```
[paradigm] source links: 9 pair(s), 1 distinct source position(s)
[paradigm]   link ...447: el/19/   -> A2
[paradigm]   link ...152: el/19/   -> B2
[paradigm]   link ...048: el/19/   -> C2
[paradigm]   link ...329: el/19/   -> A3
[paradigm]   link ...330: el/19/   -> B3
[paradigm]   link ...071: el/19/   -> C3
[paradigm]   link ...948: el/19/   -> A4
[paradigm]   link ...156: el/19/   -> B4
[paradigm]   link ...037: el/19/   -> C4
```

The destination advanced perfectly — nine distinct cells, three rows of three.
The source is `el/19/` every time: record ordinal 19, and an **empty label**.

## The "/ month" hypothesis is refuted, twice over

The standing hypothesis was that a repeated element such as `/ month` was being
classified as structural and pinning the source. It is wrong on two independent
grounds.

**It was never captured.** `/ month` appears nowhere in the 56 recorded steps.

**The clicks were all distinct.** The same recording captured the source side
correctly through a different path:

```
    1 click  x2053 y 239 w 55   Free
   22 click  x2053 y 468 w 751  Limited Codex access
   23 click  x2053 y 627 w 36   Go
   27 click  x2053 y 666 w 135  Expanded access
   34 click  x2053 y 521 w 47   $8
   40 click  x2053 y 672 w 52   Plus
   51 click  x2053 y 766 w 70   $20
```

Distinct names, distinct y, correctly captured. So the recording *does* contain
nine distinguishable sources. The link path just is not reading them.

## The mechanism, reproduced in isolation

A local page with three tiers (`Free`/`Go`/`Plus`, `$0`/`$8`/`$20`, and three
identical `/ month` lines). Three double-click selections at three different
positions, each followed by `Ctrl+C`, while a probe sampled the focused element
every 400ms:

```
  [19983ms] id=609402  role=Document  name="TIERPROBE"

48 samples, 1 distinct focused element(s)
```

**Focus never moved.** Selecting text in a browser does not change the focused
element — the Document keeps focus throughout. So the source read returns the
same node for every copy in the session, and because a Document has no label
beside it, the reference is `el/<n>/` with the label empty. That is `el/19/`.

## Why the spreadsheet side was never affected

On a spreadsheet the selected cell **is** the focused element, and that path
returns early through the Name Box before any of this is reached. The
assumption "focused element = what the user is acting on" happens to hold there
and fails everywhere else. It was never tested on a page until now.

## The fix, and the evidence it works

`read_element_position` now resolves the source from the **last click** rather
than the focused element. `WorkflowEvent::Click` already reached
`grid.note_click` carrying `metadata.ui_element`; its id was being discarded.
No new dependency, and no extra walk — the read was already paid for on the
click.

**Same isolated case that exposed the defect, re-run against the fix:**

```
[paradigm] copy source: el/19/
[paradigm] copy source: el/20/
[paradigm] copy source: el/21/
```

Three selections, three copies, **three consecutive records**. Before: one
reference three times.

**The real ChatGPT pricing page**, `$0` / `$8` / `$20`, scrolling between each:

```
[paradigm] copy source: el/30/
[paradigm] copy source: el/50/
[paradigm] copy source: el/18/
```

Three distinct sources. `prove_advance` tests distinctness, and distinctness
holds.

### A caveat those numbers make visible

They are not in order — 30, 50, 18 for three tiers read top to bottom. The
ordinal is computed against whatever `collect_nodes` saw on that walk, and the
page was scrolled between copies, so each ordinal is relative to a different
tree. **The ordinal is meaningful only within one snapshot.**

Distinctness is what detection needs and it is satisfied here, but the mirror
risk follows directly and is NOT covered: three copies of the *same* record at
three scroll positions could produce three different ordinals and look like
advancement. That is `element-bounds-are-viewport-relative-so-scrolling-moves-them.md`
arriving through a second door, and this fix does not close it.

### The label is still empty, and that is a different defect

`el/19/`, not `el/19/PRICE`. On the tier pages the price has no repeated label
beside it, so there is nothing to address it by. Unchanged by this work; see
`an-element-identity-mark-records-no-field-label.md`.

## The four declines, each tested live

The recency rule is `CLICK_SOURCE_GRACE_MS`, five seconds, modelled on
`COPY_KEY_GRACE_MS`. It is the one number here that is chosen rather than
measured, and it is a first calibration.

Every case below was run against a live page, and every one printed
`(none -- declined)`:

| Case | Gesture tested | Result |
|---|---|---|
| No click at all | window focused programmatically, then `Ctrl+C` | declined |
| Stale click | click, wait 7s, `Ctrl+C` | declined |
| Click in another window | click in Notepad, focus the page, `Ctrl+C` | declined |
| Structural element | double-click one of three identical `/ month` lines | declined |

The last is `identity::tree::locate` refusing a structural element, not a rule
written for this — a label is not a position. So `/ month` does decline, just
never for the reason the original hypothesis proposed.

**The cross-window run carried its own positive control**: the same session
went on to copy a real price and printed `el/20/`. A run in which everything
declines proves nothing about declining correctly, which is the lesson
`wheel-and-mouse-events-are-dropped-until-the-first-mouse-move.md` records.

### What it costs, measured

Reading the clicked element's id is not free: `UIElement::id()` hashes four
cross-process UI Automation reads -- automation id, control type, name, class
name. It is paid on EVERY click, not only on copies, so it was given its own
counter rather than being allowed to hide inside a keystroke figure:

```
[paradigm] clicked element ids: 6 calls, 837 us mean, 5025 us total
[paradigm] grid sampling: key-down 6 calls, 118841 us mean
```

**0.84ms per click**, against 119ms for a key-down on the same surface. Not a
concern at this scale, and now a number rather than an assumption.

### What is genuinely lost

A copy from a **focused input the user never clicked** — tabbed into, selected
with `Ctrl+A` — used to resolve through focus and now declines. Whether it ever
produced a *correct* answer is untested; focus is right in that case in
principle. It is a real narrowing, stated rather than discovered later.

## The old fix direction, kept for the reasoning

The clicked element is the source, not the focused one — and the pump already
has it. `WorkflowEvent::Click` reaches `grid.note_click` with `element_role`,
`element_text` and `metadata.ui_element`, and today that is used only to
identify the application and notice a sheet switch.

So: remember the last click's element and prefer it over `focused_element()` on
the non-spreadsheet branch. No new dependency, no extra walk — the read is
already paid for on the click.

**What that will not cover, and must not be claimed to:**

* a selection made by keyboard alone, with no click;
* a drag-select that begins on one element and ends on another, where the last
  click is the start and the copied text spans further;
* a copy issued with no preceding click at all, where there is no better
  answer than today's.

A last-click source must therefore be *recency-bounded* in the same way
`last_copy_key_ms` already bounds the clipboard pairing, and must decline
rather than reach back to an unrelated click from a minute ago.

## Reproducing

1. `cargo run --example focus_vs_selection_probe -- 20`
2. In any browser page, double-click a word, press `Ctrl+C`, and repeat on two
   other words elsewhere on the page.
3. The probe reports **1 distinct focused element** across all of it -- the
   underlying platform behaviour, which the fix routes around rather than
   changes.

A warning for anyone automating this: verify the element coordinates against
`page_bounds_probe` in the SAME window you are clicking. A run of this test
reported one source for all three copies and looked like a regression; the
window had opened 20px offset from the previous one, so every double-click
landed in the card padding and hit the same container. That reading was
correct -- the same element clicked three times IS one source -- and the
mistake was in the harness, not the code.

To watch the fix instead, run `capture_probe` and do the same three
select-and-copy gestures. Each copy prints its own
`[paradigm] copy source: el/N/` line as it happens, so a source that fails to
advance is visible immediately rather than only once a paste has paired with
it.

## Related

* `docs/known-issues/an-element-identity-mark-records-no-field-label.md` — the
  empty label in `el/19/` is that issue, seen here as a symptom rather than a
  cause.
* `docs/known-issues/element-id-is-a-hash-of-the-text.md` — why a captured click
  cannot simply be re-identified by id later.
* `docs/known-issues/position-reads-dominate-an-ordinary-copy-paste-session.md`
  — the walk this path pays for, which on this evidence is being spent to
  produce the same answer nine times.
