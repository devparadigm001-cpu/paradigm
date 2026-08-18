# The element id is a hash of the text, so two records sharing a value collapse

**Status:** open, real defect. Found 2026-08-18 while probing a PDF as a source.
**Severity: HIGH.** It silently misclassifies a record's *value* as page
*structure*, which removes that field from every record that shares the value
and shifts the record ordinals of the ones that remain.
**Where:** `identity::tree::classify` / `walk` / `locate` / `records`, resting on
`terminator-rs`'s `UIElement::id()`.
**Unrelated to PDFs** — the PDF is only where it was noticed. It applies to every
surface this project has measured, and applies retroactively to all of them.

## The rule this breaks

`identity::tree` reconstructs records with one rule and no other input:

> An id occurring more than once in the tree is **structural**. An id occurring
> exactly once is **content**.

Its module docs justify that choice explicitly, and name the alternative it
rejected:

> *"Names cannot do this job. A sender name legitimately repeats across Gmail
> rows, and a price legitimately repeats across listings — the planning document
> records `$145.00` appearing twice with two different meanings. **Ids separate
> the two cases and names do not.**"*

On Windows the final sentence is false. The id **is** the name.

## The mechanism

`terminator-rs` does not surface a platform runtime id. It synthesises one
(`src/platforms/windows/utils.rs:23`):

```rust
pub fn generate_element_id(element: &uiautomation::UIElement) -> Result<usize, …> {
    let automation_id = …; let role = …; let name = …; let class_name = …;
    let mut to_hash = String::new();
    if let Some(id) = automation_id { to_hash.push_str(&id); }
    if let Some(role)  = role       { to_hash.push_str(&role.to_string()); }
    if let Some(n)     = name       { to_hash.push_str(&n); }
    if let Some(cn)    = class_name { to_hash.push_str(&cn); }
```

and then truncates it (`src/platforms/windows/element.rs:464`):

```rust
fn id(&self) -> Option<String> {
    Some(self.object_id().to_string().chars().take(6).collect())
}
```

Where `automation_id` and `class_name` are empty, the hash input is
**role + name**. Two elements with the same role and the same text therefore
receive the same id, by construction, no matter where they sit or how unrelated
they are.

## The evidence

Three independent lines, all gathered 2026-08-18.

### 1. The same ids appeared in a different document, format and process

A PDF was generated containing the same strings as the OrderFlow card tree, then
dumped with `text_capture_probe pagetree`. Edge's viewer reported:

```
  Text  "Order RS-1001"          id=140747
  Text  "Pending"                id=121421
  Text  "Harbor Point Traders"   id=956010
  Text  "PRODUCT"                id=168734
  Text  "Ceramic Mug Set"        id=136415
  Text  "QUANTITY"               id=967325
  Text  "12"                     id=287472
```

Every one of those is **digit-for-digit identical** to the fixture in
`detect/link.rs`'s `a_real_card_tree_detects_end_to_end`, which came from a real
`pagetree` capture of an HTML dashboard. Different file format, different
renderer, different process, same ids.

Two distinct documents cannot share non-empty `automation_id`s by coincidence, so
this is also the evidence that `automation_id` and `class_name` are empty on
these nodes — the inference, stated as such, rather than a direct read of those
two properties.

### 2. The source says so

`generate_element_id` above. This is not behaviour inferred from outputs alone;
the hash input is written down.

### 3. Duplicate values collapse, reproduced directly

A second PDF, identical except that orders 1 and 2 both contain
`"Ceramic Mug Set"` and quantity `12`:

```
  Text  "Ceramic Mug Set"   id=136415     <- order 1
  Text  "Ceramic Mug Set"   id=136415     <- order 2, same id
  Text  "12"                id=287472     <- order 1
  Text  "12"                id=287472     <- order 2, same id
```

## What it does to record detection

The shipped `classify` / `locate` / `records`, run against that real capture:

```
  STRUCTURAL x2  "Ceramic Mug Set"      <-- a VALUE misread as structure
  STRUCTURAL x2  "12"                   <-- a VALUE misread as structure

  product, order 1  ("Ceramic Mug Set")   -> REFUSED (classified structural)
  product, order 2  ("Ceramic Mug Set")   -> REFUSED (classified structural)
  quantity, order 1 ("12")                -> REFUSED (classified structural)
  quantity, order 2 ("12")                -> REFUSED (classified structural)

  records() believes the page contains:
    record 0:  Pending = "Harbor Point Traders"    12 = "Order RS-1002"
    record 1:  Pending = "Ashgrove Manufacturing"  12 = "Order RS-1003"
    record 2:  Pending = "Windmere Consulting"  PRODUCT = "Ergonomic Office Chair"
               QUANTITY = "2"
```

