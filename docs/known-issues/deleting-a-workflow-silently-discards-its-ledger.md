# Deleting a workflow silently discards what it had processed

**Status:** open, **worth prioritising**. Diagnosed 2026-08-15 from a real
report, then hit AGAIN by the same user within the same hour — see "This is not
a rare edge case" below.
**Where:** `compile::store::delete`, the `delete_playbook` command, the delete
confirmation dialog in `src/components/delete-playbook`, and
`run::batch::resume_destination_row`.
**Severity:** the workflow **silently overwrites** the destination from the top
rather than appending to it. Not merely redundant work — see "The consequence
is worse than re-pasting".

## What the user sees

> "After a successful run, **Check for new** re-pasted rows 2-4, which had
> already been processed in an earlier run."

Nothing was wrong with the ledger, and nothing was wrong with the scan. The
rows had been processed by a **different** workflow — one that had since been
deleted, taking its ledger with it.

## What the store actually held

From `cargo run --example ledger_dump` against the live database:

```
4f7c5cd1-c2eb-4fd6-a448-acb7ce8ab0ac
  name    : "s"        template: confirmed     created: 2026-08-16T01:00:39Z
  source  : "1ko7z65TnzI5suwvu3LGv8yoemmhOs5siSQe9KBB8NZs"
  ledger  : 5 row(s)
      row 2  ... 01:02:35.881
      row 3  ... 01:02:48.412
      row 4  ... 01:03:00.936
      row 5  ... 01:03:13.468
      row 6  ... 01:03:26.045
```

Every entry carries the **same `source_id` as the template**, so the scan's
`(playbook_id, source_id, row_key)` lookup matches all five. Each row appears
exactly once, 12.5s apart in one continuous 51-second sequence — one pass over
2→6, not two runs.

Meanwhile every other playbook in the store is gone. All fourteen recorded runs
report a deleted owner:

```
2026-08-14T22:15:52.330Z  failed  playbook <DELETED>  run 1d12aaa5-...
```

## Why that erases the ledger

By design, and the design is right:

```sql
playbook_id TEXT NOT NULL REFERENCES playbooks (id) ON DELETE CASCADE,
...
UNIQUE (playbook_id, source_id, row_key)
```

The migration is explicit that `playbook_id` belongs in the key and that
`UNIQUE (source_id, row_key)` "would be the sharing bug" — one workflow must
not be able to mark a row done on another's behalf. `ON DELETE CASCADE` follows
from the same reasoning: the ledger is operational state that means nothing
without its workflow, unlike `runs`, which deliberately outlives it.

So a re-recorded workflow is a **new** workflow with a new id and an empty
ledger. Rows the old one processed are legitimately new to it. That is correct
behaviour, and it is also a nasty surprise, because the user did not experience
themselves as creating a new workflow — they re-recorded "the same" one.

## The consequence is worse than re-pasting

The ledger does not only decide which rows are new. It also decides **where the
destination resumes**:

```rust
let done = processed_count(conn, playbook_id, source_id)? as i64;
Ok(((first_row as i64) + done.saturating_mul(destination_step)).max(1) as u64)
```

With an empty ledger `done == 0`, so the resume row is `first_row` — the
**top** of the destination. A re-recorded workflow therefore does not append
after what the previous one wrote. It writes **over** it, from the beginning.

Measured on the second occurrence. Two different playbooks each ran once over
the same five source rows, into the same destination. Afterwards the
destination held:

```
Customer,Amount
Blue Horizon Supply,1150
Redwood Manufacturing,675.25
Silverline Consulting,920.5
Cedar Point Logistics,340
Marigold Retail Group,1580.75
```

**Five rows, not ten.** Ten writes landed in five cells' worth of rows.

That is the dangerous part, and it cuts two ways:

* **It is nearly invisible.** Because the values were identical, the overwrite
  left no trace. Nothing in the destination shows that a second workflow
  rewrote it. Had the source changed between the two runs — a corrected
  amount, an edited name — the newer values would have been silently replaced
  by whatever the second run read, with no duplicate row to notice.
