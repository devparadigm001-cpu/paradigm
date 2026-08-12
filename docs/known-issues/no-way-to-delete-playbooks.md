# There is no way to delete a saved playbook

**Status: BUILT, 2026-08-11.** `store::delete`, the `delete_playbook` command,
and a delete control with a naming confirmation all exist, and deletion was
driven through the real app UI against a scratch store with the result verified
in the database. Everything below the "Implemented" section describes the
original gap. **The last follow-up closed 2026-08-12:** orphaned run history is
now readable — see "Orphaned run history is readable".
**Affected:** `src-tauri/src/commands.rs` (no command), `src-tauri/src/compile/
store.rs` (no SQL), and the frontend's stored-playbooks list (no control).
**Found:** 2026-08-07, during real user testing.
**Severity: MEDIUM.** Blocks no Phase 1 step and corrupts nothing. It is a gap
users hit almost immediately in real use, because mistaken and throwaway
recordings accumulate with no cleanup path and the list only ever grows.

## Summary

A playbook can be created, listed, replayed, and inspected. It cannot be
removed. There is no `delete_playbook` command, no delete function in the store,
and no delete control in the UI. Every playbook ever saved — including several
throwaway test recordings made during testing on 2026-08-06 and 2026-08-07 — is
permanently present in the real database, reachable only by editing the
encrypted store by hand.

## Evidence

> **Superseded — this section describes 2026-08-07 and is no longer accurate.**
> The command list, the store surface, and the "grep returns nothing" finding
> below have all expired: `delete_playbook` and `store::delete` exist. Kept as
> the record of the original gap. See "Implemented".

### The complete set of registered commands

All eight, from `lib.rs`'s single `generate_handler!`:

```
greet
db_health_check
commands::start_record_session
commands::stop_record_session
commands::compile_and_store_playbook
commands::list_playbooks
commands::replay_playbook
commands::get_run_history
```

Four concern playbooks — create, list, replay, history. None deletes.

### The complete public surface of the store

`src/compile/store.rs`:

```
pub fn store(...)                -> write a compiled playbook
pub fn load(...)                 -> read one back
pub fn list(...)                 -> summaries for the list screen
pub fn parse_control_role(...)   -> enum helper
```

A grep for `fn delete`, `fn remove`, `DELETE FROM`, and `delete_playbook` across
all of `src/` returns **nothing**. There is no deletion path at any layer — not
in the IPC surface, not in the store, not in raw SQL.

## The schema already supports this, and has already decided the hard part

This is the useful part for whoever implements it. Deletion was anticipated in
Step 1's schema, and the awkward question — what happens to run history — is
already answered there rather than being an open decision.

```sql
-- 20260803000001_init_playbooks.sql
playbook_id  TEXT NOT NULL REFERENCES playbooks (id) ON DELETE CASCADE
```

```sql
-- 20260803000002_init_runs.sql
-- NULL for ad-hoc runs, and set to NULL rather than cascading if the
-- playbook is later deleted -- run history must outlive its playbook.
playbook_id  TEXT REFERENCES playbooks (id) ON DELETE SET NULL,
```

So a single `DELETE FROM playbooks WHERE id = ?1` produces the intended
behaviour through constraints that already exist:

| Table | On playbook deletion | Effect |
|---|---|---|
| `playbook_steps` | `ON DELETE CASCADE` | steps removed with the playbook |
| `runs` | `ON DELETE SET NULL` | **run survives**, detached from the playbook |
| `run_steps_log` | cascades from `runs` | survives, because its run survives |
| `run_steps_log.playbook_step_id` | `ON DELETE SET NULL` | detached, row retained |

Foreign-key enforcement is switched on for every connection
(`db/mod.rs:83`, `PRAGMA foreign_keys = ON`), which the whole arrangement
depends on — SQLite ignores these clauses otherwise.

### One consequence worth deciding on deliberately

The schema retains run history after its playbook is deleted, but the only API
that reads history cannot then find it. `journal::load_runs_for_playbook`
queries `WHERE playbook_id = ?1`, and detached runs have `playbook_id IS NULL`,
so they match nothing. `get_run_history` is the sole reader.

