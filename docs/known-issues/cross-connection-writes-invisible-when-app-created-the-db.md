# Writes from a second connection are invisible when the app created the database

**Status:** open. Observed repeatedly, cause unknown, **not yet explained**.
**Found:** 2026-08-13, while driving the Section 6 UI click-through.
**Severity:** unquantified, and that is the point — see "Why this is a product
question" below.

## The observation

Two processes, one SQLCipher database in WAL mode:

* the running app (`npm run tauri dev`, `PARADIGM_DATA_DIR` set), and
* a probe that opens the same paths with `db::open` and writes a playbook.

Whether the app sees the probe's write depends on **which process created the
database file**:

| how the app got its database | probe's write visible to the app |
|---|---|
| **opened** one that already existed | **yes** — 4 runs |
| **created** it itself on first launch | **no** — 3 runs |

Seven runs, no exceptions in either direction. In the failing case the app's
playbook list reports "No playbooks saved yet" indefinitely, including after a
`Refresh` that re-invokes `list_playbooks`; the probe's `store()` returns `Ok`
and its own connection reads the row back.

## What has already been ruled out

These were each hypothesised and then **disproved by measurement**, not
argued away:

1. **`PARADIGM_DATA_DIR` not reaching the app.** It does. A child launched
   through the same `Start-Process` shape prints the directory correctly.
   (The first test of this appeared to fail and the test itself was broken —
   fragile quoting in the child command.)
2. **The app using a different directory.** It does not. Launched with no
   probe involved at all, the app creates `paradigm.db` in the configured
   directory.
3. **Deleting the directory out from under a running app**, leaving the app
   on an unlinked file while the probe creates a new one at the same path.
   Plausible, and wrong: a fully clean reset — app and cargo watcher killed,
   directory removed with no held handles, single instance — reproduces the
   failure exactly.
4. **Exclusive locking or an unusual journal mode.** `db::open` sets
   `foreign_keys=ON`, a 5s busy timeout, and `journal_mode=WAL`. Nothing else.

## Why this is a product question, not a test-harness one

The run loop **does not share the app's connection**. `run::background::execute`
opens its own with `db::open`, on the run thread, deliberately — a paused run
must not hold the connection every command needs (§4.10's "does not lock the
window").

So the exact shape that fails here — *one process writes, another process's
long-lived connection does not see it* — is the shape a real run has:

* `workflow_processed_rows` is written by the **run thread's** connection.
* `check_for_new_records`, `preview_workflow_run` and `list_playbooks` read
  through the **app's** connection.

If the app's connection cannot see the run thread's ledger writes, then §4.7's
duplicate protection silently stops working from the app's point of view: a
finished batch would still be offered as new, and the preview would show a
record that has already been written.

**In the runs where the app opened an existing database, this demonstrably
works** — the second batch correctly reported "Found 6 new records starting at
row 5", which is only possible if the app's connection saw the run thread's
writes for rows 2–4. So the mechanism is fine in that configuration. What is
unknown is whether the create-case failure extends to it, and a user's **first
ever launch** is by definition the create-case.

That is the question worth answering, and it is not answerable by guessing.

## What would settle it

In rough order of cost:

1. **Reproduce without any UI.** Two plain processes: one calls `db::open` on a
   fresh path and holds the connection; the other opens the same path, writes a
   row, and the first re-queries. If that reproduces, the whole investigation
   collapses to a small integration test and the app is irrelevant.
2. **Vary one thing at a time** from there: WAL vs DELETE journal mode;
   SQLCipher vs plain SQLite; holding the first connection open across the
   write vs reopening; whether the creating connection has ever checkpointed.
3. **Check what the app's connection actually sees** — `PRAGMA
   wal_checkpoint`, `PRAGMA data_version` before and after the external write.
   `data_version` changing is exactly the signal that another connection has
   committed; if it does not change, the app is on a snapshot and the question
   becomes why.
4. **Only then** decide the fix. Candidates range from "the app should reopen
   or checkpoint after a run" to "the run thread should share the app's
   connection after all" — but choosing between them before step 3 would be
   the same guessing this document exists to avoid.

## Note

Three hypotheses about this have already been wrong. The temptation to attach
a fourth to this document was real and is being resisted deliberately: what is
written above is what was measured.