* **The guard that would catch it is the wrong shape.** `resume_destination_row`
  documents its own assumption — "that the destination started empty at
  `first_row` and that this workflow is the only thing writing to it" — and
  names §4.3's preview as the check. But the preview shows the *first* record's
  target cell, which for a fresh ledger is exactly where the previous workflow
  also started. It looks correct, because it IS the same cell. The preview
  cannot distinguish "resuming an empty destination" from "about to overwrite
  another workflow's output".

So the honest statement of severity is not "it may reprocess rows". It is: a
re-recorded workflow silently overwrites its destination from row one, and
neither the ledger, the preview, nor the finished sheet will say so.

## This is not a rare edge case

It happened twice in one hour, to the same user, on the same pair of documents
— the second time while the first was still being written up. Both dumps were
taken directly from the live store:

| | first occurrence | second occurrence |
|---|---|---|
| playbook | `4f7c5cd1-…` `"s"` | `d9b3fd21-…` `"a"` |
| created | 01:00:39 | 01:15:05 |
| ledger written | 01:02:35 → 01:03:26 | 01:17:14 → 01:18:05 |
| rows processed | 2–6 | 2–6 |

`"s"` no longer exists in the store. In both cases the ledger timestamps are
spaced **12.52–12.61s apart with no gap anywhere**, which is what a single
continuous pass looks like — so neither playbook re-processed its own rows.
Each ran exactly once, and each was a *new* workflow that had never seen those
rows.

The second report arrived described as "this playbook has NOT been
deleted/recreated", which is the whole problem in one sentence: re-recording
produces a new workflow, with a new id and an empty ledger, appearing in the
same list under a similar name. From the outside it is the same workflow. From
the ledger's point of view it has never run.

That is why the warning below is worth prioritising rather than scheduling
loosely. The confusion is not hypothetical, it is not rare, and the thing it
costs is silent overwriting of a live document.

## What is NOT claimed

That a deleted playbook actually processed rows 2-4. The cascade destroyed that
evidence. It is the best-supported explanation — one surviving playbook, a
sheet that already had those rows filled, fourteen runs belonging to workflows
that no longer exist — but it is inference, not a record.

## What to do

`delete` is a single `DELETE FROM playbooks`, and the confirmation dialog says
nothing about processed rows. The count is one query away:

```sql
SELECT COUNT(*) FROM workflow_processed_rows WHERE playbook_id = ?1
```

Warn, do not block. Something like:

> This workflow has processed **5 records**. Deleting it means a re-recorded
> version starts over — it will process those records again and write them
> from the top of the destination, over what is already there.

Deletion is the user's call; they simply cannot currently make it informed. The
wording should name the overwrite, not just the reprocessing — the reprocessing
is the cost the user can see afterwards, and the overwrite is the one they
cannot.

Worth pairing with it, and cheaper than it looks: the §4.3 preview already
knows the resume row and the ledger count. A preview that opens on a destination
cell which is **not** empty, for a workflow whose ledger is empty, is describing
this exact situation and could say so. That is the check
`resume_destination_row` claims the preview provides, actually provided.

## Deliberately not proposed

**Keying the ledger by source instead of playbook.** That is the sharing bug the
schema exists to prevent, and it would let an unrelated workflow suppress a row
this one never handled.

**Preserving the ledger across a delete.** It would be keyed to an id nothing
can read or clear — the migration says so — and a later workflow has no
principled claim on the old one's history.

The honest fix is to tell the user what deleting costs, not to change what it
does.

## Related

* [batch-scan-cost-is-linear-in-rows.md](batch-scan-cost-is-linear-in-rows.md)
  — the same `Check for new` path.
* [templated-runs-leave-no-run-history.md](templated-runs-leave-no-run-history.md)
  — why the timeline above had to be reconstructed from ledger timestamps
  rather than read out of `runs`.
