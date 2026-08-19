# A grid edit restarts mid-typing and re-emits the same value

**Status:** open, real defect. Found 2026-08-18 from a live recording.
**Severity: MEDIUM.** It emits an extra `Type` step carrying the **complete**
value, so replay writes the same cell twice with the same content. Less damaging
than a partial write, but it inflates step counts and it is a symptom of the
edit-tracking state machine losing its place mid-edit.
**Where:** `capture::grid::absorb_sample`, the `Some(_)` "moved to a different
cell" branch.
**Distinct from** the cross-watcher doubling in
`sheets-cell-edits-are-captured-by-both-watchers.md`. That one is two *watchers*
emitting once each. This is `capture::grid` emitting **twice on its own**.

## What happens

Cell B2, one edit of `Ceramic Mug Set`, produced three `Type` steps. Two are the
cross-watcher pair. The third is this defect:

```
step 24  type  B2   detail: read from element (full value), 26 keystroke(s) over 4242ms   <- text watcher
step 25  type  B2   detail: grid cell editor, 35 keystroke(s) over 4489ms                 <- grid watcher
step 26  type  B2   detail: grid cell editor, 1 keystroke(s) over 0ms                     <- grid watcher AGAIN
```

**`1 keystroke(s) over 0ms`, carrying the complete value.** A `GridEdit` that was
created and emitted inside the same millisecond, holding all of
`"Ceramic Mug Set"`.

It is mid-session, not a stop-flush: steps 27 onward follow it.

## The mechanism

`emit()` does `self.current.take()`, so nothing can emit the same `GridEdit`
twice. Two emissions mean two edits existed. Only three things emit: a trigger
key, `flush()` (session stop and `ApplicationSwitch`), and `absorb_sample`'s
`Some(_)` branch. With no `Navigate` and no trigger key adjacent, it is the
third:

```rust
Some(_) => {                       // sampled cell != the edit in flight
    let identifiers = self.identity_for(&el);
    let finished = self.emit(timestamp_ms);      // emits the old edit
    self.current = Some(GridEdit { cell, text, … keystrokes: 1, started_ms: timestamp_ms });
    Absorbed::Switched(finished)
}
```

Two facts combine to make the re-emission carry the *whole* value:

1. **A new `GridEdit`'s text is the editor's entire current content**, because
   it comes from `sample()`, which reads `el.text(0)` — not "what was typed
   since". So an edit restarted after the value is complete immediately holds
   the complete value.
2. **`keystrokes` starts at 1 and `started_ms` at now**, so the next spurious
   switch emits it as `1 keystroke(s) over 0ms`.

That also explains why the same mechanism looks different depending on *when* it
fires: restart during typing yields a partial payload, restart after the value
is complete yields a duplicate complete one.

## What is not established: the cause of the spurious switch

For that branch to run, `sample()` must have returned a cell reference differing
from the edit in flight, while still passing `is_cell_editor` (`role == "ComboBox"`
and a name that looks like a cell ref). **What produced a different cell
reference mid-edit is not known.** No stray `Type` step for another cell appears
in the recording, which is consistent with the phantom edit emitting nothing —
`emit()` returns `None` on empty text — but that is inference, not evidence.

**A suggestive detail, deliberately not overstated:** `0ms` means two samples in
the same millisecond, and key-down/key-up of one key share a millisecond. Key-up
sampling (added 2026-08-17) is what introduced a second sample per key, so it
plausibly made this reachable where one-sample-per-key was not. That is a
hypothesis. It has **not** been shown that the second sample was the key-up, and
it should be measured rather than assumed before anything is changed.

## Scope

Observed once, in one cell, in one recording — B2. A2 in the same recording did
not show it, nor did C2. So it is intermittent, which is consistent with a
transient misread and is the reason the cause is still open.

## Why it matters

* Replay writes the cell a second time. Same value, so usually harmless, but it
  is an unrequested write and the project's safety model counts writes.
* It means the edit-tracking state machine can lose its place mid-edit without
  anything noticing. The benign symptom here (duplicate complete value) and a
  damaging one (a partial value committed as final) are the same bug at
  different moments.

## What a fix must not do

**It must not suppress "duplicate" emissions by comparing payloads.** Two
genuine edits of one cell to the same value are legal — a user retyping a value
is a real recording — and suppression would silently drop them.

**It must not require the cell reference to be stable to proceed.** A cell
reference that reads differently is exactly the signal that *something* changed;
ignoring it would trade this defect for a mis-attributed edit.

**It must not be "fixed" by removing key-up sampling.** That change closed a
measured defect — a value abandoned by a window switch was recorded one
character short, every time, and the fix is verified live (2026-08-18, A2
recorded the complete `"Harbor Point Traders"`). Any fix here must keep that.

The first step is diagnostic, not corrective: record what `sample()` actually
returned when the branch fired. The cell reference it saw is not currently
logged anywhere.

## Reproducing

Not reliably reproducible yet — it fired once in three cells.

1. Start Record Mode, open a Google Sheet, click a cell, type a multi-word value.
2. Stop, save the playbook, and
   `cargo run --example dump_playbook -- <id> %APPDATA%\com.amitj.paradigm`.
3. Look for a `grid cell editor, 1 keystroke(s) over 0ms` step, or any grid step
   whose keystroke count is far below the characters in its payload.

The `detail` string is the only way to see this, and it is **not** on the review
screen — see the diagnosability note in
`sheets-cell-edits-are-captured-by-both-watchers.md`.

## Related

* `docs/known-issues/sheets-cell-edits-are-captured-by-both-watchers.md`
* `docs/known-issues/text-input-capture-truncation.md` — the earlier
  one-character-short defect that key-up sampling was added to fix.
