# Templated Workflows — gaps in the Section 5 build order

Two pieces the design needs are not items in the backend build order, and
neither is a consequence of the other. Written down because the last one of
these — capture recording *where a value came from* — was produced in item 2,
consumed by nothing until item 6, and was one review away from being finished
without anyone noticing.

**Both are now items in the design document's Section 5** — the destination
writer as item 8 (built), the first-record check as item 9 (not built). This
file is the record of how each was found and why it was missing; Section 5 is
where the work is tracked. A gap recorded only here would repeat the exact
failure it describes.

Status as of item 8 (destination writer, built and proven end to end).

---

## Gap 1 — §4.3's first-record safety check has no backend item

**§4.3 in full:**

> Before committing to a full batch, the system shows the very next record it's
> about to write — real values, in the real destination — with a simple
> confirm/cancel. Catches a wrong mapping after one record instead of after
> twelve.

**Where it appears in the build order:** Section 6 (frontend), item 2 —
"First-record preview with confirm/cancel (4.3)". That is the only mention.

**Where it does not appear:** anywhere in Section 5. Items 7, 8 and 9 are run
controls, new-batch detection and format-drift detection. None of them is this,
and items 1–6 were not either.

So the frontend has an item to build a preview screen, and there is no backend
item for the thing that would populate it.

### What it actually needs, none of which exists

1. **Read the next record without writing it.** `run::run_with_control` reads
   and writes in one pass; there is no "show me what record N would be" entry
   point. The pieces are all there — `SourceReader::peek` and `read` — but
   nothing composes them into a dry run.
2. **A place for `detect::verify` to be called.** Item 4 built the Qwen
   sensibility check and item 6 established that this is its real call site:
   `verify` needs a human-readable label per field locator, which comes from the
   source's header row, and this is the only point in the flow where a reader is
   open on the source *and* nothing has been written yet. It is currently built,
   tested, and called from nowhere.
3. **A gate on the run.** §4.10: "Rejecting the first-record preview (4.3):
   cancels cleanly. Nothing activates; the recording stands as an ordinary
   one-shot playbook, unaffected." Nothing today can express that outcome —
   `compile_and_store_playbook` either attaches a template or does not, and the
   decision is made before any preview could have happened.

### Conclusion

**Not in scope for item 7, and it needs its own backend item.** It is not run
controls, and folding it into them would bury a user-facing safety check inside
an item about pause and stop.

**Resolved as a tracking matter:** it is now **Section 5, item 9** of the design
document, placed before new-batch detection because it gates whether a run
starts at all. Still unbuilt — this is a scheduling fix, not an implementation.

---

## Gap 2 — `DestinationWriter` has no concrete implementation

Item 6 built the `DestinationWriter` trait and tested the run loop against
fakes. Item 7 built the controls and the background thread, also against fakes.
Both are genuinely tested — but nothing implements the trait for a real
surface, so no run can currently touch a real spreadsheet.

This is item 6 residue. Section 5 item 2 says the source-reader interface should
be built "with the spreadsheet reader as its one concrete implementation", and
`SpreadsheetReader` exists. The destination side got the interface and no
implementation, and item 6's report did not say so.

### What it needs

A `SpreadsheetWriter`, mirroring `SpreadsheetReader`: locate the Name Box,
navigate to the target cell, type the value, and verify it landed. `goto` in
`source::spreadsheet` already does the navigate-and-verify half and is proven
against live Google Sheets. The new part is the write and its read-back check.

It needs live evidence against a real Sheets window to be worth anything, the
same standard `SpreadsheetReader` was held to.

### Resolved — built as Section 5 item 8

`run::spreadsheet::SpreadsheetWriter` exists and the loop has been proven
against a live Google Sheet: three records read from Sheet1 and written to
Sheet2, paused mid-record and resumed, verified by per-sheet CSV export, then
re-run to confirm the ledger prevented a second write. See
`examples/text_capture_probe.rs -- templatedrun`.

### What it did NOT resolve

There is still no command that **starts** a run, so the four run-control
commands (`pause_workflow_run`, `resume_workflow_run`, `stop_workflow_run`,
`get_workflow_run_status`) remain registered and reachable with nothing to act
on. A start command is now unblocked — `run::background::spawn` can be handed a
real reader and writer — but it belongs with item 9, because §4.3 says the
first-record preview gates the run, and adding a start command that skips that
gate would build the thing item 9 exists to prevent.
