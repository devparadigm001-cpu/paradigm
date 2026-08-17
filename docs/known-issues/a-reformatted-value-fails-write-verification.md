# A value Sheets reformats on entry fails write verification

**Status:** open, real defect. Found 2026-08-16 while probing Amazon as a source.
**Severity: HIGH.** It reports a **successful** write as failed, on exactly the
kind of data the product exists to copy.
**Where:** `run::spreadsheet::SpreadsheetWriter::write` / `read_back`.
**Unrelated to Amazon** — Amazon is only where prices first went through the
writer. Any destination write of a value Sheets normalizes hits this.

## What happens

`write` types a value into a cell, then reads the cell back and refuses to
report success unless the read matches what it wrote:

```rust
let landed = self.read_back(field, row)?;
if landed != value.trim() {
    return Err(SourceError::Unreadable { … "wrote {value:?} but the cell reads
        {landed:?} afterwards; refusing to report a write that did not land" });
}
```

Write `$9.99` and that check fails:

```
E FAILED: could not read "E" at "1": wrote "$9.99" but the cell reads "9.99" after…
E FAILED: could not read "E" at "3": wrote "$15.99" but the cell reads "15.99" aft…
```

**But the write succeeded.** The document's own CSV export, taken immediately
after, holds the values exactly as intended:

```
  row 1: E="$9.99"
  row 3: E="$15.99"
```

So the run reports a failure for a cell that contains precisely what was asked
for.

## The mechanism

`read_back` reads the **formula bar**:

```rust
let raw = self.formula_bar.text(0)…
```

The formula bar reports a cell's **underlying value**, not its formatted
display. Typing `$9.99` into Sheets does not store the string `"$9.99"` — Sheets
parses it as the number `9.99` and applies currency formatting. The cell then
*displays* `$9.99`, *exports* as `$9.99`, and its formula bar reads `9.99`.

The comparison is therefore between a formatted input and an unformatted
underlying value, which can never match for any value Sheets normalizes.

### The control that proves it

In the same run, one price wrote and verified **successfully**:

```
  wrote E at 2      payload="$13.79 List: $17.99"
```

That string cannot be parsed as a number, so Sheets stored it verbatim as text,
the formula bar returned it verbatim, and the comparison matched. **The
malformed value passed and the clean values failed** — which is the signature of
a normalization mismatch rather than a read fault or a write fault.

## Scope is wider than currency

Currency is only how this was found. The mechanism implicates anything Sheets
rewrites on entry:

| Written | Likely stored / formula bar |
|---|---|
| `$9.99` | `9.99` — **measured** |
| `50%` | `0.5` |
| `1,234` | `1234` |
| `8/17/2026` | a date serial |
| `007` | `7` |
| `(555) 123-4567` | may stay text, may not |

**Only the currency row is measured.** The rest follow from the same mechanism
and are *predicted, not tested* — recorded that way deliberately. Verifying them
is cheap and should be done before any fix is designed, because the fix's shape
depends on how many normalizations it has to survive.

## Why it matters

This is a **false negative in the one check that guards against silent wrong
writes**, and it fires on ordinary business data. A run copying prices — the
exact Amazon-listing-to-spreadsheet workflow that surfaced it — would fail on
every row while writing every row correctly.

The failure is also the worst shape for a user: the data is *there*, visibly
correct, and the app says it failed. That invites exactly the wrong response,
which is to stop trusting the verification.

## What a fix must not do

The obvious repair — compare loosely, or strip `$` before comparing — would be a
mistake, and the reason is the whole history of this file.

`read_back` exists because of measured, silent, wrong-value failures: writes
appending into an already-open editor (`"\nNEW-BORIGINAL"`), values landing in a
cell other than the intended one, and pending edits travelling with the cursor.
Those produce results that look plausible. A verification relaxed to "close
enough" would let all of them back through, and this project has repeatedly
found that a *mostly* correct run is the hardest kind of failure to notice.

Directions worth considering, none chosen and none measured:

1. **Normalize both sides through the same parse** before comparing — compare
   `9.99` to `9.99` rather than `"$9.99"` to `"9.99"`. Needs a definition of
   "same value" that does not accidentally equate genuinely different strings.
2. **Verify by export instead of by formula bar.** The export is already the
   arbiter everywhere else in this project, and it showed the correct value
   here. Costs a fetch per verification, which is likely too slow per write.
3. **Compare against the displayed value rather than the underlying one.** The
   display is what the user asked for — but it is not in the accessibility tree,
   so there is nothing to read.

## Reproducing

```
text_capture_probe amzncapture      # writes prices; currency rows fail verification
```

Any write of `$9.99` through `SpreadsheetWriter` reproduces it; Amazon is not
required.

## Related

* `an-open-cell-editor-turns-a-write-into-an-append.md` — why `read_back` exists.
* `gmail-as-a-source-what-the-tree-exposes.md` and
  `amazon-listings-as-a-source.md` — the investigations this came out of.