Retained history that nothing can read is retained in name only. Either
something must be able to list orphaned runs, or the retention is not actually
delivering what the schema comment intends. This is not an argument for
cascading the deletion — the Step 1 reasoning still stands — but it is a loose
end that deletion makes visible for the first time.

## Why it matters

The gap compounds rather than staying constant. Recording is the primary action
in the product, and early use produces a high proportion of mistakes, tests, and
abandoned attempts. Each one is permanent. The list screen degrades steadily
into a mixture of real playbooks and debris, with no way for the user to tell
the product to forget something.

There is a privacy dimension too, and it is the sharper one. Capture is
system-wide (see `record-mode-unscoped-system-wide-capture.md`), so a recording
can pick up more than the user intended. Right now a user who realises they
recorded something they did not mean to has no way to remove it. "Delete that"
is the natural response to a mistaken recording, and the app has no answer to
it.

## Implemented (2026-08-11)

### Most of the backend already existed, undocumented

Worth saying first, because it changes what this task actually was. The doc
above states there is no delete at any layer. By the time it was picked up,
`store::delete`, `commands::delete_playbook`, its `generate_handler!`
registration, three store tests and two IPC tests were **already present**. The
doc had gone stale; the store and IPC halves needed no work.

### The UI already existed too — on `frontend-dev`

**This section originally claimed there was no stored-playbooks screen on any
branch, and that `frontend-dev` had no React at all. Both were false.**
`frontend-dev` carries a complete implementation:

* `src/App.tsx` → `StoredPlaybooksSection`: lists via TanStack Query
  (`useQuery(["playbooks"], list_playbooks)`), a Delete and a Replay per row,
  loading/empty/error states, a manual Refresh, per-row "Deleting…" state, and
  `await refetch()` after a successful delete so the list reflects the store.
* `src/components/delete-playbook/DeletePlaybookDialog.tsx` — a Radix
  `AlertDialog` naming the playbook and its step count, with ESC and
  click-outside disabled so only the explicit buttons resolve it.
* `src/components/delete-playbook/useDeletePlaybookConfirmation.tsx` — a
  reusable hook matching the codebase's existing
  `useAutomationPreview` / `useAccessibilityPermissionGate` pattern.

**How the wrong conclusion was reached, because the mechanism matters more than
the mistake.** The check was `git ls-tree -r --name-only origin/frontend-dev |
grep '\.tsx$'`, run from inside `src-tauri/`. Git resolves `ls-tree` paths
relative to the current directory, so it silently searched only that subtree and
returned nothing. Empty output was read as "no such files". That is the
absence-as-data error catalogued in `replay-window-selector-ambiguity.md` — made
here for the sixth time, while writing a document that records the pattern. The
correct invocation needs `--full-tree`, or to be run from the repository root.

### What was built, and then removed

A `src/PlaybookList.tsx` was added on `backend-dev` (commit `dbf33b7`) — list,
per-row Delete, and a confirmation naming the playbook. It was **removed again**
once `frontend-dev`'s implementation came to light, because it duplicated it and
was worse in every respect that was compared: a hand-rolled `<div role="dialog">`
instead of Radix (no focus trap, no ESC handling), a locally redeclared
`PlaybookSummary` type instead of importing the shared `PlaybookSummaryView`, and
no Replay, error state, or manual refresh. Its one apparent advantage —
re-reading the store after a delete rather than dropping the row locally — turned
out to be present already, as `await refetch()`.

It is recorded rather than quietly erased because the sequence is the useful
part: a duplicate implementation is what a bad cross-branch check produces, and
the duplicate would have merged **silently** — `PlaybookList.tsx` existed only on
one side, so a merge adds it with no conflict to force anyone to look.

### The one thing carried across

Per-row delete controls on `frontend-dev` all had the same accessible name
("Delete"), so nothing could tell them apart. That matters for this product
specifically, whose own tooling drives the UI through the accessibility tree, and
whose replay refuses to act on exactly this condition (`FailedAmbiguous`). The
labels are now unique — see "Disambiguating the delete controls".

### Disambiguating the delete controls (`frontend-dev` `945a2b2`)

Every row's Delete button carried the accessible name "Delete". Nothing could
tell them apart — not a screen reader, and not this product's own automation,
which drives the UI through the accessibility tree and refuses to act when a
selector matches more than one element (`StepResult::FailedAmbiguous`, see
`replay-window-selector-ambiguity.md`).

