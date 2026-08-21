# ipc_pipeline may have been quieted rather than fixed, and nobody has measured it

**Status:** OPEN QUESTION, deliberately unanswered. Raised 2026-08-21.
**Severity: unknown, and that is the point.** If the answer is "quieted", a
capture defect became harder to notice while remaining exactly as broken --
which is the specific failure `a8ae43c` was written to prevent.
**Where:** `tests/ipc_pipeline.rs::full_pipeline_over_ipc`, and the click path
in `capture::mod.rs` that now reads `UIElement::id()`.

## What was observed, and how little it proves

`a8ae43c` (2026-08-08) records the test as failing **roughly 6 runs in 10**,
faithfully reporting the race in
`docs/known-issues/text-input-capture-truncation.md`. That commit deliberately
refused to retry or `#[ignore]` it: *"a test red 60% of the time is reporting
that faithfully."*

On 2026-08-21, after the last-click source fix, six runs gave **one failure and
five passes**.

Six samples cannot distinguish that from the documented rate. At p(fail)=0.6,
seeing one or fewer failures in six runs has probability around 4% -- unlikely,
not impossible, and precisely the size of coincidence that turns up when you go
looking after the fact. **No claim is made here that the rate changed.**

## Why there is a plausible mechanism, which is what makes it worth measuring

The fix put `UIElement::id()` on the click path in
`capture::mod.rs::observe_grid`, ahead of `grid.note_click`. It costs a measured
**837us mean** -- four cross-process UI Automation reads.

The race it might touch is specific. `a8ae43c` records that the test drives the
field in the **no-settle shape** -- the whole string in one `type_text` with no
pause after the click -- measured at about 4 captures in 10, against 40/40 for
the settled shape that real human typing produces. A delay inserted between the
click and what follows is exactly the kind of thing that converts one shape
toward the other.

So the concern is not vague. It is: **an 0.84ms delay may have moved a
documented race enough to change what the test reports, without changing whether
the underlying text capture works.**

## What a real answer requires

Not six runs. Not a run today and a run tomorrow.

1. **20 runs at `HEAD` before the fix** (`2527eef`), counting failures.
2. **20 runs at the fix** (`ee912be`), same machine, same session, nothing else
   running -- this test drives a real browser and is sensitive to load.
3. Compare the two proportions. With 20 runs a side, a shift from 0.6 to 0.2 is
   detectable; a shift from 0.6 to 0.45 is not, and the result should say so
   rather than round toward the interesting answer.

At ~32s per run that is roughly 20 minutes of wall clock per side. It is cheap
in effort and expensive in attention, which is why it wants a session of its
own rather than being squeezed onto the end of something else.

## What must NOT be done with the result

**A greener test must not be reported as progress.** If the rate did drop, the
capture defect is unchanged and the signal that reports it has been degraded.
That is a regression in observability, and the correct response is to restore
the signal -- by making the test drive the no-settle shape harder, not by
accepting the quieter number.

**And if the rate did not change, that must be stated too.** The hypothesis
above is plausible and may simply be wrong. Six runs that happened to look
interesting are not evidence, and the write-up should be equally willing to
record "no detectable change".

## Related

* `docs/known-issues/text-input-capture-truncation.md` — the defect the test
  exists to report.
* `docs/known-issues/the-source-of-a-copy-is-read-from-focus-which-a-web-selection-never-moves.md`
  — the fix that added the click-path read, and its measured cost.
