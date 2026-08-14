# The new-batch scan takes minutes on a real sheet

**Status:** open, not urgent. Measured 2026-08-14 against a live workflow.
**Where:** `run::batch::scan`, via the `check_for_new_records` command.

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

None attempted. Recorded because the current behaviour reads as a hang, and the
next person to see it should know it is arithmetic rather than a deadlock.
