# A repeated click collapsed every positional candidate

**Status:** FIXED 2026-08-22, same day it was introduced and the same day it was
found. Kept because the lesson about the FIXTURE is the durable part.
**Severity while live: HIGH.** `detect::candidates` returned zero page-side
candidates on any recording a person actually made. The spreadsheet half worked
throughout, which is what made it look like a working feature.
**Where:** `detect::candidates::assign_records`, the collision check.

## What it was

The check rejected the whole page side when two elements landed at the same
record, band and rank:

```rust
if !seen.insert((record, band, rank)) {
    return None;
}
```

Intended to catch a mis-detected pitch folding two records into one. It could
not do that, because **rank is taken from a deduplicated x list**, so two
elements at one key always share an x. What it actually detected was any
*repeated position* — which is to say, any user clicking the same thing twice.

The fix compares the y instead. The same element clicked twice has one y; a
genuine fold has two.

## The measurement that exposed it

Session record-1a2123c0, a natural OrderFlow recording:

```
179 page clicks -> 0 field groups, 0 candidates
distinct click positions 24, repeated positions 23, most-clicked hit 59x
```

**23 of 24 positions were clicked more than once, and one was clicked 59
times.** The check fired on the second click of the session and every candidate
after it was lost.

## Why no test caught it, and what changed

Every fixture was clean: one click per field per record. The controlled
OrderFlow probe made **six clicks on six distinct positions**. A person makes
**179 on 24**.

A clean fixture cannot express this defect at all — the collision it fires on
never occurs. So the regression test,
`repeated_clicks_on_one_element_do_not_collapse_the_page_side`, is built from
the **measured click count per position**, transcribed from the real recording,
not from a tidy one-click-per-field arrangement.

Verified to bite: reverting the check to the old form turns that test from 4
candidates to 0.

## The lesson, which is the reason this file exists

**A fixture that is tidier than reality tests a system that does not exist.**
The tidiness here was not laziness — one click per field is the obvious way to
write the case — and it is exactly what hid a defect that made the feature
useless on every real recording.

Where a rule depends on how often something occurs, the fixture has to carry a
real distribution. `identity::tree`'s fixtures already do this, and are labelled
as real captures for the same reason.

## Related

* `docs/planning/Filtered-Post-Hoc-Confirmation.md` — the recording, and the
  full funnel before and after.
* `docs/known-issues/an-action-cannot-say-which-window-it-happened-in.md` — the
  second defect the same recording exposed.
