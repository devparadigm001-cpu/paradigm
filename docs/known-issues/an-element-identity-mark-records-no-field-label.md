# An element-identity mark records a position with a missing or WRONG field label

**Status:** open, real defect. Found 2026-08-19 in the first live test of the
`Ctrl+Shift+M` source marker. **Severity escalated 2026-08-22.**
**Severity: HIGH**, raised from MEDIUM-HIGH.

The original defect was an **empty** field: position and record ordinal right,
field `""`, and a mapping built from it unreadable. The escalation is that the
label is not always empty — it can be **confidently wrong**, adopting a repeated
value such as an order status as the name of the field beside it
(`el/0/Pending`).

An empty label announces that it does not know. A wrong one does not, and it now
reaches the user, because `detect::candidates` renders candidates on the review
screen. Silent-wrong-target is the failure class this project keeps finding, and
this is that class arriving in the label.
**Where:** `capture::grid::read_element_position` → `identity::tree::locate` →
`detect::link::surface_and_cell`.
**Not** a spreadsheet problem. The Name Box path is unaffected and produced a
correct `cell C2` in the same session.

## What happens

Three marks, one per surface, from the live session:

```
[paradigm] source mark @1787165981805: armed via Name Box, cell C2
[paradigm] source mark @1787165990549: armed via element identity, el/18/
[paradigm] source mark @1787165998696: armed via element identity, el/1/
```

`el/18/` and `el/1/` are `el/<ordinal>/<label>` with **the label missing**. The
ordinal is there; the field name is not.

## Why it matters downstream

`detect::link::surface_and_cell` decodes that reference into a position:

```rust
if let Some((ordinal, label)) = crate::capture::grid::decode_element_ref(reference) {
    return Some((
        document.to_string(),
        Cell { field: label, record: RecordKey::declared("ordinal", ordinal.to_string()) },
    ));
}
```

So `Cell::field` becomes `""`. A `Pattern` built from such observations carries
`FieldMapping { source_field: "", destination_field: "A" }`, which surfaces to
the user as a mapping from nothing to a column, and gives a run nothing to look
for on the page.

Worth being precise about the limit of this claim: **what the run loop actually
does with an empty source field has not been traced.** What is established is
that the field is empty and that the mapping is not human-readable. Whether it
fails loudly or quietly at run time is unknown and should be checked before any
fix is designed around an assumption about it.

## The mechanism

`identity::tree`'s walk attaches a label to content by adjacency: the structural
element most recently seen labels the content that follows it. Content with no
structural element before it is *unlabelled* — a real and intended outcome,
recorded as `RecordView::unlabelled` and described in the module docs as the
natural Tier 2 identity candidate.

`locate` reports the same thing as `Located { record, label: None }`, and
`encode_element_ref` renders `None` as an empty label segment.

So nothing is malfunctioning. The rule is behaving exactly as written; it is the
**input** that differs from what the design was tested against.

## Why the offline proof did not catch this

`detect::link`'s `a_real_card_tree_detects_end_to_end` produces clean
`PRODUCT → A` and `QUANTITY → B` mappings, and it runs on a **real** capture. But
that fixture is 21 nodes — a card tree trimmed to the records themselves. Every
value in it sits directly after its label.

A live page is not trimmed. The mark that returned `el/18/` was the nineteenth
unlabelled content node on the page, which is what a real dashboard looks like
once navigation, headers and chrome are included.

This is the same shape as
`docs/known-issues/element-id-is-a-hash-of-the-text.md`: a rule that looked
correct against a curated fixture, and behaves differently against the live
surface the fixture was taken from.

## What has never been observed: a failed mark

**Recorded as an honest gap rather than a result.** `MarkOutcome::Unavailable`
— the `NOT armed` line — has **not once** appeared in live use. Three marks
across three deliberately different surfaces all armed, including Notepad, which
was predicted to fail.

The reason is `page_identity`'s fallback:

```rust
url.or_else(|| {
    let title = root.name().unwrap_or_default();
    (!title.trim().is_empty()).then_some(title)
})
```

With no URL it falls back to the **window title**, and nearly every window has
one. So a mark almost always resolves *something*, and the failure path may be
very hard to reach at all.

Two consequences, neither yet settled:

* **The failure path is untested, not proven absent.** No live evidence exists
  that `NOT armed` renders correctly, or that a caller handles it.
