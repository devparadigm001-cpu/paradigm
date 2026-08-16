# Two open spreadsheets and neither exposes its formula bar

**Status:** open. Understood precisely, and **not fixable from the reader side**
by any mechanism tried. Investigated 2026-08-16.
**Where:** `source::spreadsheet::SpreadsheetReader` — every per-cell read.
**Severity:** high, and it lands on exactly the configuration a templated
workflow needs.

## The finding, in one line

If **two or more distinct Google Sheets documents are open**, the formula bar of
*every* one of them stops exposing cell contents to the accessibility tree.
With exactly one open, it works perfectly.

## Deterministic, not intermittent

Checked first, because a handful of correlated runs is not a cause.
`windowsweep` and `distinctsweep` repeat the same read at each configuration:

| configuration | correct reads |
|---|---|
| 1 window, 1 document | 5/5 |
| 2 windows, **same** document | 5/5 |
| 3 windows, **same** document | 5/5 |
| 1 window + **1 other document** | **0/4** |
| 1 window + **2 other documents** | **0/4** |

Window count is not the variable — three windows on one document read perfectly.
The variable is how many **distinct documents** are open, and the threshold is
the second one.

## Not a targeting problem

The element chosen is the same element in both states — same position, same
724×27 size, same index. Only its content differs:

```
one document open :  #5  2143,195  740x27  "Blue Horizon Supply\n"
two documents open:  #5  2193,245  724x27  "\n\n\n\n<BOM>\n"
```

The Name Box reads `"A2"` correctly throughout, so navigation lands every time.
It is the formula bar's **text** that is absent, from an element that is
present, correctly sized and correctly located.

The Edit count also drops 9 to 8, and the Name Box count varies between 1 and 2
across runs. Both are real and both are incidental: the element that disappears
is a 1×1 stub `MIN_FIELD_W` rejects anyway, and the second Name Box is already
excluded — see
[a-second-name-box-is-picked-as-the-formula-bar.md](a-second-name-box-is-picked-as-the-formula-bar.md).

## Everything tried, and rejected

| attempt | result |
|---|---|
| `activate_window` on the target, then read | still empty |
| wait 4s after navigating, then re-read | still empty |
| real mouse click into the page, then navigate and read | still empty |
| read the **most recently opened** document | still empty |
| read the **first opened** document | still empty |

The last two matter most: with two documents open, **neither** reads. It is not
that one keeps a live formula bar while the others go stale — they all go stale.

## Why this is worse than it first looks

A templated workflow is defined between a **source document** and a
**destination document**. When those are separate spreadsheets, both must be
open for a run — which is precisely the configuration that breaks every read.

This reframes a lot of the difficulty seen while building §4. The probes that
worked used ONE document with two sheets (`<doc>!Sheet1` into `<doc>!Sheet2`),
so only one document was ever open. The real workflow that kept producing
silent blanks — and the overwrite warning that would not fire — uses two
separate documents.

## What still works

* **The CSV-export scan.** `check_for_new_records` reads the source through
  `export?format=csv` and never touches the formula bar. Verified reading
  correctly with no browser open at all. Built as a speed fix; it is now the
  only read path unaffected by this.
* **Single-document workflows.** Two sheets in one document are unaffected.
* **Failing loudly.** Because the formula bar is now proved rather than guessed,
  this condition surfaces as a refusal at `open` instead of blank data flowing
  silently into `classify_row`, `peek` and the overwrite check.

## What would actually address it

Not a workaround inside the reader — every mechanism available to it has been
tried. The realistic directions are structural:

1. **Extend the CSV path to the run's source reads.** Already named as a
   direction in
   [two-documents-in-one-window-cannot-both-resolve.md](two-documents-in-one-window-cannot-both-resolve.md);
   this makes it considerably more attractive, since it removes the formula bar
   from the source read path entirely. It does not help the destination, which
   has to be written live.
2. **Find another verification for the destination.** The writer's read-back is
   the other formula-bar dependency, and a failed read-back currently means a
   refused write. Whether an export-based check could stand in for it is
   unmeasured.
3. **Decide whether one document with two sheets is the supported shape** for
   now, and say so — rather than letting users build two-document workflows that
   cannot read.

## Reproducing

```
text_capture_probe windowsweep <doc> 5     # window count is not the variable
text_capture_probe distinctsweep 4         # distinct documents IS the variable
text_capture_probe focustest               # activation and waiting do not help
text_capture_probe clicktest               # real interaction does not help
text_capture_probe editdump <doc> A2       # the element list, side by side
```
