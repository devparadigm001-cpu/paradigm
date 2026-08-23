# A detail-view workflow has no spatial record axis

**Status:** open, found 2026-08-23 in session record-6db50bd4 (Gmail).
**Severity: HIGH for generality.** `detect::candidates` is built entirely on
positional identity, and there is a whole class of real workflow where the
position of the field being copied is **constant by construction**. On those,
the filter correctly finds nothing — and finding nothing is useless.
**Where:** `detect::candidates::assign_records`, and the premise underneath it.

## The measurement

A natural Gmail recording: open an email, copy its subject into a sheet, go
back, open the next. Three emails. Every distinct click position in the whole
session:

```
   3 x2252 y192     8 x2324 y212     8 x3002 y310
   1 x1996 y200     1 x510  y405     1 x0    y0
```

**Six positions for thirty actions**, and all eight subject captures share one:
`(2324, 212)`.

```
  2 click x2324 y212 w552  "W2W Fwd: Short staffed today!! (8/23)"
 10 click x2324 y212 w378  "60% OFF: Best Sellers Of The Summer"
 22 click x2324 y212 w321  "Don't forget your $25.00 bonus"
```

Same x, same y, three different emails. The **widths differ** (552, 378, 321),
which is the tell: this is the subject *text* element, sized to its content, and
it sits at the top of the reading pane. Open a different email and the same slot
holds different text.

## Why the funnel is right to return nothing

```
stage 0  raw actions                  : 30
stage 1  after dropping Navigate      : 24  (-6)
stage 2  with a cell ref or a position: 24  (-0)
stage 3  distinct field groups        : 1
stage 4  surviving the Rule of 3      : 0
```

The 22 page clicks yield **zero** groups: `record_pitch` over
`{0, 192, 200, 212, 310, 405}` finds no period covering those unrelated UI
positions, so `assign_records` declines. Correct — there is no repeating spatial
structure to find.

The single surviving group is the spreadsheet column, with **two** records
(`A2`, `A3`), which is below the Rule of 3. Also correct.

**Nothing is broken.** Every stage did the right thing, and the answer is still
useless, because the premise does not hold on this surface.

## What this is NOT

* **Not the walk cap.** `element_bounds` comes from the Click event's own
  element, not from the 3000-node walk. The cap declined the three *source*
  reads — `0 pair(s)`, as predicted — but the candidate path had full data.
* **Not the label defect.** No candidate surfaced to be mislabelled.
* **Not the pitch floor.** Gmail's inbox rows really are 28px apart, and the new
  rule resolves that correctly — but those rows were never clicked. The pitch
  never entered the data.

## The shape of the answer: records separated in TIME, not space

This project already has the discriminator. The temporal rule measured on
2026-08-20 — records are the outer loop of a task and fields the inner, so the
record axis cuts the step sequence into blocks that do not interleave — scored
**0% overlap on the correct axis and 100% on the wrong one** on OrderFlow.

Here it is exactly what is needed. The three subjects are consecutive in
`step_order` and separated by navigations; they are three records that happen to
share one position. Space says one record, time says three, and time is right.

So `assign_records` needs a sibling: when position does not separate records,
**step contiguity can**. That is a real piece of work, not a tweak — it needs a
rule for when to trust which axis, and preferring time unconditionally would
break OrderFlow, where a user scanning down a list produces temporally
interleaved clicks on genuinely distinct records.

## Reproducing

1. Record: open a record in a detail view, copy a field, go back, repeat 3x.
2. Save it, then:
   `cargo run --example candidates_from_recording -- <id> %APPDATA%\com.amitj.paradigm`
3. `distinct click positions` far below the number of records touched, and
   `ZERO groups from clicks`, is this issue.

## Related

* `docs/planning/Filtered-Post-Hoc-Confirmation.md` — the pipeline, whose
  positional premise this bounds.
* `docs/known-issues/gmail-as-a-source-what-the-tree-exposes.md` — the earlier
  Gmail investigation, which found no per-record container. This is a different
  and larger problem: no per-record *position*.

## Reproduced on Amazon the same day

Session record-6ca9bd5a, three products, opened one at a time. The Amazon page's
own click positions yield **no pitch at all** — same outcome as Gmail, same
cause: the user opened each record rather than reading a visible list, so the
fields were captured at whatever position the detail view puts them.

Two surfaces, two different applications, one shape. "Open a record, act, go
back" is not an edge case; it is how people use an inbox and a search-results
page alike.

Amazon nonetheless surfaced five candidates where Gmail surfaced none, and the
difference is **entirely on the destination side**: Amazon's session typed nine
values (three columns x three rows), while Gmail's typed two (`A2`, `A3`) and so
fell one short of the Rule of 3. Nothing about Amazon's page structure helped.
Its two page-side candidates are artifacts of cross-window pooling — see
`an-action-cannot-say-which-window-it-happened-in.md`.
