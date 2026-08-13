# Templated Workflows — gaps in the Section 5 build order

Two pieces the design needs are not items in the backend build order, and
neither is a consequence of the other. Written down because the last one of
these — capture recording *where a value came from* — was produced in item 2,
consumed by nothing until item 6, and was one review away from being finished
without anyone noticing.

Status as of item 7 (run controls).

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

Suggested position: **before item 8**, since it gates whether a run starts at all
and both remaining items assume runs happen.

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

### Consequence while it is missing

The four run-control commands (`pause_workflow_run`, `resume_workflow_run`,
`stop_workflow_run`, `get_workflow_run_status`) are registered and reachable but
have nothing to act on, because there is no command that starts a run. Adding a
start command needs a writer to hand `run::background::spawn`.

**This is the shorter of the two gaps and blocks the more visible thing.**
