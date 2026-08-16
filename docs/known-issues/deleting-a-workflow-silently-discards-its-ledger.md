# Deleting a workflow silently discards what it had processed

**Status:** open, worth scheduling. Diagnosed 2026-08-15 from a real report.
**Where:** `compile::store::delete`, the `delete_playbook` command, and the
delete confirmation dialog in `src/components/delete-playbook`.
**Severity:** the user re-does work they already did, against live documents.

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
> version starts over and may process them again.

Deletion is the user's call; they simply cannot currently make it informed.

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
