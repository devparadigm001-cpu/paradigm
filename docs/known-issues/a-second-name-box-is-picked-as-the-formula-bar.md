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
