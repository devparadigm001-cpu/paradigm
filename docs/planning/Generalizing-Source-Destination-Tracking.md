# Generalizing Source/Destination Tracking Beyond Spreadsheets

**Status:** direction confirmed 2026-08-16, grounded in real, direct evidence
from four separate application investigations. Partly built as of 2026-08-17
(Steps 2a and 2b). This document exists so the decision and its evidence
survive past this conversation.

**For the current standing of the work -- what is general, what has actually
been proven and to what depth, and why "works on any site" is not a state that
can be reached -- see §9. Read that section before describing this work's
coverage anywhere.**

---

## 1. The problem, stated precisely

The current source-position tracking mechanism (`SourceLink`, built during
the templated-workflows effort) works by pairing a Ctrl+C with reading a
spreadsheet's **Name Box** — a UI element that happens to display "which
cell is selected" as plain text. This is not a general mechanism. It is a
spreadsheet-specific trick that works *because* spreadsheets uniquely expose
that information in a readable way.

Tonight's mock-dashboard test proved this directly: recording a copy from a
non-spreadsheet page produced zero source links, not because anything was
broken, but because there is no Name Box to read on a dashboard, an email,
or any other kind of page. The mechanism was never built to answer "where
did this click happen" for anything other than a spreadsheet cell.

**Explicit, non-negotiable requirement:** cell-reference reading (the Name
Box mechanism, `resume_destination_row`, row/column-shaped position
tracking) must not remain the detection mechanism. It is a workaround built
for one application's specific limitation, not the intended design. The
replacement must operate on raw element identity and user keystrokes/clicks
— what the user actually did, on whatever page they did it on — never on
any spreadsheet-specific concept. This is confirmed, not optional, and
applies even to the existing Sheets case: the target end-state is one
general mechanism that happens to also handle spreadsheets, not a
spreadsheet mechanism with exceptions bolted on for other apps.

## 2. The real goal, restated

Detection should work off **element identity**, not cell references: when a
user copies or clicks something, record a stable reference to *that exact
element*. When they later type or paste somewhere else, record a stable
reference to *that* element too. If the same source-element-to-destination-
element relationship repeats a few times, that's the pattern — regardless of
whether either side is a spreadsheet, a dashboard, an inbox, or anything
else.

This is not a harder problem than the Sheets case — it's a *more general*
one. The Name Box trick existed specifically because Sheets' own grid is
canvas-rendered and barely exposes anything directly. Every other
application investigated tonight (Gmail, Amazon, the mock dashboard) already
exposes its fields as real, individually readable elements — the hard part
for those apps was never *reading* them, it was **knowing which element
identity to trust**.

## 3. Real evidence from tonight — the same trap, four times, in four different shapes

Every application investigated this project has hit some version of "an
element looks reliable and isn't." This is the evidence a general identity
mechanism has to be built against, not a hypothetical:

| App | The trap | Concrete example |
|---|---|---|
| Sheets | A second, empty Name Box sat nearest to the real one and got picked instead | Formula bar mis-selection, `require_formula_bar` bug |
| Gmail | Structural cells share one id across every row; only content-bearing cells have distinct ids | Star-cell id `601850` identical on every row |
| Amazon | A label's id is shared across every listing; only the value has a distinct id — and even values can coincidentally repeat | Price label id `118404` shared; `$145.00` legitimately appears twice with different meanings |
| Mock dashboard | Field *labels* share one id per field type across all seven orders; field *values* have distinct ids | `PRODUCT` label id `168734` identical on every card; `"Ceramic Mug Set"` has its own distinct id |

**The pattern across all four:** structural/label elements are unreliable
(shared ids, generic names, repeated across instances). Content/value
elements are consistently more reliable (distinct ids, distinct text). A
general identity mechanism should prefer the second category and treat the
first as a stable *role* label, not a source of tracked identity.

## 4. What already generalizes, and can be reused directly

- The `SourceReader` interface — already built generically ("a source," not
  "a spreadsheet"), with the Sheets reader as its first implementation.
