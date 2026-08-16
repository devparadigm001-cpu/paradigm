# The formula bar reports nothing until the page has been typed into

**Status:** open, mechanism identified, no fix attempted. 2026-08-16.
**Where:** `source::spreadsheet::SpreadsheetReader` — every per-cell read.
**Supersedes the generalisation in**
[two-open-spreadsheets-kill-the-formula-bar.md](two-open-spreadsheets-kill-the-formula-bar.md),
whose reproduction data stands but whose explanation was wrong.

## The finding

A Google Sheets formula bar exposes cell contents to the accessibility tree
only **after the page has been genuinely typed into**. Navigating it — even
dozens of times — does not wake it. Writing one cell does, immediately.

Measured directly, one document, three steps in a row:

```
reader BEFORE any write : open refused — no element reports cell contents
write A1 = "WAKE"       : succeeded, its read-back verified
reader AFTER the write  : READ ["WAKE"]
```

## Why this looked like three different bugs

It explains every earlier observation, including the ones that contradicted
each other:

* **`SpreadsheetWriter` always works.** It types *first* and reads back
  *after*. Its verification therefore always runs against a woken page. It
  never had this problem and never could.
* **`SpreadsheetReader` fails on a freshly loaded document.** It never types.
* **`pathduel` found both paths reading fine, 4/4.** A write had happened in
  that session already — the page was awake, so both paths worked and the
  "reader vs writer" theory collapsed.
* **`settletest` failed for 204s across ten attempts.** No write ever occurred,
  and navigation alone (~20 navigations) never woke it. So it was never a
  settling period.

## What was ruled out along the way

| hypothesis | verdict |
|---|---|
| multi-**window** resolution | refuted — 3 windows on one document read 15/15 |
| the reader picks a different element than the writer | refuted — same element, same bounds; `groupdump` showed the "Name box" groups hold only the 75×20 Edit and a button, so the exclusion never touched the formula bar |
| the reader's positive-content check is too strict | refuted — `pathduel` had both paths at 4/4 in the same state |
| foreground / `activate_window` | refuted |
| waiting (4s, and 204s) | refuted |
| a real mouse click into the page | refuted |
| reading the most recent vs the first document | refuted — neither read |

The **distinct-document count** correlation from `distinctsweep` (5/5 with one
document, 0/4 with two) is real and reproducible, and is **not** explained by
this. With one document open the tree exposed contents without any write; with
two it did not. Whether that is a second effect or the same one under different
conditions is **unresolved** — and after being wrong twice tonight about this
mechanism, that is left as an open question rather than a third guess.

## What this means

The reader's positive-content check is **not** too strict. It is doing exactly
its job: refusing to read from an element that demonstrably reports nothing. It
turned a silent stream of blank cells into a loud refusal, which is how this was
found at all.

But it also means a `SpreadsheetReader` opened on a document nobody has typed
into will refuse — and for a **source** document, nobody ever does type into it.
That is the run's read path.

## Directions, none attempted

1. **Wake the page deliberately at `open`.** Type something into a scratch cell
   and undo it. Effective by this measurement, and unacceptable as written: it
   writes to a user's source document to read it. A variant worth measuring is
   whether typing into a cell and pressing Escape — which commits nothing —
   wakes it, since that would be a read-only provocation.
2. **Use the CSV export for the run's source reads.** Already named in
   [two-documents-in-one-window-cannot-both-resolve.md](two-documents-in-one-window-cannot-both-resolve.md)
   and measured viable at 1.32s per fetch. This removes the formula bar from the
   source path entirely and needs no provocation at all. It does not help the
   destination — but the destination writes first, so the destination was never
   affected.
3. **Accept it and document the shape that works.** The write path is sound.

## Reproducing

```
text_capture_probe waketest      # reader, write, reader — the A/B
text_capture_probe pathduel <doc> 4   # writer vs reader, alternating
text_capture_probe settletest    # time alone does not wake it
text_capture_probe groupdump <doc>    # what a "Name box" group contains
```
