# Templated Workflows — Foundational Design (Pre-Phase-2)

**Status:** Real, deliberate redesign of what a "recording" and a "saved
workflow" mean in Record Mode. Confirmed to require new backend and frontend
work not covered by anything currently built. This document is the complete,
locked specification from a live design session — not the original ~182-page
planning corpus, which does not describe this mechanism anywhere (confirmed
by direct, repeated search).

**Priority:** This is foundational — agreed to be built and working before
Phase 2 (Ghost Mode) proceeds, since Ghost Mode's whole premise (detect and
automate repetition) depends on Record Mode actually being able to represent
a repeating, data-driven pattern in the first place, not just a fixed literal
sequence.

---

## 1. The problem this solves

Everything built in Phase 1 assumes a recording is a **fixed, literal
sequence of actions** — replay reproduces exactly what was captured, character
for character, at exactly the position it was captured. This was deliberately
and carefully built and tested to work exactly that way.

The real product need is different: a user doing repetitive data entry (the
concrete example used throughout design: copying orders from a source
spreadsheet into a formatted destination sheet, one order at a time) wants to
do this **a few times, then have the system recognize the pattern and
continue it with new data it has never seen** — and later, run the same saved
workflow again on an entirely new batch of orders, with zero manual
copy-pasting.

This requires a genuinely different kind of object: not a literal recording,
but a **template** — a mapping between a source and a destination, plus a
rule for advancing through both, applied fresh each time it runs.

## 2. Scope, deliberately narrowed for the first version

**This scopes the first build, not the architecture.** Spreadsheets are the
first concrete source type this gets built against, chosen for reliability
— but the underlying design uses general concepts (a source, a destination,
a record, a field mapping) throughout, with spreadsheets as the first real
implementation of those concepts, not a special case they're hardcoded
around. See Section 3's note on the source-reader interface for the one
place this distinction actually changes how something gets built, not just
described.

- **The first source type is spreadsheet applications specifically**
  (Google Sheets, Excel) — chosen for reliability: spreadsheets already have
  real, proven structured reading in this system from everything built to
  date. Other repetitive-task sources (a folder of files, an inbox, another
  kind of structured list) are real, intended future extensions, not ruled
  out — deliberately not attempted in the same build as the first source
  type, so getting one source type genuinely solid isn't put at risk by
  solving several at once.
- **Pattern detection happens once, right after the recording stops** — not
  live, while recording is in progress. Live detection is a real future
  option, not built now.
- **Three examples required before a pattern is trusted** (Rule of 3, matching
  the same threshold already used elsewhere in this product), and the source
  position must have genuinely, measurably advanced across all three — not
  just "looked similar." If the source didn't move between examples, that's
  treated as inconclusive, not as confirmation of a fixed, unchanging value.
- **Unrelated activity between examples is filtered automatically, not
  treated as breaking the pattern:**
  - Actions that never touch the source or destination (an accidental tab
    switch) are excluded entirely from what detection looks at.
  - A value that gets corrected before moving on (paste wrong, delete, paste
    right) counts only by its final, settled value — the correction itself
    is not treated as a separate step.

## 3. What Phase 1 already provides, and one real architectural collision

Confirmed directly against the current codebase, not assumed:

**Schema is flat and literal, exactly as expected.** `playbooks` and
`playbook_steps` hold no representation of "these steps are one pattern" —
`action_payload_json` carries exactly what capture produced (selector, typed
value, process scoping), nothing more. Compile's job today is annotate-and-
flatten: assign `reversible`, compute the selector, redact, serialize. No
aggregation, no detected relationships between steps.

**Source-position capture is not a missing feature — it's a reversal of a
real, deliberate decision.** `multiline-document-capture-duplicates.md`
explicitly considered and rejected storing where a pasted value came from,
on privacy grounds — called a real escalation, a redaction-policy question,
not a capture detail. This needed its own resolution before backend item 2,
not just building past it.

