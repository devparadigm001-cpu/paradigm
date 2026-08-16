# A second "Name box" element gets picked as the formula bar

**Status:** open, **serious** — silently returns blank for cells that have data.
Found 2026-08-16 while chasing why the overwrite warning stayed silent.
**Where:** `source::spreadsheet::SpreadsheetReader::open` /
`require_formula_bar`.

## What happens

A Google Sheets browser window exposes **two** elements named `Name box`:

```
window : "Untitled spreadsheet - Google Sheets ..."
  Name boxes : 2
  role:Edit  : 8
     #0 at 2009,195 reads "B2"     <- the real Name Box
     #1 at 2085,195 reads ""       <- a second one, empty, to its RIGHT
```

`open` takes the first (`.into_iter().next()`), which is correct — #0 is the
real one. It then calls `require_formula_bar`, whose rule is "the nearest
qualifying `Edit` on the Name Box's row, at or after its right edge".

**Name box #1 satisfies that rule and is nearer than the real formula bar.** So
the reader pairs the real Name Box with a second Name Box and calls it the
formula bar.

The result is the worst possible shape of failure:

* `goto` navigates and **verifies** — it reads #0, which updates correctly, so
  the position guard passes;
* every `read_cell` then reads #1, which is always `""`;
* so cells that plainly hold data come back **blank, with no error**.

Confirmed against ground truth. The reader resolved the right document, the
right sheet (gid 0), with the window activated, and reported:

```
shape_at(2) -> occupied []
CSV ground truth:  A2 = "Blue Horizon Supply"   B2 = "1150"
```

## What it broke

The overwrite warning, which is how it was found: an occupied destination read
as empty, so the warning stayed silent. But the fault is **not** in the warning
— it is in every read through `SpreadsheetReader` whenever the second Name Box
is present. That includes the run's own source reads.

It also explains why this looked like a multi-window problem and was not.
`window_for` resolved correctly every time; the damage happens after, inside
the window.

## Why the existing guard did not catch it

`require_formula_bar` exists precisely to refuse a bad pick, and its docs say
so: "the alternatives to the formula bar in a real window include the Name Box,
which reports a plausible-looking cell reference". The reasoning anticipated
reading `"B2"` where a customer name was meant — a wrong value, which is
noticeable.

The case that actually occurred is quieter. The second Name Box reads **empty**,
so the reader returns `""` and every caller treats it as a blank cell, which is
a legitimate value everywhere in this system: `classify_row` calls it blank,
`peek` calls the source exhausted, and the overwrite check calls the
destination safe.

## The fix, not yet applied

Exclude Name Box elements from formula-bar candidates. `open` already locates
them by name, so their rectangles are known before `require_formula_bar` is
called and can be filtered out of the candidate list.

Worth pairing with a positive check rather than only an exclusion: the formula
bar is the element that changes when the selection changes, and a reader could
confirm its choice once at `open` — navigate to a known non-empty cell and
require the candidate to report something. That turns "I picked the nearest
Edit" into "I picked an Edit that demonstrably reports cell contents", which is
the property actually wanted.

## Reproducing

```
text_capture_probe nameboxcount          # counts Name box elements per window
text_capture_probe destcheck <doc> <row> # what the reader reads vs the CSV
```

## Fixed 2026-08-16, and what the fix did NOT fix

**1. Name Boxes are excluded from candidates.** `rank_formula_bar_candidates`
takes every Name Box rectangle in the window and filters them out, however well
they fit the geometry. Two tests pin it: one that the real formula bar is chosen
from the measured layout, and one stating what the bug WAS — without the
exclusion, nearest-to-the-right is the second Name Box.

**2. The choice is now proved, not assumed.** `open` no longer takes the
nearest candidate. It ranks them and requires each to DEMONSTRATE it reports
cell contents — navigate to a header cell, and reject the candidate if it
echoes the reference back (a Name Box) or reports nothing across every mapped
column. The first that proves itself is kept; if none do, `open` fails with what
it tried and why.

Verified against the exact failing case. In a clean single-window state:

```
shape_at(2) -> occupied ["A", "B"]
      A2 = "Blue Horizon Supply"
      B2 = "1150"
CSV ground truth:  A2 = "Blue Horizon Supply"   B2 = "1150"
```

### The part that is NOT fixed

With **several Sheets windows open at once**, the formula bar element still
reports nothing. Dumped mid-failure, after navigating to a cell holding
"Blue Horizon Supply":

```
#5   2153,205   724x27   "\n\n\n\n\n\n\n\n\u{feff}\n"      <- multi-window
#5   2143,195   740x27   "Blue Horizon Supply\n"           <- single window
```

Same element, same cell, same document, same gid — content in one state and
newlines-plus-a-BOM in the other. The Name Box reads the right cell throughout,
so navigation is landing; it is the formula bar's *content* that is absent. The
count of Name Box elements also varies between runs on the same window (two,
then one), which suggests the tree carries stale entries when several Sheets
windows exist.

**What the fix changes about this is the failure mode, and that is the point.**
Before, a blank read was indistinguishable from real empty data and flowed
silently into `classify_row`, `peek` and the overwrite check. Now it fails at
`open` with:

> no element on the Name Box's row could be shown to report cell contents … Tried
> 1 candidate(s): candidate 5: it reported nothing for any of ["A", "B"] at
> header row 1.

Silent wrong data became a loud refusal. The underlying multi-window rendering
problem is separate, is not understood, and should be tracked on its own rather
than folded in here.
