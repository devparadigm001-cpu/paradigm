# Cross-connection write visibility — investigated, NOT a bug

**Status:** CLOSED 2026-08-14. **The reported pattern was not real.**
**Probe:** `src-tauri/examples/db_visibility_probe.rs`

> This document originally asserted that writes from a second connection were
> invisible to the app whenever the app had *created* the database, based on a
> seven-run correlation. **That was wrong.** It is kept, corrected, because a
> confidently-wrong known-issue doc is more dangerous than no doc: someone
> would eventually have "fixed" a database layer that was never broken.

## What was originally claimed

| how the app got its database | external writes visible |
|---|---|
| opened one that already existed | yes — 4 runs |
| created it itself | no — 3 runs |

Seven runs, no exceptions. It looked like a real pattern. It was a
coincidence.

## What actually settled it

### 1. The minimal reproduction, which did not reproduce

Two plain processes, no Tauri, no webview, no accessibility tree, no async.
One holds a connection; another opens the same database and writes a
playbook; the holder is then asked what it sees. Both cases run — holder
creates the database, and holder opens a pre-existing one:

```
case: holder CREATED the database
  holder sees 0 playbook(s) before  (data_version 3)
  [writer] stored ..., and sees 1 playbook(s) itself
  holder sees 1 playbook(s) after   (data_version 4)
  a FRESH connection sees 1
  => no problem here: the holder saw the write.

case: holder OPENED an existing database
  holder sees 1 before (data_version 3) -> 2 after (data_version 4)
  => no problem here: the holder saw the write.
```

`data_version` moving from 3 to 4 is the direct evidence: it changes
precisely when *another* connection has committed, so the holder did not
merely happen to re-read — it observed the other process's commit.

The probe deliberately also opens a **third, fresh** connection after the
write. Without it a failure would have been ambiguous between "the write
never landed" and "the holder is on a stale snapshot"; with it, the two are
distinguishable. Neither occurred.

### 2. The app-level retest, which also did not reproduce

The one click in the UI driver still trusting that `click()` returning `Ok`
meant the page had reacted was **Refresh** — the very click whose failure
produced "No playbooks saved yet". Every other click had already been moved
to a retry-until-effect helper after this repo's own recorded lesson that
delivery is not reaction; Refresh had been missed.

With Refresh retried the same way, the driver was run against an app that
had **created** its database — the configuration that had failed every
previous time:

```
-- clicking Refresh --
  list shows the templated badge: "↻ repeating"
-- §4.8 --  "Found 3 new records starting at row 2..."
-- §4.3 --  "About to write source row 2 → destination row 2", value "Acme"
-- §4.9 --  "Processed rows 2–4 — 3 records."
CSV         A2..B4 match the source exactly
```

## The actual cause

**A UI click that was delivered and never acted on.** The database was fine
the entire time. The write was always there; the app was never asked to
re-read.

The "create vs open" correlation came from run ordering, not causation: the
runs where the database already existed happened to be ones where the
Refresh click landed.

## What this cost, and the lesson worth keeping

Five hypotheses, four of them wrong, before the right one:

1. `PARADIGM_DATA_DIR` not reaching the app — wrong, and the test that
   suggested it was itself broken.
2. The app using a different directory — wrong.
3. Deleting the directory under a running app — wrong.
4. A database-level visibility problem — wrong, and this document asserted
   it.
5. A click that was never acted on — correct.

The pattern in the errors is worth more than any of them individually: this
repo had **already recorded** that a click can be accepted and never seen,
for Google Sheets tabs, and had already added a retry helper for exactly
that. The failure was a known failure mode in an un-migrated call site, and
it got mistaken for a novel database bug because seven runs correlated.

A seven-run correlation is not a cause. The minimal reproduction that the
previous version of this document recommended as step one is what settled
it, in about ten minutes, and it should have been step one rather than step
five.

## What remains true and worth keeping

The run loop genuinely does open its **own** connection on the run thread
(`run::background::execute`), deliberately, so a paused run cannot hold the
connection every command needs. Cross-connection visibility therefore does
matter to correctness — `workflow_processed_rows` is written by one
connection and read by another. That is now **measured to work**, in both
directions, rather than assumed. `db_visibility_probe` is the check if it is
ever doubted again.