**Resolution, locked:** the tension mostly dissolves once what actually needs
storing is separated by what's genuinely sensitive:

- **The learned mapping** (durable, saved with the workflow) stores
  **structure only** — "source column C → destination column E, advance one
  row each run" — never a literal reference to specific past rows or their
  content. Structural, not sensitive; doesn't need redaction.
- **Duplicate-row tracking (4.7)** stores a **position marker only** — "row
  47: done" — no value attached.
- **Actual source values** are only ever handled **transiently** — during
  pattern detection (comparing the 3 live examples) and during each run
  (reading the current row to write it) — never persisted as a durable
  position-plus-content pair.
- The **existing accessibility-exclusion list already fully covers** the
  case of an app that should never be tracked at all — no new exclusion
  tier is needed, since an excluded app is never observed in the first
  place.
- Redaction applies only where source **content** could end up in text a
  model or log sees (e.g., Qwen's mapping-sensibility check) — never to the
  positional references themselves, which are structure, not content.

This keeps the feature fully buildable without needing to reverse the
original privacy reasoning at all — the durable, at-rest data this design
actually needs turns out to be structural, not the sensitive content-plus-
origin link the original rejection was specifically about.

**Everything else Phase 1 already provides, confirmed real and reused
directly:**
- Replay's tested wrong-window/wrong-element resolution fixes apply directly
  to the new iterative replay loop — inherited, not rebuilt.
- The local model (Qwen2.5-0.5B), already wired in for auto-naming, is given
  a second job (4.1) rather than needing a new integration.
- The existing reversible/irreversible safety pattern is reused directly for
  the missing-field handling in 4.4 — not a new concept.
- The existing Stored Playbooks list and shared confirmation-dialog
  component are extended, not replaced (see Section 6).

**One real architectural decision this scoping requires: a general source-
reader interface, not spreadsheet-specific logic threaded throughout.**
Reading "the next record" from a spreadsheet and reading "the next record"
from some other source (a folder, an inbox) are genuinely different
technical operations underneath — but pattern detection, mapping storage,
and replay should never talk to "a spreadsheet" directly. They talk to "a
source," through a single interface (get the current position, read the
next record, check for exhaustion, detect drift) that the spreadsheet reader
implements first. Adding a second source type later means writing a new
reader against that same interface — not rewriting detection, mapping, or
replay. This costs real, deliberate design discipline in how backend item 2
gets built now; the alternative is a genuine rewrite the first time a second
source type is wanted, not just an inconvenience.

## 4. The complete design, locked

**Read concretely, generalize through Section 3's interface.** Everything
below is described in spreadsheet terms — row, column, cell — because
spreadsheets are the actual first implementation being built, and concrete
language is clearer to build from than abstract placeholders throughout a
detailed spec. Wherever "row" appears, read it as "the next record from the
source"; wherever "column" appears, read it as "a mapped field" — the
general concepts Section 3's source-reader interface is built around. A
future second source type would implement that same interface differently;
nothing below is spreadsheet-specific by necessity, only by current example.

### 4.1 Pattern detection

After Stop is pressed: check whether, across at least 3 pasted/typed values,
both the source position and the destination position advanced in a
consistent, predictable way. If so, this is a genuine candidate pattern, not
just repeated clicking. The local model (Qwen2.5-0.5B) then verifies the
detected mapping is semantically sensible — e.g., confirming a source column
that looks like customer names is landing in a destination column that makes
sense for that data — the same model already used for labeling, given a new
job.

**If the model's confidence comes back low rather than clearly confident:**
the fallback is asking the user to re-record the workflow with cleaner
examples, rather than proceeding on a shaky mapping or presenting a vague
"is this right?" to the user with no real signal behind it. **The actual
confidence threshold that separates "proceed" from "ask to re-record" is
explicitly not fixed by this design** — it needs to be tunable and tested
against real behavior to find the level that reliably avoids both false
positives (confidently wrong) and unnecessary re-recording (unconfident when
the mapping was actually fine), the same way other thresholds in this
product (e.g., the 90% confidence floor elsewhere) are treated as real
values to calibrate from use, not numbers to guess at now.