Four of the six copyable values are refused. `"12"` is promoted to a *field
label*. Values are attributed to the wrong orders. `"Ceramic Mug Set"` — the
actual product on two of the three orders — does not appear at all.

**In this instance it fails closed**, because only one record survives and
`detect` returns `TooFewExamples`. That is luck, not design. The record
*ordinals* are corrupted, so a page with more records produces a confidently
wrong mapping rather than a refusal — the silent-wrong-target class this project
keeps finding.

## Scope

Windows, `terminator-rs` 0.23.35, any node whose `automation_id` and
`class_name` are empty.

Every surface whose records this project reconstructs *from the tree* is in that
category — the four in the planning document's §3 table. (Sheets is not: its grid
is canvas-rendered and exposes no per-cell elements at all, so it is served by
the Name Box and the CSV export and is untouched by this.) The fixtures already
in the repository show the same signature — identical text sharing an id,
distinct text not:

| Surface | Shared id | Text |
|---|---|---|
| Gmail | `601850` | `"Not starred"`, on every row |
| Amazon | `118404` | `"Price, product page"`, on every listing |
| OrderFlow | `168734` | `"PRODUCT"`, on every card |
| PDF | `121421` | `"Pending"`, on every order |

For Gmail and Amazon that is consistent with content-hashing rather than an
independent measurement of it; the direct cross-document proof was obtained on
the dashboard/PDF pair.

**Note the irony worth keeping:** the rule *appears* to work on all four
surfaces precisely because it is hashing the text. Labels repeat, so labels
collapse to one id and read as structural. The rule has been getting the right
answer for the wrong reason, and only a repeated *value* separates the two.

### A second, unquantified hazard in the same function

`.chars().take(6)` keeps the first six characters of the **decimal** rendering of
a `usize` hash, whose length varies. Two unrelated elements whose hashes agree in
their leading digits collide, and the chance grows with tree size —
`read_element_position` allows up to 3000 nodes. Not measured, and not the defect
above; recorded so it is not rediscovered as the same thing.

## The test that should have caught this

`identity::tree`'s `records_are_not_merged_when_two_values_coincide` is named for
exactly this trap and does not test it:

```rust
let tree = vec![
    n("100", "Text", "PRICE"),
    n("201", "Text", "$145.00"),
    n("100", "Text", "PRICE"),
    n("202", "Text", "$145.00"),
];
```

Two identical values are given two *different* ids (`201`, `202`), which the
platform never does. The fixture is unrealistic in the one dimension the test
exists to check, so it passes and provides false assurance about the project's
most-cited trap.

## What a fix must not do

**It must not key identity on text with extra steps.** Adding role, bounds or a
parent's name to the hash narrows collisions without removing the class: any
scheme derived from what the element *says* collapses two records that say the
same thing.

**It must not silently prefer the first occurrence.** Picking one of two
colliding elements produces a record attribution that is wrong half the time and
reports nothing.

**It must not be assumed to dissolve under a different automation library.** The
collapse is `terminator-rs`'s synthesis, so a direct `uiautomation` path may
expose a genuine `RuntimeId` — but that is an expectation. It should be measured
on a real surface before anything is built on it.

The likely direction is that **document-order position is the identity**, since
`walk` already assigns record ordinals positionally and needs no id to do it. The
id would then be used for addressing only, where collisions are harmless.

## Reproducing

Reads only; clicks nothing, types nothing.

1. Build an HTML page with three records where two share a field value.
2. `msedge --headless --disable-gpu --no-pdf-header-footer --print-to-pdf=dupes.pdf file:///…/dupes.html`
3. Open `dupes.pdf` in Edge, frontmost.
4. `cargo build --example text_capture_probe --no-default-features`
5. `./target/debug/examples/text_capture_probe.exe pagetree "dupes.pdf" "Order RS"`

The `full contents of the first 3 cards, every accessor` block prints each node's
id. The two records sharing a value share an id.

## Related

* `docs/planning/Generalizing-Source-Destination-Tracking.md` §3 — the
  four-application evidence table this rule was built on, and §9.4, which records
  this defect against the generality status.
* `identity::tree` module docs — the rule and the justification this falsifies.
* `docs/known-issues/complex-web-grid-capture-unreliable.md` — the same family:
  an element that looks reliable and is not.
