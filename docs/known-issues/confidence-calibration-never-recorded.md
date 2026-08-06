# Calibration: no confidence samples are ever recorded in production

**Status:** confirmed by direct inspection of the real on-device database.
**Root cause identified** — the recording call is simply absent from the product
code path. Not fixed.
**Affected:** `confidence_calibration` (migration 20260803000003),
`src/labeling/calibration.rs`, `src/commands.rs::compile_and_store_playbook`.
**Found:** 2026-08-05, during Phase 1 Step 13 (confirm calibration data
collection is running). It is not running.
**Severity: HIGH — blocks Phase 2.** Nothing in Phase 1 misbehaves, and no data
is lost or corrupted. What is lost is time: Phase 2's calibration mapping needs
a body of accumulated real samples to normalise against, and that body is not
being built. Every Record Mode session that runs before this is fixed is a
sample that cannot be recovered afterwards. **This needs a code fix, not just a
note.**

## Summary

`confidence_calibration` is empty in the real database and always will be.
`calibration::record` has exactly one call site in the entire tree, and it is in
a probe that writes to a temporary directory. No product code path calls it, so
real usage cannot populate the table.

The table, its schema, the binning logic, `CalibrationSample::from_outcome`, and
`calibration::bins_for` all exist and are tested. Only the call that connects
them to real usage is missing.

## Evidence

### The real database is empty

`cargo run --example calibration_dump -- %APPDATA%\com.amitj.paradigm`, run
against the production store on 2026-08-05:

```
app data dir : C:\Users\amitj\AppData\Roaming\com.amitj.paradigm
database     : C:\Users\amitj\AppData\Roaming\com.amitj.paradigm\paradigm.db
db size      : 98304 bytes
wal size     : 98912 bytes

confidence_calibration: 0 row(s)
  (EMPTY -- no calibration samples recorded in this database)

-- context from the same database --
  playbooks        1
  playbook_steps   65
  runs             1
  run_steps_log    1

  run_steps_log by model_source:
    (null)                             1  first=2026-08-06T03:57:24.757Z  last=2026-08-06T03:57:24.757Z

  most recent playbooks:
    2026-08-06T02:57:43.521Z  record_mode  Step 11a proof recording           1ef92b5f-c321-40fe-af61-eb28b252a7e1
```

Three consecutive runs, identical output. This is not an empty install: real
Record Mode work happened here tonight — one playbook of 65 steps was recorded,
saved, and replayed. Zero calibration samples were captured for any of it.

### The only call site is a probe, and it writes to a temp directory

```
$ grep -rn "calibration::record" . --include=*.rs
./examples/clean_tag_probe.rs:319:            if let Err(e) = calibration::record(&mut conn, &sample) {
```

One hit, whole tree. And that probe's connection comes from
`examples/clean_tag_probe.rs:316`:

```rust
let (db_path, key_path) = db::paths_in(&tmp);
```

So even when the probe runs, its samples land in a scratch database, never in
`%APPDATA%\com.amitj.paradigm`. (`Temp\paradigm-probe-calibration\paradigm.db`
on this machine is where they actually went.)

### The product path never reaches it

`commands.rs::compile_and_store_playbook` is the real save path. It calls
`clean()` — but only in the branch where the caller supplied no name:

```rust
let (label, label_generated) = match name_hint.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
    Some(hint) => (hint.to_string(), false),
    None => {
        let engine = labeling::shared(&model_path(&state))?;
        let cleaned = clean(&actions, &redaction);
        let outcome = engine.label(&cleaned.description)?;
        (outcome.label, true)
    }
};
```

`src/labeling/clean.rs` contains no reference to calibration at all.

## Where the fix goes — and where it does not

**Not in `clean()`.** This is worth stating explicitly because it is the
intuitive guess and it cannot work. `clean()` runs *before* inference and
returns a `CleanedPattern`; it never sees the model. A calibration sample needs
`mean_token_probability` and `repaired`, which only exist on the `LabelOutcome`
that `engine.label()` returns. `clean()` has nothing to record.

**The correct point is immediately after `engine.label(...)`,** in the `None`
branch above, where `outcome` is in scope. The constructor for exactly this
already exists and is currently unused by product code:

```rust
CalibrationSample::from_outcome(&outcome)   // src/labeling/calibration.rs
```

so the change is roughly: build the sample from `outcome`, then
`calibration::record(&mut conn, &sample)` against the connection the command
already holds in `state.db`.

### A second, legitimate reason tonight's count is zero

Even once wired, this branch only runs when `name_hint` is empty. Tonight's
playbook was saved as `"Step 11a proof recording"` — a supplied name — so no
model call happened and there was genuinely nothing to calibrate. That is
correct behaviour, not a second bug, but it means **fixing the wiring alone will
not produce samples from user-named saves.** Any test of the fix must exercise
the auto-label path, and whoever plans Phase 2 should know that samples
accumulate only from sessions the user does not name.

## Why it matters

Phase 2's job is to turn the model's raw self-reported confidence into a
calibrated probability. That mapping is derived from observed data: for each
raw-score bin, how often did the model actually produce well-formed output. With
an empty table there is nothing to derive it from, and Phase 2 either ships
uncalibrated or waits for data collection to start and then accumulate.

The cost is strictly time, and it is already being paid. Sessions that ran
before the fix cannot be back-filled: `mean_token_probability` is not persisted
anywhere else, so the evidence for those generations is gone once the process
exits. Every day this stays unwired is a day of sample collection that has to
happen later instead.

Scope is small. This looks comparable to the `step_indices` and
`irreversible_count` additions of the same evening: a handful of lines at one
call site, plus a test that the row lands.

## Next steps

1. **Wire it.** Record a `CalibrationSample::from_outcome(&outcome)` after
   `engine.label(...)` in `compile_and_store_playbook`. Decide deliberately
   whether a failure to record is logged-and-ignored or surfaced — a
   calibration write failing should almost certainly not fail the user's save.
2. **Test it against a real path**, not a probe. The existing IPC tests seed
   `pending_actions` and pass a `nameHint`, which skips labeling entirely; a
   test for this must omit the hint so the model branch runs.
3. **Check the other candidate producers.** Chat Mode intent parsing and form
   Q&A generation (Phase 2) will produce `LabelOutcome`-shaped confidence too.
   Wiring one call site now is right, but the recording point should be somewhere
   every future model call can reach rather than copied per command.
4. **Re-verify with `cargo run --example calibration_dump -- <app_data_dir>`**
   after the fix, against the real store, and confirm rows appear with
   `normalized_score` still NULL — Phase 1 records raw observations only;
   populating `normalized_score` is Phase 2's calibration pass.
