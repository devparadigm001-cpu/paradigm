# Calibration: no confidence samples are ever recorded in production

**Status: FIXED** (2026-08-08, commit `0e1012f`). Calibration samples are now
recorded from the real labelling path. The everything-below describes the
original finding; see "The fix" for what changed and "Still open, and correctly
so" for the one part deliberately left for Phase 2.
**Originally:** confirmed by direct inspection of the real on-device database.
**Root cause identified** — the recording call was simply absent from the
product code path.
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

## The fix (2026-08-08, commit `0e1012f`)

`compile_and_store_playbook` builds a `CalibrationSample::from_outcome` after
`engine.label()` returns and records it against the connection already in
scope — the real database, not a temp path. The labelling branch returns the
sample as a third element, because `outcome` was previously consumed by
`(outcome.label, true)`.

```rust
if let Some(sample) = &calibration_sample {
    if let Err(e) = calibration::record(&mut conn, sample) {
        eprintln!("[paradigm] calibration sample not recorded: {e}");
    }
}
```

Two decisions, both commented at the call site:

* **Recorded before `store::store`**, so a store failure does not discard an
  observation that is already valid. The model ran either way — the sample is
  about the model, not about whether this playbook was saved.
* **A recording failure is logged and dropped, never propagated.** Refusing to
  save a user's recording because a statistics row could not be written would
  trade something they care about for something they have never heard of.

### Verification

Three tests in `tests/ipc_commands.rs`, which went from 12 to 15 tests:

| Test | What it establishes |
|---|---|
| `model_labelling_records_a_calibration_sample` | An empty name hint routes through the model and a real row appears. Reads the row back rather than counting it: non-blank `model_source`, bin bounds a sane half-open tenth inside `0..=1`, `sample_count` exactly 1, `success_count` in range, and `normalized_score` still NULL |
| `supplying_a_name_records_no_calibration_sample` | A caller-supplied name records nothing, asserted explicitly rather than left as an absence someone might notice |
| `a_calibration_failure_does_not_prevent_saving_the_playbook` | **The adversarial one.** Drops `confidence_calibration` so `record` genuinely fails, then confirms the playbook still saves — verified by finding it in `list_playbooks`, not by trusting the command's own report |

The third is the one that matters most for the design decision above. Without
it, "calibration failure must not block a save" would be a comment rather than a
behaviour anything checks.

Suite at the time of the fix: 76 lib, 8 `db_encryption`, 15 `ipc_commands`, 2
`replay_aborted` passing. `ipc_pipeline` remains deliberately red on the
unrelated no-settle race. Clippy clean on both changed files.

The `normalized_score IS NULL` assertion deserves a note: it is not incidental.
Phase 1 records raw observations only, and that test will fail the moment
something starts populating the normalised column early.

## Still open, and correctly so: nothing reads the data back

Samples now accumulate. **Nothing consumes them yet** —
`calibration::bins_for` exists, is tested, and has no caller in product code.

**This is not a new defect.** It is the division of labour the schema was
designed around: migration `20260803000003` comments `normalized_score` as
"Written by Phase 2's calibration pass. NULL = not yet calibrated." Phase 1's
job is to collect honest raw observations; turning them into a calibrated
mapping is explicitly Phase 2's.

So the correct reading is that the collection half is done and the consumption
half is scheduled, not missing. The thing that *was* wrong — samples never being
collected at all, so Phase 2 would arrive to an empty table — is fixed.

One consequence worth stating plainly for whoever builds Phase 2: samples only
ever accumulate from sessions the user does **not** name, because a supplied
name means no model runs and there is no confidence score to calibrate. That is
correct behaviour, but it means the sample rate is a function of user naming
habits rather than of session count, and the table will fill more slowly than a
count of recordings would suggest.

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

1. ~~**Wire it.**~~ **Done** — commit `0e1012f`, see "The fix". Failure is
   logged and dropped, deliberately.
2. ~~**Test it against a real path**, not a probe.~~ **Done** — three tests in
   `tests/ipc_commands.rs` driving the real IPC command, including one that
   drops the table to prove a calibration failure cannot block a save.
3. **Check the other candidate producers.** Chat Mode intent parsing and form
   Q&A generation (Phase 2) will produce `LabelOutcome`-shaped confidence too.
   Wiring one call site now is right, but the recording point should be somewhere
   every future model call can reach rather than copied per command.
4. **Re-verify against the real store**, with
   `cargo run --example calibration_dump -- <app_data_dir>`, once a real session
   has been saved *without* a name. The tests prove the wiring against a scratch
   database; this would confirm it on the on-device one. Not yet done — the
   table was last dumped before the fix.
5. **Consider whether Phase 2 needs a faster sample rate.** Samples only accrue
   from unnamed sessions (see "Still open"), so the table fills as a function of
   naming habits rather than usage. If that proves too slow, the options are to
   record confidence from other model calls (item 3) or to reconsider the
   assumption that a supplied name means nothing worth calibrating.
