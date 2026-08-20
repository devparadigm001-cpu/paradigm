# Filtered post-hoc confirmation

**Status:** prototyped and measured 2026-08-19. Not built into the product.
**Prototype:** `src-tauri/examples/confirmation_prototype.rs`.
**Supporting evidence:** `src-tauri/examples/meaning_probe.rs`,
`src-tauri/examples/field_name_probe.rs`.

## The proposal

Recording requires **no marker at all** — no `Ctrl+C`, no `Ctrl+Shift+M`. After
Stop, the raw action stream is filtered down to a small set of real candidates,
and the user confirms or unchecks them.

The filter decides **what to ask about**. It never decides the answer.

## Why this shape, and not the alternatives

Three approaches were tested against the same case first, and all three failed
on it: a *systematic incidental* click — a user checking the same field on every
record — which is indistinguishable from a meaningful one.

**Pure structural inference** (2026-08-18). Feature vectors for a meaningful
field and a systematically-checked one came out **identical on every
component**: distinct records, clicks per record, addressing-element kind, label
multiplicity, value distinctness. No threshold can separate equal inputs.

**A model judging intent** (`meaning_probe`, 2026-08-19). The local model
answered confidently and stably, but its confidence went the *wrong way*:
decidable cases 0.8910 mean (range 0.8658–0.9314), undecidable 0.9162
(0.8898–0.9461). To flag all undecidable cases the threshold must exceed 0.9461,
which also flags 4/4 decidable ones; to spare all decidable ones it must fall
below 0.8658, which flags none. **No separating threshold exists**, so
"escalate on low confidence" cannot route these regardless of what sits on the
other side. This independently reproduces the note already on
`detect::verify::CONFIDENCE_FLOOR`: *"uniformly confident whether it is right or
wrong"*.

**A model naming fields** (`field_name_probe`, 2026-08-19). Different problem,
different wall. Reordering the *same* neighbouring tokens moved the answer —
`555-0142` was named `EMAIL`, then `ADDRESS`, then `EMAIL`; a company name was
named `ORDER`, `NAME`, `PRODUCT`. The model returns a neighbouring word rather
than classifying the value, and never once said `PHONE` for a phone number.
Naming was also unstable across records (`ORDER`, `COMPANY`, `COMPANY` for one
column), which alone breaks `detect`, since it groups by `source_field`.

The conclusion those three share: **nothing can infer intent from the
recording.** So the design stops trying, and asks.

## The rules, all reused

| | rule | source |
|---|---|---|
| 1 | Rule of 3, over **distinct records** | `detect::detect` step 4 |
| 2 | Structural exclusion | `identity::tree::classify` — see limits |
| 3 | Non-trackable actions never enter the stream | `capture::to_candidate` |
| 4 | Positional identity for grouping | `CapturedAction::element_bounds` |

**Rule 3's premise needed correcting.** Only one third of it holds:

| | enters the action stream? | |
|---|---|---|
| scroll / mouse wheel | **no** | `WorkflowEvent::Mouse` → `_ => None` |
| window navigation | **yes** | `ApplicationSwitch` → a `Navigate` action |
| incidental clicks | **yes** | `Click` → a `Click` action |

The real recording bears this out: 5 `Navigate` steps and a pile of incidental
clicks among its 37.

## The pipeline

0. raw action stream
1. drop `Navigate` — context, never a field candidate
2. drop actions carrying no usable identity
3. group by **field**: a spreadsheet cell by its column (`parse_cell_ref`), a
   page element by its x-band from `element_bounds`
4. keep groups covering **≥3 distinct records**

## Measured

### Fixture A — REAL: the 37 steps of playbook "d test"

```
stage 0  raw actions                    : 37
stage 1  after dropping Navigate        : 32  (-5)
stage 2  after dropping unidentifiable  : 15  (-17)
stage 3  distinct field groups          : 8
stage 4  surviving Rule of 3            : 0
```

Zero, and **correctly**: that recording contains one record (row 2 only), and
the Rule of 3 is over records. Live detection said the same — *"only 1 record
were copied across"*. It also means the real capture cannot answer the tedium
question, which is why fixture B exists.

Its measured noise ratio, which fixture B reuses: **14% Navigate, 46% unnamed
group clicks, 41% carrying a usable name.**

### Fixture B — MODELLED: three records, A's shape and noise ratio

```
stage 0  raw actions        : 54
stage 4  Rule of 3 survivors: 7      -> 13.0% survive
```

Of those seven, five are spreadsheet columns. Typing a value into a cell **is**
the act of transferring it, so those are not genuinely in question. The
ambiguous items are the two page-element clicks: **2 real decisions from 54 raw
actions (3.7%)**, with five more shown as confirmable context.

### The systematic incidental appears — and only because of bounds

The first run grouped page elements by **name**, and the systematic incidental
did not appear. Neither did the meaningful source click. Both were dropped:
values differ across records by design, so name-grouping gives every record its
own group of one. That is the empty-label defect
(`an-element-identity-mark-records-no-field-label.md`) arriving from a new
direction, and it would have made the approach useless on the source side.

Grouping by the x-band from `element_bounds` fixed it:

```
[ ] Click on text at x100-300    3 records, 3 actions   (truth 0M/3I)   <- systematic incidental
[ ] Click on text at x320-520    3 records, 3 actions   (truth 3M/0I)   <- meaningful
```

Correctly present, not buried, and **indistinguishable to the filter** — same
record count, same action count, same shape.

That is the design working, not failing. The filter cannot tell them apart and
does not try; it presents both and the user ticks one. Repetition is used as a
**filter on the question set**, never as an oracle, which is the structural
difference from every approach that failed above.

## Honest limits

* **Fixture B is modelled.** Its shape and noise ratio come from the real
  recording; its three records do not.
* **Its bounds are modelled too.** The real recording predates `184e6ce`, so it
  carries none. Positional grouping has not been tested against captured bounds.
* **Structural exclusion is approximated.** `identity::tree::classify` needs the
  page tree, which is not stored with the action, so stage 2 uses "carries no
  usable name". That removed 17 of 37 actions in the real data, so it does most
  of the work — but it is not the `classify` rule and must not be described as
  it.
* **Every element-identity caveat still applies**, including that a mark on a
  non-spreadsheet surface costs ~432ms and that `page_identity` falls back to a
  window title.

## The next step

**Capture one real three-record recording under the current build** — which now
records `element_bounds` — and re-run this exact pipeline against it. That
confirms or corrects the 13% and 3.7% figures with genuine data, and is the
first test of positional grouping against bounds that were actually measured
rather than modelled.

Until then the pipeline is proven on real data only for the case that yields
zero candidates, and the interesting numbers are a projection.
