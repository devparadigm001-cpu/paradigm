# The new-batch scan takes minutes on a real sheet

**Status:** open, not urgent. Measured 2026-08-14 against a live workflow;
timed per-cell and investigated 2026-08-15.
**Where:** `run::batch::scan`, via the `check_for_new_records` command.

> **2026-08-15:** two candidate fixes were investigated and **both ruled out by
> measurement**, not by argument. Neither the mouse clicks nor a stale Name Box
> is the problem. See "What the 2026-08-15 investigation settled" below before
> proposing either again.

## What was measured

Clicking **Check for new** on a real templated workflow (two mapped columns,
two separate Google Sheets documents), polled every 20 seconds:

```
[ 20s] Looking for new records
[ 40s] Looking for new records
 ...
[160s] Looking for new records
[180s] Run the workflow on these      <- prompt finally appeared
```

**Roughly three minutes**, with the UI showing "Looking for new records…" the
whole time and the button disabled.

## Why

`scan` walks the source one row at a time and calls `peek` on each, and every
`peek` is a **real Name Box navigation plus a formula-bar read, per mapped
column**. `SpreadsheetReader::goto` alone sleeps 900ms before it will trust
what it reads, because it verifies the cursor arrived rather than assuming.

So the cost is roughly:

```
rows scanned  ×  mapped columns  ×  (navigate + read)
```

With two mapped columns that is somewhere around three seconds per row, and it
is paid **every time the user clicks Check for new** — including when the answer
turns out to be "nothing new", which is the common case for a workflow that is
already up to date.

`SCAN_LIMIT` caps the walk at 200 rows, so the worst case is bounded but large:
several minutes before the prompt appears.

## Why it is not simply a bug

The cost buys correctness that this system has already paid for twice. `goto`
verifies the cursor landed because an unverified navigation is how a run reads
or writes the wrong cell while reporting success — the failure mode
`selector-matching-precision.md` and `complex-web-grid-capture-unreliable.md`
both exist for. Making `peek` cheap by trusting the navigation would reintroduce
exactly that.

It is also honest about what it does not know: the scan reports `capped: true`
when it stops at the ceiling, rather than presenting a partial count as a total.

## What the 2026-08-15 investigation settled

Prompted by a different-sounding report — that the app "keeps clicking Enter"
on the last-selected cell during a scan. It turned out to be this same
behaviour seen from the outside, and chasing it produced hard numbers.

### The clicking is real, and it is `press_key`

`goto` reads as a keystroke:

```rust
self.name_box.press_key("{Enter}")
```

It is not. The default is
`press_key(key, try_focus_before = true, try_click_before = true)`, so **every
call performs a real mouse click on the element first**. Nothing at the call
site says so. One click per cell read is why a scan looks like the app is
clicking rather than merely thinking.

Per-cell, not per-row: `read_cell` does one `goto`, `read_row` one per mapped
field, and `peek` on a **blank** row additionally reads `LOOKAHEAD_ROWS = 5`
rows ahead. A two-field blank row therefore costs **12** navigations.

### Removing the click makes it slower — measured, then reverted

The obvious fix is to drop the click: `set_value` has already put the reference
in the box, so it buys nothing. But the only public way to control that flag is
`press_key_with_state_and_focus`, and its state tracking costs more than the
click saves. `examples/text_capture_probe.rs scantime`, same sheet, same 12
rows, 24 cells:

| variant | total | per cell |
|---|---|---|
| `press_key` (clicks) | 23.45s | 0.977s |
| `press_key_with_state_and_focus` | **28.46s** | 1.186s |
| `press_key`, reverted | 23.50s | 0.979s |

**21% slower.** The revert lands within 0.2% of the original, so the middle row
is a real regression rather than noise. Shipped on reasoning alone, it would
have been announced as a speedup while making scans a fifth slower.

### The Name Box is not stale, and re-resolving it is not warranted

The other hypothesis was that `SpreadsheetReader` resolves `name_box` once in
`open()` and never again, so a click by coordinates could land on the grid
after a scroll or re-render — which would look exactly like Enter being pressed
on the last-selected cell.

Confirmed against real use: **no navigation error appeared during normal
scanning.** The only `PositionLost` came from the user typing into the sheet
mid-scan, which is the guard doing its job — `goto` verifies the Name Box reads
the requested cell and refuses otherwise. So the guard is holding with the
current handling.

Re-resolving per navigation would add a locator lookup **per cell** — ~24 per
12 rows — on the path the table above shows is already dominated by per-cell
cost, to buy correctness that is demonstrably already present. Not implemented,
deliberately.

### Where the time actually goes

Not the click, and not element lookup. `NAVIGATE_SETTLE` is **900ms of each
977ms cell read, about 92%**. No change to the keypress can matter beside it.

That constant is the only real lever left, and it is **not** a comfort margin:
it is how long navigation is given to land before the formula bar is believed.
Shortening it risks reading the *previous* cell's value — the exact failure
`run::spreadsheet` exists to prevent, and the same family as the corruption in
`an-open-cell-editor-turns-a-write-into-an-append.md`. Left untouched. Any
change there needs a correctness measurement, not a stopwatch.

This is why option (1) below is still the real fix: it removes navigations
entirely rather than trying to make each one cheaper, and every attempt to make
them cheaper now has a number showing why it does not work.

## What would change it, roughly in order of appeal

1. **Ask the source for a range instead of a row at a time.** Reading a whole
   column in one operation — a CSV export, or a single selection read — would
   collapse hundreds of navigations into one. This is the real fix and it is a
   change to `SourceReader`, not to `scan`.
2. **Start from the ledger, not from the top.** The scan currently re-walks
   every already-processed row before reaching new ones. `processed_count` is
   already known; starting the walk past it would skip the common case
   entirely. Cheaper than (1) and much narrower, but it assumes processed rows
   are contiguous from the start, which nothing currently guarantees.
3. **Report progress.** Not faster, but "checked 40 rows…" is a very different
   experience from an unchanging spinner for three minutes.

None of (1)-(3) attempted. Recorded because the current behaviour reads as a
hang, and the next person to see it should know it is arithmetic rather than a
deadlock — the loop is bounded by `SCAN_LIMIT = 200` and terminates.

Two other things were attempted, and both are dead ends with numbers attached:
removing the per-keypress mouse click (21% slower), and re-resolving the Name
Box per navigation (unnecessary — the guard is holding). Neither is worth
revisiting without new evidence.

## Reproducing the measurements

```
PARADIGM_SCRATCH_DOC=<doc-id> cargo run --example text_capture_probe -- scantime 12
```

Times the scan's real inner loop — `peek` then `advance`, the same two calls
`scan_with_limit` makes — and reports per-cell as well as per-row, because the
cost is per cell. Seeds the sheet only if it is empty, so a before/after pair
measures the same data; `clearscratch` blanks it between runs.