**The name alone would not have fixed it, which is why the label carries the
position.** Playbook names are not unique — nothing enforces uniqueness at any
layer — so `Delete <name>` produces two identical labels for two recordings
called the same thing. That is not hypothetical; it is the case the fix was
tested against.

Two playbooks both named `"Duplicate Name Probe"`, seeded into a scratch store
and driven through the real app:

```
  playbooks listed: ["Duplicate Name Probe, 1 of 2", "Duplicate Name Probe, 2 of 2"]
```

Distinct, and readable aloud — which a UUID suffix would not have been.

The change lives on `frontend-dev`, since that is where the UI is. It was
committed on its own: that worktree had an unrelated in-progress edit to
`confidence-calibration-never-recorded.md`, left untouched.

### The UI test found a real defect that no unit test would have

> Applies to the **removed** `PlaybookList.tsx`, not to `frontend-dev`'s dialog,
> which is a Radix `AlertDialog` and was never in document flow. Kept because the
> lesson is about the method, not the component.

First run through the real app: the confirm button was unreachable.

```
  invoke failed (Element is not visible: Element is offscreen)
```

The confirmation rendered in normal document flow, below a long scaffold page,
so on a short window it sat below the fold. To a user that is "I pressed Delete
and nothing happened" — a destructive action that looks broken rather than
guarded, which is worse than no confirmation at all. Fixed by making it a fixed,
centred overlay. Every layer had passed its own tests while this was true.

### Verified end to end, against a scratch store

> Run against the removed `PlaybookList.tsx`. What it establishes that outlives
> the component is the **backend** path: `delete_playbook` over real IPC removes
> exactly one playbook and the store agrees. `frontend-dev`'s UI calls the same
> command.

Driven through the real app with `PARADIGM_DATA_DIR` pointed at a throwaway
directory — never the real database, since the feature's whole purpose is
removing recordings and practising on real ones would be a poor trade.

```
  playbooks listed: ["DeleteMe Probe", "KeepMe Probe"]
  -- clicking Delete --
  confirmation names it: "DeleteMe Probe"
  -- confirming --
  playbooks after delete: ["KeepMe Probe"]

  confirmation named the playbook : true
  deleted row is gone from the UI : true
  the other playbook survived     : true
```

and the database, read independently afterwards:

```
  store holds 1 playbook(s):
    "KeepMe Probe"
```

The control playbook is the point: "the row disappeared" is also what a
delete-everything bug looks like. Both the UI and the store agree that exactly
one playbook went.

Backend behaviour was already covered and still passes: an unknown id errors
rather than reporting success, steps cascade, runs survive detached, and other
playbooks are untouched. Full suite green — 128 tests, clippy clean.

### The probe's own false positive, fixed

Recorded because it nearly produced a wrong pass. The first version checked
"does any text on screen name the playbook", which the **list row** satisfies —
so it would have reported a correct confirmation even if no dialog opened. It
now requires text that appears only inside the dialog ("This cannot be undone")
before crediting the name. Its first enumeration of the webview also came back
`Err` and was swallowed by `if let Ok(...)`, reporting an empty list for a UI
that had rendered perfectly; it now retries and prints the error.

## Orphaned run history is readable (2026-08-12)

The last open item. The schema had already decided the policy — keep the history
— and that decision is reaffirmed, not revisited. What was missing was a way to
get it back.

### The gap was reachability, not retention

`migrations/20260803000002_init_runs.sql` gives `runs.playbook_id`
`ON DELETE SET NULL`, with the comment *"run history must outlive its playbook"*.
That worked: the existing test
`deleting_a_playbook_removes_its_steps_but_detaches_rather_than_deletes_runs`
proves the row survives with a NULL `playbook_id`.

**Surviving is not the same as being reachable.** The only reader was
`journal::load_runs_for_playbook`, which takes an id to look up — and after
deletion there is no id to pass. The history was being retained and could not be
read by anything. That is retention with no purpose, which is why the original
item offered "or revisit whether retaining them serves any purpose" as the
alternative. Retention serves a purpose now.

### What was built

* **`journal::load_orphaned_runs(conn)`** — `WHERE playbook_id IS NULL`, most
  recent first, mirroring `load_runs_for_playbook`'s ordering.
