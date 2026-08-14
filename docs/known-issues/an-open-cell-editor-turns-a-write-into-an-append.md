# An open cell editor turned a write into an append

**Status:** FIXED 2026-08-14. Reproduced, fixed, and the fix re-measured by the
same probe that found it.
**Where:** `run::spreadsheet::SpreadsheetWriter`.
**Found by:** a real corruption in a user's destination sheet, not by a test.

## What the user saw

A run reported a write failure. Their destination sheet afterwards held

```
A2 = "Northwind Traders\n\n\n305.75"
B4 = (empty)
```

`305.75` belonged in B4. `SpreadsheetWriter` has no operation that moves a
cell's contents, so the value did not travel by any route the code knowingly
takes — something typed it there.

## What was actually happening

`type_here` types into `focused_element()`, on this assumption:

```rust
// `set_value` on the editor is not available here, so the cell is selected
// and typed over, which replaces its contents.
```

That is true of a **selected** cell. It is not true of a cell whose **editor is
already open** — there, typing lands at the caret, which is a merge. Nothing
was ensuring the editor was closed.

`examples/text_capture_probe.rs editmode` drove four cases against a scratch
sheet. Case A is the baseline; B, C and D each open the editor first:

| case | setup | result |
|---|---|---|
| A | fresh cell holding `ORIGINAL` | `NEW-A` — correct |
| B | editor opened with F2 | `"\nNEW-BORIGINAL"` |
| C | editor open, `XYZ` typed into it | `"\nNEW-CORIGINALXYZ"` |
| D | edit left open on **A4**, write aimed at **B4** | B4 = `"\nNEW-DORIGINALPENDING"` |

Two distinct failures, and the newline separator that appears in every one of
them is the `\n` from the user's corrupted cell.

Case D is the serious one. The pending edit **followed the navigation**: A4's
uncommitted text was carried to B4 and committed there, alongside the value
actually aimed at B4. A value landed in a cell it was never aimed at — the one
failure this module exists to prevent.

The editor can be open for reasons the run did not cause. This user had typed
into the sheet by accident during an earlier interrupted scan, which is exactly
how the state arises in practice.

## What held up, and what did not

**The read-back caught every single one.** All three corrupting writes were
refused, `RunStop::WriteFailed` was raised, and nothing was marked processed.
The verification layer did its job.

**The Name Box check did not catch it, and could not.** It confirms *which cell
is addressed*, not *whether an editor is open on it*. Both are true at once in
every failing case above.

**The report was wrong**, and that was fixed first, separately: the summary said
"Nothing was written" over a destination that had visibly changed. See the
commit *"A failed write must never report that nothing happened"*.

## The fix

`dismiss_editor()` — press Escape — at the start of `write` and `shape`, the two
public entry points where prior UI state is unknown.

Escape **discards** the pending edit. That is deliberate: the run cannot know
what that text was, and merging unknown content into a destination cell is
strictly worse than losing it.

Re-running the identical probe after the fix:

```
  A fresh cell             success      A1 = "NEW-A"
  B editor open            success      A2 = "NEW-B"
  C editor open + text     success      A3 = "NEW-C"
  D open edit, other cell  success      A4 = "ORIGINAL"   B4 = "NEW-D"
```

Every cell holds exactly what was aimed at it, and the pending edits were
discarded rather than merged or carried.

## A second defect this exposed

`csv_at` — the helper every spreadsheet check in the probe is measured against —
split on `,` and `\n` with no quote awareness. Against cells containing
newlines it read them as row breaks, shifted every row below, and reported
**"no value landed in the wrong cell"** while the raw export showed one plainly.

It would have done the same for any cell containing a comma; `Northwind
Traders, Inc.` is an ordinary customer name. A verifier that misreads ground
truth is worse than none, because it is believed. Replaced with a real quoted
parser and pinned by seven tests, including one against the verbatim export
from the run that found this.

## Not explained by this

The user's cell held **three** newlines. The reproduction produces one per
merge. Three suggests three merges into the same cell across a run, which is
consistent but unmeasured. Not asserted.