- Capture's existing per-action element identity — **only partly, and not the
  part that matters.** Role and name are recorded for every click and for a
  text-field type. A *grid* type records neither: `capture::grid::emit`
  synthesises `element_role: "ComboBox"` and puts the cell reference in
  `element_name`, so that action carries no element identity at all. And **no
  runtime id is recorded on any action** — `CapturedAction` has no id field,
  and `to_candidate` holds the event's `UIElement` and takes only role and
  name from it. Ids *are* read transiently, by `read_element_position` on the
  clipboard path and by `capture::text` to compare elements, but none is
  stored.

  *Corrected 2026-08-18.* This bullet previously read "(role, name, runtime id
  where available) … already being recorded for every click and type action".
  The id half was false. That matters because the id is exactly what the
  structural rule in `identity::tree` keys on, so this is raw material the work
  has to **add**, not reuse. See §9.4 for a second, independent reason the id
  is weaker material than this bullet assumed.
- The Rule-of-3 confirmation logic, the reversible/irreversible safety
  model, pause/resume, fail-loud-never-guess — none of this is
  spreadsheet-specific and all of it should carry over unchanged.

## 5. What's genuinely new

1. **A general element-identity capture mechanism** for both source and
   destination, independent of app type — not Name-Box-based, built on the
   actual accessibility-tree element identity already available in capture.
2. **A rule distinguishing structural elements from content elements**,
   informed directly by the shared-id pattern found four separate times
   tonight (Sections 3 above) — since a naive "trust any id" approach would
   repeat the exact failure already found in every single app tested.
3. **A generalized "is this the next new record" signal**, replacing the
   current row-number-based advancement logic. This is the same open
   question already left honestly unresolved for Gmail — it does not yet
   have a real answer, and this work does not pretend otherwise.

## 6. What this document does not do

It does not pick the exact technical mechanism (e.g., which specific
accessibility-tree properties to prefer, how to formally distinguish
structural from content elements). That's real design and investigation
work for the next session — this document exists to make sure the direction
and its supporting evidence aren't lost, the same way every other real
decision in this project has been written down rather than left in chat.

---

## 7. Decisions taken after this document was first written

### 7.1 A non-uniform walk is valid advancement (decided 2026-08-17)

**Previously undecided.** Sections 1–6 above never addressed what should
happen when a recording's examples are not evenly spaced — when a user
processes some records, skips one, and carries on.

**Decision: a non-uniform walk counts as valid pattern advancement, and is
not grounds to reject the pattern.**

**Reasoning.** A skipped record is still evidence of a genuinely repetitive
task. The framing this rests on is §2 above — "if the same
source-element-to-destination-element relationship repeats a few times,
that's the pattern" — and the Foundation design's §1, "recognize the pattern
and continue it." Neither is a claim about *spacing*. What makes a recording
a pattern is that the same relationship recurs across distinct records; the
distance between those records is an artifact of the substrate, not part of
the evidence. Uniform spacing is a property grids happen to have, and
requiring it imports a spreadsheet assumption into the general mechanism —
exactly what §1 forbids.

*(A note for whoever reads this next: the decision as given cited "Section
1.2". No §1.2 exists in this document or in the Foundation design; the
supporting framing is §2 here and §1 there, cited above. Recorded so the
reference does not send someone hunting for a section that was never
written.)*

**What the current code actually does, measured rather than assumed**
(`text_capture_probe skiptest`, run against the real `detect::detect` and a
real encrypted ledger):

```
control      2,3,4,5 -> 2,3,4,5   => Pattern  source_step=1 examples=4
skipping     2,3,4,6 -> 2,3,4,5   => InconsistentAdvance
                                       source_steps: [1, 1, 2]
                                       destination_steps: [1, 1, 1]
first three  2,3,4   -> 2,3,4     => Pattern  source_step=1 examples=3
```