* **`get_orphaned_run_history`** — a new IPC command taking **no arguments**,
  which is the honest signature: there is no id to scope by, and that is exactly
  what makes these runs orphaned. Extending `get_run_history` was considered and
  rejected — it is keyed on a `playbook_id` that cannot express NULL.
* Both history commands now share `runs_with_logs`, so they cannot present the
  same rows differently. The three `runs` readers also share one column list and
  one row mapper, which previously existed in triplicate.

### Verified, not assumed

`a_deleted_playbooks_run_history_is_still_readable_through_the_orphan_path`
drives the whole path against a **real encrypted database**: store a playbook,
start a run, log two real step events, finish it, delete the playbook, read it
back. It asserts the gap as well as the fix — that after deletion
`load_runs_for_playbook` returns **empty**, which is why the orphan path has to
exist — then that `load_orphaned_runs` returns the same run id with
`playbook_id: None`, status `completed`, its timing intact, and **both step-log
rows still present**. Step logs matter: a run with no events is not history.

```
test compile::store::tests::a_deleted_playbooks_run_history_is_still_readable_through_the_orphan_path ... ok
test result: ok. 105 passed; 0 failed
```

Reachability over the real IPC boundary is covered by
`every_registered_command_is_reachable_over_ipc`, which the new command was added
to — a command can compile and be listed in `generate_handler!` and still be
unreachable:

```
test every_registered_command_is_reachable_over_ipc ... ok
test result: ok. 15 passed; 0 failed
```

### One limitation, stated rather than papered over

**NULL is overloaded.** The same migration documents NULL as *also* marking an
ad-hoc run that was never recorded as a playbook, so the schema cannot
distinguish "orphaned by deletion" from "never had a playbook".

Today that ambiguity is harmless, and for a checkable reason rather than by
luck: `journal::start_run` takes `&str`, not `Option<&str>`, so every run is
created attached and a NULL can only have come from a deletion. If an ad-hoc run
path is ever added, this command starts returning both kinds mixed together, and
telling them apart needs a column that does not exist yet. Recorded on
`load_orphaned_runs` itself, where someone adding that path will be reading.

**No frontend surface yet.** The command exists and is reachable; nothing in the
UI calls it. That is deliberate — the read path is what the schema's retention
promise required, and where orphaned history belongs in the interface is a
product question, not a defect.

## Next steps

- [x] ~~**Add `delete_playbook(playbook_id)`.**~~ Already existed when this was
      picked up -- `store::delete`, the command, and its registration. Original
      scope: Small and well-scoped, comparable
      to the `step_indices` and `irreversible_count` additions: a
      `store::delete` running one `DELETE FROM playbooks WHERE id = ?1`, a thin
      command wrapping it, and registration in the single `generate_handler!`.
      No schema change is required.
- [x] ~~**Return a clear error for an unknown id**~~ Done: `store::delete`
      checks `rows_affected` and returns `DbError::NotFound`. Original scope: rather than reporting success.
      `DELETE` affects zero rows and succeeds when the id does not exist, so the
      command should check `rows_affected` and say so.
- [x] ~~**Test that the cascade actually fires.**~~ Done:
      `deleting_a_playbook_removes_its_steps_but_detaches_rather_than_deletes_runs`.
      Original scope: Assert that steps are gone, and
      that runs survive with `playbook_id IS NULL` — proving `PRAGMA
      foreign_keys` is really in force on that connection rather than assuming
      it from the migration text.
- [x] ~~**Add a delete control to the stored-playbooks list, with confirmation.**~~
      Done, and the list itself had to be built -- none existed. Verified through
      the real UI. Original scope:
      Deletion is irreversible and there is no undo, so the confirmation should
      name the playbook being deleted rather than being a generic prompt.
- [x] ~~**Decide what happens to orphaned run history** — see above. Either add a
      way to read runs whose playbook is gone, or revisit whether retaining
      them serves any purpose.~~ **Decided and built 2026-08-12: keep them, and
      make them readable.** `journal::load_orphaned_runs` +
      `get_orphaned_run_history`, verified end to end against a real encrypted
      database and over the real IPC boundary. See "Orphaned run history is
      readable".
