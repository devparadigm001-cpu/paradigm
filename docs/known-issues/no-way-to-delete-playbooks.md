# There is no way to delete a saved playbook

**Status: BUILT, 2026-08-11.** `store::delete`, the `delete_playbook` command,
and a delete control with a naming confirmation all exist, and deletion was
driven through the real app UI against a scratch store with the result verified
in the database. Everything below the "Implemented" section describes the
original gap. One follow-up remains open: orphaned run history is still
unreadable.
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

What genuinely did not exist was the **UI**, and the reason is worth recording:
there is no stored-playbooks screen on any branch. `src/App.tsx` is the Tauri
scaffold, and `frontend-dev` is rooted at `src-tauri/` with no React at all. So
"add a delete control to the stored-playbooks list" had no list to attach to —
the minimal surface had to be built with it.

### What was added

`src/PlaybookList.tsx`: lists playbooks from `list_playbooks`, a Delete button
per row, and a confirmation that **names the playbook** and states that run
history is kept. After a successful delete the list re-reads the store rather
than removing the row locally, so what is shown is the store's answer rather
than the component's guess.

The confirmation names the target deliberately. A generic "are you sure?" is the
prompt most likely to be clicked through by reflex, and the mistake it would wave
past — deleting the wrong recording — cannot be undone.

### The UI test found a real defect that no unit test would have

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
- [ ] **Decide what happens to orphaned run history** — see above. Either add a
      way to read runs whose playbook is gone, or revisit whether retaining
      them serves any purpose.