So today the skipping recording is **rejected at confirmation** and can never
be saved. The rejection is specifically the gap — the same recording without
the skip detects cleanly, and its first three examples alone detect cleanly.

**The skipped record is not lost by the ledger.** With rows 2, 3, 4 and 6
marked processed and a source holding rows 2–8, `run::batch::scan` returns:

```
Found { count: 3, first_row: "5" }
```

Row 5 is the *first* record offered. Both `scan` and the run loop walk record
by record calling `is_processed` on each position; neither extrapolates from
`first_row + processed × step`. The step arithmetic governs only whether a
recording is **accepted** and where destination rows **land** — never what
counts as unprocessed.

**Status.** The new mechanism already implements this decision:
`identity::prove_advance` replaces difference with distinctness, and
`an_uneven_walk_still_counts_as_advancing` pins rows 2, 5, 9 as advancing.
`detect::detect` is **unchanged** and still rejects — consistent with the
agreed sequencing, where the new mechanism lands behind its own tests and the
cutover happens last with a live re-verification as the acceptance gate.

## 8. Tracked open items

### 8.1 A run with a step greater than 1 never checks intermediate records

**Found during the §7.1 investigation. Distinct from that decision, and not
resolved.**

`run::advance_source` advances the reader `source_step` times between records.
With `source_step = 1` the run visits every record and calls `is_processed` on
each. With `source_step > 1` it steps *over* the intermediate records without
ever calling `is_processed` on them — so those records are invisible to a run,
no matter what the ledger says about them.

A `scan` starting from the top **does** see them, because it walks one record
at a time. So the two paths disagree about which records exist:

| | visits every record | honours the ledger per record |
|---|---|---|
| `batch::scan` | yes | yes |
| run loop, `source_step = 1` | yes | yes |
| run loop, `source_step > 1` | **no** | only for records it lands on |

That is a real inconsistency between "what a scan offers" and "what a run
will process", and it is reachable today by any template whose detected step
is greater than 1.

Not yet resolved, and deliberately not bundled with §7.1 — that decision is
about which recordings are *accepted*, this is about which records a run
*reaches*. Fixing one does not fix the other. Worth noting that the general
mechanism removes the notion of a numeric step entirely, so this may dissolve
rather than need a separate fix — but that is an expectation, not a result,
and it should be verified rather than assumed at cutover.

---

## 9. Status of generality: what is proven, and why it is never finished

**Written 2026-08-18. A standing status statement, meant to be amended as
surfaces are added. It is worded deliberately so that it cannot be read as a
completion claim, because there is no state in which it would become one.**

### 9.1 The mechanism is general and app-agnostic

**No file in the identity or detection path contains logic for any specific
application.** There is no branch on "if Gmail", no allowlist of roles, no
per-site label table, no application name at all in:

* `identity::tree` -- `classify`, `walk`, `locate`, `records`. The only inputs
  are a document-order node list and its multiplicity.
* `identity` -- `RecordKey`, `prove_advance`, `next_unprocessed`,
  `FieldAddress`. Order-free and arithmetic-free by construction.
* `detect` -- `detect`, `link::observations`, `link::dominant_surfaces`. These
  deal in fields and record keys. A spreadsheet reaches them only after the
  spreadsheet adapter has already turned `"B2"` into a field and a record.

The fixtures in those modules' tests are real captures from separate
investigations, kept as *evidence that the rule generalizes* -- not as things
the code matches against. Deleting every fixture changes no behaviour.

### 9.2 What has actually been shown, surface by surface

Four surfaces, and four different degrees of proof. "Proven" is not one thing,
and this table exists so that it is never reported as though it were.

