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

### The clicking is real, and it is `press_key` — but only when focus fails

**Corrected 2026-08-15** (see "Range-reading" below, where the implementation
was finally read rather than inferred). The first version of this section said
every `press_key` clicks. It does not.

`goto` reads as a keystroke:

```rust
self.name_box.press_key("{Enter}")
```

The default is `press_key(key, try_focus_before = true, try_click_before =
true)`, and the Windows implementation does this:

```rust
if try_focus_before {
    match self.focus() {
        Ok(_)  => { /* focused; NO click */ }
        Err(_) => if try_click_before { self.click() }   // fallback only
    }
}
```

So the click happens on every call where **`focus()` failed**, not on every
call. That matters for reading the original report: a user who says a scan
"keeps clicking" is telling you that focus is failing repeatedly on the Name
Box — which is a symptom to chase, not cosmetic noise, and sits in the same
family as the intermittent `PositionLost` in
[an-open-cell-editor-turns-a-write-into-an-append.md](an-open-cell-editor-turns-a-write-into-an-append.md)'s
neighbourhood.

It also explains the measurement below better than the original reading did:
removing the click did not speed anything up because in the healthy case there
was no click to remove — the 21% regression was pure state-tracking overhead.

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

## Range-reading, investigated 2026-08-15: the tree does not offer it

Option (1) below — "ask the source for a range instead of a row at a time" — was
investigated before building. **The accessibility tree does not expose a
range's values.** Measured with `text_capture_probe rangeread`, differentially:
snapshot every text-bearing element with **A2** selected, snapshot again with
**A2:B6** (ten cells) selected, and diff. A hopeful search would have found the
formula bar showing the active cell and called it progress; a diff cannot.

Both states contain **19 text-bearing elements**. Exactly two differ:

| element | one cell | range of ten |
|---|---|---|
| Name Box | `"A2"` | `"A2:B6"` |
| formula bar | `"A2"`'s value | still only the **active** cell's value |

No new element appears, and **no element holds more than one cell's value** --
checked against all five known values in the range, not by eye.

So per-cell navigation is not a missed optimisation. It is the only thing the
tree offers, and `SourceReader` is not leaving anything on the table. Option (1)
as written -- "a change to `SourceReader`, not to `scan`" -- is **not
implementable against the accessibility tree**, and that sentence was wrong.

### The clipboard route is UNRESOLVED, not ruled out

Select the range, Ctrl+C, read the clipboard. Three attempts, all failing to
deliver the keystroke at all:

* `press_key("^c")` -- focus fell back to a click, which collapsed the range to
  a single cell. The copy was meaningless, and without the Name Box check added
  afterwards it would have looked like a clean negative result.
* `press_key_with_state_and_focus("^c", false, false)` -- nothing focused, keys
  went nowhere.
* `press_key_with_state_and_focus("^c", true, false)` -- focused, still nothing
  copied.

In every attempt the Name Box read `"A2:B6"` at the moment of the copy, so the
selection was right; the clipboard was simply unchanged. **This is a delivery
failure, not evidence against the approach** -- the same trap as the reverted
click "optimisation", where a mechanism failed before the hypothesis was
reached. Whether Sheets would put a TSV block on the clipboard for a selected
range is still unmeasured here.

Two things to settle before anyone tries again:

1. **How to send Ctrl+C through `terminator` at all.** `press_key` ends in
   `send_keys(key, 10)`, which is SendKeys syntax, so `^c` should be right --
   but it demonstrably did not arrive. Prove the keystroke lands on something
   observable before spending it on this question.
2. **Whether taking the clipboard is acceptable at all.** Nothing in the design
   forbids it -- §3 permits transient handling of source values, and the
   clipboard mentions in
   [multiline-document-capture-duplicates.md](multiline-document-capture-duplicates.md)
   are about `use_clipboard: false` in replay's typing path, not a prohibition.
   The real objection is ownership: a scan that silently replaces what the user
   had copied is taking something that is not the app's, and doing it every time
   Check for new runs. Save-and-restore is possible -- the probe does it -- but
   it is not free and not atomic, and a crash mid-scan leaves the clipboard
   holding spreadsheet rows.

### What this means for the real fix

If the clipboard route is also unavailable, the remaining candidate is the
**CSV-export design** -- the same `export?format=csv` request every probe in
this repo already uses as ground truth. That is not a patch to `SourceReader`;
it is a second source implementation with its own authentication, freshness and
sheet-selection questions, and it deserves its own evaluation rather than being
bolted on. Recorded here so that option (1) below is read as "needs a design",
not "needs an afternoon".

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

## CSV-export spike, 2026-08-15: fast and fresh, but it needs the browser

Measured with `text_capture_probe csvspike` against the same live sheet, using
the existing `export?format=csv` mechanism every probe here already uses as
ground truth. A spike, not an implementation.

### Speed and freshness: both good

| | measured |
|---|---|
| one export, 3 consecutive fetches | 2.33s, 2.35s, 1.30s — **~2.0s average** |
| per-cell scan, same sheet | ~0.98s per cell, 2 cells per row |
| break-even | **~2 rows** |
| projected at `SCAN_LIMIT = 200` | **391s per-cell vs 2.0s export (~196x)** |

An export costs the same whatever the row count; the per-cell path does not.
That is the whole case, and it is why the ratio on a small sheet understates it.

**Freshness is not a problem.** A cell was edited and the very next export —
**1.3 seconds later** — already contained the new value. No lag, no polling, no
stale-read window. This was the risk most likely to kill the idea outright and
it did not materialise.

### The real obstacle is authentication

A plain fetch of the export URL with no browser session:

```
STATUS=401
```

The export URL is **not** publicly readable. Every use of it in this repo works
because it drives the signed-in **browser**, which carries the session cookie,
and then reads the downloaded file out of the Downloads folder.

That is fine for a probe and awkward for a product feature. It means a
CSV-based scan would either:

1. **keep driving the browser** — which works today, but opens a window per
   export and drops a file in the user's Downloads every time Check for new
   runs. §4.10's "does not lock the window" is not literally violated, but the
   spirit is; and
2. **hold real Google credentials** — OAuth, token storage, refresh, revocation,
   and a consent screen. That is a product decision about connecting an
   account, not a `SourceReader` refactor.

There is no third option in which an unattended run quietly fetches a CSV.

### Conclusion for the "real fix"

The performance case is settled and strong — roughly 196x at the scan ceiling,
with no freshness penalty. The blocker is entirely about **access**, and it is
the kind that needs a decision rather than an implementation.

So option (1) below stands as the right direction and remains **not a quick
fix**. What it needs next is a decision on how the app is allowed to reach the
user's spreadsheet, not more measurement.

### One caveat about the spike itself

The first run reported "54 data rows" and a 52.8x ratio. Both were wrong: Sheets
exports every allocated-but-blank row as a bare `,`, and the row filter counted
those as data, inflating the denominator tenfold. Corrected to count lines with
at least one non-empty field before the numbers above were taken.

The same run also left a `FRESH-3` marker in the sheet's D1. Its cleanup called
`write("D", "")`, and **writing an empty string does not clear a cell** —
`type_here` types nothing, the old value survives, the read-back then fails, and
the error was swallowed. Deleting is a different operation from writing nothing;
the probe now uses the Name Box and the Delete key and verifies against the
export. `clearcell` was added for the same reason.
