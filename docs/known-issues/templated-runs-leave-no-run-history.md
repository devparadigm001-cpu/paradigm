# Templated runs leave no run history

**Status:** open, worth scheduling. Found 2026-08-15 while diagnosing a
different report.
**Where:** `run::background` / `run::run_with_control` do not call
`replay::journal::start_run`.
**Severity:** observability. Nothing is lost or corrupted — but a whole class of
run is invisible to every question anyone will later ask about it.

## What is missing

`journal::start_run` writes the `runs` row that makes a run answerable:

```rust
"INSERT INTO runs (id, playbook_id, feature, status, billable, started_at) ..."
```

Its only production caller is `replay::mod.rs:640` — Phase-1 replay. The
templated run loop never touches it. So `runs` records replays and nothing
else.

## How that showed up

While reconstructing a real report, the newest row in `runs` was:

```
2026-08-14T22:15:52.330Z  failed  playbook <DELETED>  run 1d12aaa5-...
```

while the ledger entries under investigation were written on **2026-08-16 at
01:02-01:03**. Nearly two days of templated runs — including the one being
asked about — left no trace in the table built to record runs.

The timeline had to be rebuilt from `workflow_processed_rows.processed_at`
instead: usable only because a run happens to mark each row as it goes, and
only for runs that wrote something. A run that stopped on drift, was cancelled,
or failed before its first write leaves nothing at all.

## Why this matters more than it sounds

Every diagnostic question asked of this system tonight was a history question:

* which run re-processed those rows, and when?
* was this the first run after confirming the pattern, or a later one?
* did the run that corrupted the destination complete or stop?

Each needed a DB dump and inference from side effects. `runs` already exists,
already has `status`, `started_at` and a `playbook_id` that deliberately
outlives its playbook — which is exactly what was wanted when every relevant
playbook had been deleted.

It also interacts with
[deleting-a-workflow-silently-discards-its-ledger.md](deleting-a-workflow-silently-discards-its-ledger.md):
`runs` survives deletion and the ledger does not, so run history is the ONLY
thing that could have said what a deleted workflow had done. Wiring it up would
have answered that question directly instead of leaving it at "best-supported
inference".

## What to do

Have the templated run loop open a run in the journal and close it with the
outcome, the same way replay does. Points worth deciding rather than assuming:

1. **`feature`** should distinguish a templated run from a replay, or the two
   become indistinguishable in the same table — the opposite of the point.
2. **`status`** must cover the templated stops that replay has no equivalent
   for: `RunStop::SuspiciousGap`, `RecordDoesNotFit`, `WriteFailed`,
   `Stopped`, `LimitReached`. Collapsing them to "failed" would discard the
   distinction §4.4 spends most of its effort making.
3. **`billable`** is a real decision, not a copy of replay's value.
4. **Nothing may leak content.** §3 keeps values transient; a run row records
   that a run happened and how it ended, never what it moved. The existing
   `RecordReport` is already position-only and is the right shape to follow.
5. **A journal failure must not fail the run.** The same reasoning
   `compile_and_store_playbook` already applies to calibration: refusing to
   run someone's workflow because a bookkeeping row would not write trades
   something they care about for something they have never heard of.

## Not attempted

No code was written. Recorded now because it was found while chasing something
else, and because the next investigation will hit the same wall — the evidence
it needs is not being written down today.

`cargo run --example ledger_dump` prints `runs` alongside each playbook's
ledger, and marks rows whose playbook has been deleted, which is the closest
thing to run history that currently exists.
