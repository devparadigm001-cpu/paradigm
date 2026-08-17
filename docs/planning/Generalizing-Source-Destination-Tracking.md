# Generalizing Source/Destination Tracking Beyond Spreadsheets

**Status:** direction confirmed tonight, grounded in real, direct evidence
from four separate application investigations. Not yet built. This document
exists so the decision and its evidence survive past this conversation.

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
- Capture's existing per-action element identity (role, name, runtime id
  where available) — the raw material this needs, already being recorded
  for every click and type action, just not yet used for cross-app pattern
  detection.
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