| Surface | Real capture | Carried as far as | Not done |
|---|---|---|---|
| Gmail message list | yes -- real ids, incl. the star cell `601850` shared by every row | the grouping rule only (`identity::tree` unit test) | end-to-end detection; live recording |
| Amazon listings | yes -- real ids, incl. the price label `118404` shared by every listing | the grouping rule only (unit test) | end-to-end detection; live recording |
| OrderFlow dashboard | yes -- full card tree with ids | the whole chain, offline: nodes -> `locate` -> `encode_element_ref` -> `observations` -> `detect` -> `Pattern` | the live recording, which needs a human's hands |
| PDF (Edge's viewer) | yes -- probed 2026-08-18 via `pagetree` | tree *shape* only: each value is its own `Text` element with an id, in document order, under a `Group "Page 1"` | the grouping rule; end-to-end detection; live recording; every non-Edge viewer |

### 9.3 Google Sheets is not a fifth proof point -- it is the standing exception

Sheets must not be listed as evidence for the general mechanism, because the
general mechanism cannot read it. Its grid is canvas-rendered: the whole window
is ~56 nodes and the document subtree is two, `Document > Pane`. There are no
per-cell elements, so there is nothing whose id could be counted.

Sheets is therefore served by two *separate* paths, both deliberate:
`read_position` resolves a spreadsheet through the Name Box and returns before
`read_element_position` is ever reached, and record reading is done from the
CSV export rather than from the tree. §1's requirement -- that the Name Box
trick must not remain *the* detection mechanism -- is satisfied by it no longer
being the only one, not by it having been removed.

### 9.4 One caveat that qualifies every row in 9.2 (found 2026-08-18)

`identity::tree`'s rule is "an id occurring more than once is structural; an id
occurring exactly once is content", justified on the grounds that ids separate
two coincidentally-equal values and names do not.

**On Windows that justification does not hold.** `terminator-rs`'s
`generate_element_id` hashes `automation_id + role + name + class_name`
(`platforms/windows/utils.rs:23`), then truncates to six characters
(`element.rs:464`). Where `automation_id` and `class_name` are empty -- which is
the case for the text nodes on *every* surface in 9.2 -- the id is a hash of the
text. Measured three ways on 2026-08-18: a PDF built with the dashboard's
strings reproduced the dashboard's ids exactly, across a different format and
renderer; the source above states the mechanism; and a capture in which two
orders shared a product had both instances collapse to one id.

The consequence is that two records that legitimately share a value have that
value classified **structural** and refused by `locate` -- which is precisely
the `$145.00` trap in §3's table, documented as avoided and in fact not avoided.
`tree.rs`'s `records_are_not_merged_when_two_values_coincide` passes only
because its fixture gives the two identical values two different ids, which the
platform never does.

So every row in 9.2 shows that the rule generalizes *across applications*. None
of them shows that it is correct *within* an application whose records can
repeat a value. That is an open defect, not a caveat to be worked around.

### 9.5 Coverage of "any site" is not a state that can be reached

**This is the part that must never be softened into a completion claim.**

The mechanism being app-agnostic is a property of the *code*. It is not a
guarantee about the *world*. Every application investigated so far has produced
some version of "an element looks reliable and is not" -- four different shapes
of it in §3, and a fifth on 2026-08-18 in §9.4 -- and each one was found only by
capturing that application and looking. None was predicted from the ones before.

It follows that:

* **"Works on any site" is not a milestone, and there is no build that
  achieves it.** Support is established one real application at a time, by
  capturing it, running the rule against what came back, and recording what
  broke.
* **A surface is only as proven as the row in 9.2 says it is.** Reaching the
  grouping rule is not the same as reaching a detected pattern, and neither is
  the same as a live recording.
* **Adding a surface can invalidate earlier ones.** §9.4 was found on the fifth
  surface investigated and applies retroactively to the first four. Anything
  written here is provisional in that direction, permanently.
* **The correct way to describe this work, internally or to a user, is by
  naming the applications it has been verified against and the depth of that
  verification** -- never as general coverage, and never with a number of
  supported sites, which would imply a countable set that closes.

The honest summary, and the one to reuse: *the mechanism is general by
construction and carries no per-application logic; it has been exercised
against four real applications to four different depths, with one open defect
affecting all of them; and it is extended and re-verified one real application
at a time, indefinitely.*