* **Succeeding is not obviously better than failing here.** A surface resolved
  only by its window title yields a document identity that is unstable —
  Notepad's title changes with the filename and the dirty marker — which is the
  exact ambiguity the `/d/<id>/` lookup exists to defeat for spreadsheets. The
  mark reports success and the document half is weak.

Whether the fallback should stand is a design question, not a bug, and it is
open.

## What a fix must not do

**It must not invent a label.** Falling back to the element's own text would put
*content* in the field position, where a field name is schema — the same
distinction that keeps `el/<ordinal>/<label>` clean under §3 today.

**It must not silently drop unlabelled marks.** A user who deliberately pressed
the marker and got nothing recorded, with no message, is worse off than one who
gets an unusable field and can see that it is unusable.

**It must not assume the nearest preceding text is the label.** That is what the
adjacency rule already does, and the empty result means there was no structural
element to find — not that one was missed.

The honest options are to report the mark as unusable at the moment it is taken,
so the user can act while they are still looking at the page, or to let the user
name the field themselves — the `DesignatedIdentityField::chosen_by_user`
pattern the project already uses for exactly this class of "cannot be inferred
safely" decision.

## Reproducing

1. Start Record Mode. Open a real page with no Name Box — a dashboard, a
   listing, a PDF in Edge.
2. Click a value on it, press `Ctrl+Shift+M`.
3. Read `paradigm-dev10.log` for the `source mark` line. An `el/<n>/` with
   nothing after the final slash is this defect.

## Related

* `docs/known-issues/a-source-mark-costs-half-a-second-off-the-spreadsheet-path.md`
  — the other finding from the same session.
* `docs/known-issues/element-id-is-a-hash-of-the-text.md` — the fixture-versus-
  live-surface pattern, and why `identity::tree`'s inputs deserve suspicion.
* `docs/planning/Generalizing-Source-Destination-Tracking.md` §9.2 — the table
  of what has been proven per surface, and to what depth.

## Confirmed on a fourth application, and now user-visible (2026-08-22)

Four applications have shown it: Gmail, Amazon, OrderFlow, and the ChatGPT
pricing page — the last through session record-fe88fb0d, where every one of
nine source references was `el/N/` with the label half empty.

`detect::candidates` shipped on 2026-08-22 and this defect is now something the
**user reads**, not only something the logs record. A candidate is described by
its position rather than by a field name:

```
[ ] cand-1  click on the 1st element across, 0px into each record    3 records
[ ] cand-2  click on the 1st element across, 100px into each record  3 records
```

Those two are the customer name and the quantity on a real OrderFlow page,
correctly grouped across three orders. The **grouping** does not depend on
labels — it is positional by design, which is exactly why it works — so this
does not break detection. It degrades the question the user is asked into one
they have to decode.

**The names were captured.** The same recording holds `Harbor Point Traders`,
`Ashgrove Manufacturing`, `Windmere Consulting`. They cannot name the field
because they are the *values*, and they differ per record by design. The field
name would have to come from an adjacent structural element, which is what
`identity::tree` supplies and what the action stream does not carry.

So the fix has a shape: get the label from the tree at capture time, alongside
the position, rather than trying to recover it later from values. That is the
same conclusion this doc already reached; what is new is that the cost of not
doing it is now paid in front of the user.

## Worse than empty: confidently WRONG (2026-08-22)

Session record-1a2123c0, a natural OrderFlow recording:

```
el/0/Pending      -> A2
el/0/UNIT PRICE   -> E2
el/1/Pending      -> A3
el/1/PRODUCT      -> C3
el/2/Pending      -> C4
```

`UNIT PRICE` and `PRODUCT` are right. **`Pending` is the order status**, printed
identically in every record — so it is structural by the multiplicity rule, and
the adjacency rule adopts it as the label for whatever value follows it.

The mechanism is not malfunctioning. It takes the structural element preceding
the value, and on this page that is sometimes a status badge rather than a
column heading. But the output is a field named `Pending`, which is worse than a
field named nothing: an empty label announces that it does not know, and this
one does not.

Whatever supplies labels in future has to distinguish *a heading that addresses
the next value* from *a repeated value that merely sits above one*. Adjacency
alone cannot, because both are structural and both precede content.