### 4.2 Turning it on

The user is asked, plainly: *"This looks like a repeating pattern — want me
to do the rest?"* Alongside this, show what was actually detected (e.g.,
"Name → Column B, Total → Column E") — kept out of the way by default behind
a "See more" expansion, so the primary prompt stays clean, but the real
mapping is one click away for anyone who wants to verify it before
confirming. Nothing happens automatically without this confirmation.

### 4.3 First-record safety check

Before committing to a full batch, the system shows the very next record it's
about to write — real values, in the real destination — with a simple
confirm/cancel. Catches a wrong mapping after one record instead of after
twelve.

### 4.4 Running the workflow

Read the next unprocessed source record → apply the learned mapping → write
it to the destination → mark that source record as done, persistently (see
4.7) → check whether it matched the shape of the original examples → repeat
until the source is exhausted.

- **Record doesn't fit the pattern at all** → stop, show the user exactly
  which record and why.
- **Record is mostly fine but missing a field the source normally has** →
  continue (this is a reversible situation — a blank cell isn't damaging),
  but log it clearly for the summary. Never silently skip without recording
  that it happened.

### 4.5 Format drift — both sides, not just one

If the **destination** no longer looks like it did when the workflow was
recorded (a column that used to hold customer names doesn't look right
anymore), or the **source** has changed shape (a new column added, columns
reordered): stop and ask, using the same mechanism for both.

**The correction interaction, click-only for this version:** a small,
non-blocking floating panel — not a full blocking dialog, since the user
needs to be able to click in the live spreadsheet underneath it — asks the
user to click the correct column. Best-guess is offered first when a
plausible match exists ("Looks like column D now?"), one click confirms or
corrects.

**Confirm-before-locking-in, to guard against a mis-click:** after the user
clicks, the panel shows what it understood ("You selected column D — 'Client
Phone.' Use this for Customer Name?") before committing — a **Try again**
option re-opens the same prompt with nothing lost.

**Correction scope, explicitly asked:** after a correction, the user is asked
whether this should become a **permanent** part of the workflow going
forward, or was just a one-off fix for this particular record. Distinguishes
"this one order was weird" from "the format actually changed."

### 4.6 Run controls — Stop and Pause are genuinely different

- **Stop**: hard, permanent. Whatever record was actively being written when
  Stop is pressed is either allowed to finish cleanly or fully discarded —
  never left half-written.
- **Pause**: temporary. On resume, the system **backs up to the start of
  whatever record was in progress and redoes it cleanly from the beginning**
  — never resumes mid-write. A harmless redo is preferred over any risk of a
  half-done state surviving a pause.

### 4.7 Duplicate-row protection — persistent, not inferred

Every templated workflow tracks, durably, exactly which source rows it has
already processed. Re-running the workflow when nothing new exists in the
source should be recognized and reported plainly, not silently do nothing and
not silently reprocess everything.

### 4.8 New-batch detection

The system watches the known source for new, unprocessed rows and offers a
quick confirmation before running — *"Found 12 new rows starting at row
45. Run the workflow on these?"* — combining automatic detection with the
brief, deliberate confirmation the user asked for, rather than either fully
automatic (no chance to catch a mistake) or fully manual (defeats the point).

### 4.9 Summaries — quiet by default, detailed only when it matters

A clean run gets a short, plain line: *"Starting at order 3."* The report
only expands with real detail when something genuinely needs attention — a
missing field, a mismatch, a stopped run — matching the same "quiet when fine,
loud when not" principle already used throughout the rest of this product.

**End-of-run completion summary, two fixed pieces:**
- **The range just processed** ("Processed orders 45–57") — confirms it
  found the right starting point, not just that it finished.
- **A flagged-for-review count, shown only when greater than zero**
  ("2 flagged for review") — kept off a clean run's summary entirely, per the
  same quiet-by-default principle; only appears when there's genuinely
  something to look at.

### 4.10 Execution mode and source-exhaustion signal

**Runs in the background, non-blocking.** The user can keep working in the
app while a run is in progress — it does not lock the window. Minimizing the
app is fine and does not interrupt a run; **fully closing the app does**,
since this executes on the app's own background thread rather than
surviving independently of it (unlike Ghost Mode's continuous detector,
which is a different kind of process). A completion popup surfaces the
summary (above) once the run finishes.

**Defining "the source is exhausted":** stop at the first row where
specifically the columns the mapping actually reads from are empty — not any
blank row anywhere, since a stray formatting gap in an unrelated column
shouldn't be mistaken for the end of the data. If a mapped-column gap is
found but there is clearly more non-blank data further down in those same
columns, that is treated as suspicious rather than conclusive — stop and ask
rather than silently deciding the run is complete.

**Timeout on an unanswered confirmation prompt:** not fixed at a single
value. Depends on whether leaving it open genuinely blocks the user from
other work, or has a real cost (e.g., holding a resource, a paid API call
left pending) — decided per prompt type at build time, not guessed at once
here.

**New-batch watching, scoped consistently with 4.10's execution model:**
checks for new, unprocessed source data whenever the app is open (including
minimized) — not a separate, persistent background service running
independently of the app, which would be a different execution model than
everything else in this design. One consistent rule throughout, rather than
"this piece survives being closed but nothing else does."

**Rejecting the first-record preview (4.3):** cancels cleanly. Nothing
activates; the recording stands as an ordinary one-shot playbook, unaffected.

**A stopped run (4.6) keeps whatever it already successfully wrote** —
consistent with how every other reversible action in this system already
behaves; stopping does not undo prior, already-completed writes.

### 4.11 Ongoing workflow state — a schema decision, not a single flag

Confirmed by the "already run once, just replaying — no need to re-ask"
behavior (4.2/4.8): a templated workflow carries real, ongoing state (has it
been confirmed as a repeating pattern; is it currently running or paused),
which is naturally represented as a small set of real columns on the
existing schema, decided against the real, current table structure at build
time — not a single boolean, and not layered onto the existing `source`
constraint, which already means something else.

### 4.12 Recording contains the pattern — one per workflow, review screen
extended, not replaced

**Field order doesn't matter, field identity does.** If the same source
fields land in the same destination columns each time, that counts as one
consistent pattern regardless of the sequence they were filled in during the
recording.

**One pattern per workflow — refined: coherent, not merely singular.** A
detected pattern may span more than one column mapping, as long as they all
advance together consistently (e.g., source columns A and C both feeding
destination columns B and D, in the same row-by-row rhythm) — that is one
coherent, multi-column pattern, not several conflicting ones. This gets
surfaced quietly in the "See more" detail alongside the primary mapping, not
flagged as a conflict.

It is only **genuinely distinct, unrelated patterns** appearing in the same
recording (e.g., part of the recording copies orders into one sheet, an
unrelated part does something else entirely) that should fail detection for
that recording — treated as a signal the recording needs to be split into
separate, single-purpose recordings, not something detection tries to
resolve on its own.

**The existing review screen is extended, not replaced.** When a pattern is
detected, the recorded example steps still appear on the same review screen
already used today — unchanged, still editable the same way. The detected
mapping is added as its own section at the top of that same screen, also
editable. The "want me to do the rest?" confirmation (4.2) is the natural
next action from this screen, not a separate, disconnected prompt — so if
the mapping looks wrong, the user is already looking at exactly the captured
steps it was inferred from, in the place they'd expect to fix it.

### 4.13 Duplicate-row tracking is scoped per workflow, not per source

If two different saved workflows both read from the same source (e.g., one
copies orders into a shipping sheet, another into an accounting sheet),
their processed-row tracking must be kept fully independent. Sharing
tracking by source alone would mean running one workflow could make the
other silently believe rows it has never touched are already handled — a
real correctness risk, not a hypothetical one. The row-tracking table (5.1)
is keyed by workflow identity, not source identity alone.

## 5. Backend build order

1. **Schema**: real, ongoing per-workflow state (4.11) — templated-or-not,
   confirmed-or-not, running/paused status — plus a new table tracking,
   per templated workflow, which specific source rows have already been
   processed (4.7). Decided against the real, current schema at build time,
   not a single flag layered onto an existing constraint.
2. **Source-reader interface, spreadsheet as its first implementation**:
   the general contract (current position, read next record, check
   exhaustion, detect drift) built first, with the spreadsheet reader as its
   one concrete implementation — not spreadsheet logic threaded directly
   through detection/mapping/replay. This is what keeps a future second
   source type a matter of writing a new reader, not a rewrite (Section 3).
   Source-position capture within this reader is scoped per the Section 3
   privacy resolution: held transiently during detection and each live run,
   never persisted as a durable position-plus-content pair.
3. **Pattern detection on stop**: the Rule-of-3 check described in 4.1,
   running once when recording ends.
4. **Qwen verification step**: pass the detected mapping to the existing
   local model for a sensibility check (4.1).
5. **Compile, extended**: store the mapping and advancement rule alongside
   the literal example steps captured during recording — additive to the
   existing schema, not a replacement.
6. **Replay, rebuilt for the iterative case**: the read → map → write →
   mark-done → check → repeat loop in 4.4, including the reversible/
   irreversible-based missing-field handling (reusing the existing safety
   pattern, not building a new one).
7. **Run controls**: real pause/stop mid-run, executing on the app's own
   background thread — non-blocking, survives minimize, does not survive a
   full app close (4.10). Genuinely new, since existing replay runs a fixed
   step list to completion with no pause point.
8. **New-batch detection**: the source-watching + confirmation flow in 4.8.
9. **Format-drift detection**: the before/during-run check in 4.5, for both
   source and destination.

## 6. Frontend build order

**Reuses the existing Stored Playbooks list — no separate view.** A
templated workflow shows up in the same list as every other saved playbook,
using the existing UI already built for browsing, running, and deleting
playbooks. What's new is behavior specific to templated entries (a repeat
icon or label, the run controls in item 4 below when one is actively
running) — not a parallel screen.

**Extends the existing review screen (4.12), not a new screen.** The
detected mapping is added as a new, editable section at the top of the
current review screen, alongside the existing captured steps — build this
as an extension of what already exists there, not a separate flow.

1. Post-stop prompt: *"This looks like a repeating pattern — want me to do
   the rest?"*, surfaced from the extended review screen above, with the
   detected mapping available behind a "See more" expansion (4.2).
2. First-record preview with confirm/cancel (4.3).
3. New-batch confirmation prompt (4.8).
4. Running-state overlay with real Stop and Pause controls (4.6).
5. The non-blocking click-to-point correction panel, including the
   confirm-what-you-selected step (4.5).
6. The "make this permanent?" follow-up after a one-off correction (4.5).
7. Clean, simple summary display, expanding only when something needs
   attention (4.9).

## 7. Explicitly out of scope for this version, and why

- **Unstructured sources** (webpages, freeform text, emails) — a genuinely
  different, harder problem (document understanding, not pattern-following);
  deliberately deferred rather than attempted alongside the structured case.
- **Live pattern detection while recording** — analyze-on-stop only, per the
  locked decision; live detection is a real future option, not this version.
- **Typed correction** ("type 'client' and have it understood") — click-only
  for this version, per the locked decision; typing is a real, well-scoped
  future addition once click-based correction is proven.
- **Self-healing format drift without asking** — detect-and-ask only;
  confident automatic re-mapping is a legitimate future direction once real
  usage shows how often and how drift actually happens, not attempted blind
  now.
