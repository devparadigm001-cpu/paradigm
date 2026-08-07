# Capture is unreliable against complex web grids (Google Sheets)

**Status:** two distinct problems, both **confirmed against real recordings**;
root causes hypothesised but **not established**.
**Affected:** `src-tauri/src/capture` — clipboard handling (absent) and typed
text against `combobox`-role grid cells.
**Platform:** Windows. Observed in Google Sheets. Other grid UIs untested.
**Found:** 2026-08-06, during Phase 1 Step 12 — the first real human end-to-end
test of the whole app.
**Severity: HIGH for the product, not blocking Step 12.** Spreadsheets are a
prime target for the repetitive data-entry work this product exists to automate,
so unreliable capture there is a gap in the core use case rather than an edge
case. It is explicitly *not* a blocker in the way the text-truncation defect
was: Step 12 ran, completed, and produced informative results — **this finding
is one of those results.**

## Summary

Every probe before Step 12 targeted simple `<input>` elements or Notepad. The
first session against a real, canvas-rendered, virtualized grid surfaced two
problems that no prior test could have caught:

1. **Clipboard operations produce no captured action at all.** Copy and paste —
   the actual mechanism of the user's task — are invisible to the pipeline.
2. **Typed text into Sheets' cell editor is frequently empty or garbled, and
   cell attribution drifts** between the click and the type action.

Both were found by a human doing a real task, not by a probe.

## Evidence

### Finding 1 — clipboard operations are not captured

Session `record-17bbca6a-4baf-4fe2-bfba-dbb1df918e24`:

| | |
|---|---|
| Captured actions | 60 |
| Unmapped raw events | 450 |
| `type` actions | **1** |

The user's task was copying data from Notepad and pasting it into Sheets. The
single `type` action was a date the user re-typed by hand — **not** from a
paste. The copy and paste operations themselves produced no captured action of
any kind, so the moment of data transfer, which is the entire substance of the
task, is missing from the recording.

#### On the suspected cause — partly confirmed, partly not

The hypothesis going in was that `Ctrl+C`/`Ctrl+V` are classified as neither
trigger keys nor typing keys in `capture/text.rs`. Checking the code, that is
half right, and the wrong half matters:

* **Not trigger keys — confirmed.** `is_trigger_key` is only `0x0D` (Enter) and
  `0x09` (Tab).
* **Not typing keys — false.** `Ctrl+C` and `Ctrl+V` arrive as key codes `0x43`
  (`C`) and `0x56` (`V`), both inside `is_typing_key`'s `0x41..=0x5A` range. And
  nothing in `src/capture` reads `ctrl_pressed` at all — verified by grep. So a
  paste currently increments the keystroke counter **exactly as if the user had
  typed a literal `V`**, inflating the count and, since the emit condition is
  "value changed OR keystrokes seen", potentially triggering an emit for the
  wrong reason.

**The more direct cause is upstream of the key handling entirely**
(`src/capture/mod.rs:91`):

```rust
record_clipboard: false,
```

Clipboard recording is switched off in the `WorkflowRecorderConfig`, with the
comment "Not needed for Phase 1 and each is another source of captured content
we would have to gate." No clipboard event is ever emitted, so no amount of
key-classification work will surface a paste on its own. Any fix starts here.

Neither cause is confirmed as *the* explanation for the observed recording —
they are what the code says, not what was measured on session 17bbca6a.

### Finding 2 — empty/garbled payloads and cell attribution drift

Session `record-50020dc5-99d6-487a-9266-9818ffce8cd3`, manual typing only, no
clipboard involved. Three `type` actions were captured against `combobox`-role
cells, and **2 of the 3 carried empty or corrupted payloads**:

* one payload empty;
* one reading `"19"` plus a stray invisible character, rather than the value
  actually typed.

Separately, and possibly a different problem: clicks and types were sometimes
attributed to **different cells** — a click on `C1` followed immediately by a
`type` action logged against `B1`. Sheets identifies "which cell" through its
Name Box accessibility element, which appears to update asynchronously; at the
moment a type action completes it does not reliably reflect the truly-focused
cell.

This resembles the no-settle race in
[text-input-capture-truncation.md](text-input-capture-truncation.md) — a value
read before the UI has caught up — but against a `combobox`-role grid cell
rather than a standard `<input>`. **Do not assume they are the same defect.**
The differences are substantial enough to matter: a canvas-rendered virtualized
grid, a role that is not `edit`, and a separate element (the Name Box) standing
in for the target's identity. A stray invisible character in the payload is also
unlike anything the `<input>` investigation produced. Whether it is one root
cause or two is an open question, and answering it is the first task below.

### Secondary: already-tracked issue confirmed in the wild

Session `17bbca6a` also captured a `navigate` action to **Paradigm's own
window** mid-recording. That is a live, real-world confirmation of the
system-wide capture scope documented in
`docs/known-issues/record-mode-unscoped-system-wide-capture.md` — recorded here
only as corroboration; see that doc for the issue itself.

Note that doc currently exists on the `frontend-dev` branch only, so the link
above will not resolve from `backend-dev` until the branches meet.

## Why it matters

Spreadsheet data entry is close to the archetypal task this product automates:
repetitive, structured, and exactly what someone would want to record once and
replay. Both findings break that use case in different ways, and both fail
quietly:

* **A missing paste** means the playbook contains the navigation and the clicks
  but not the data movement. Replaying it reproduces the shape of the task while
  doing none of the work, and nothing in validation can detect the gap — a
  playbook with no `type` action is perfectly valid.
* **An empty or garbled payload** puts wrong data into a real spreadsheet on
  replay. Cell attribution drift is worse still: correct data written to the
  wrong cell is the kind of error that survives review, because every individual
  step looks plausible.

This is also a lesson about the test strategy rather than only about Sheets.
Every probe to date used simple `<input>` elements or Notepad, and every one
passed. One human session against a real target surfaced two problems
immediately. Probe coverage has been measuring the environment it was built for.

## Next steps

- [ ] **Decide how clipboard operations should be modelled at all.** Enabling
      `record_clipboard` is necessary but not sufficient: there is no `paste`
      value in the schema's `action_type` enum (`click`, `type`, `navigate`,
      `read`). Either a paste compiles down to a `type` carrying the pasted
      text — which puts clipboard contents into the store and therefore through
      the redaction policy — or the enum grows, which is a locked-schema
      migration. This is a design decision, not a code fix.
- [ ] **Stop treating `Ctrl`-modified keys as typing.** Independent of the
      above, `is_typing_key` counting `Ctrl+V` as a literal `V` is wrong on its
      own terms. `KeyboardEvent` carries `ctrl_pressed`; `capture` currently
      ignores it.
- [ ] **Determine whether Finding 2 is the documented no-settle race or a new
      defect.** Both are plausible and they imply different fixes. The existing
      `examples/text_capture_probe` harness can be pointed at a grid to compare
      captured payloads against ground truth the same way.
- [ ] **Investigate cell attribution separately from payload corruption.** They
      appeared together but may be independent; the Name Box being a distinct
      element from the cell being typed into is reason enough to treat them as
      two problems until shown otherwise.
- [ ] **Test a second complex grid** — Excel Online, or any non-Sheets
      virtualized data grid — to establish how much of this is Sheets-specific
      versus general to grid UIs. This determines whether the fix is a
      special case or a category.
- [ ] **Add a complex-grid target to routine probe coverage.** The gap that let
      both findings reach a human test is that nothing between the simple
      `<input>` probes and a live user session ever exercised one.
