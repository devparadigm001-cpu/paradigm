# The pitch floor returns a harmonic instead of declining

**Status:** FIXED 2026-08-23. Measured 2026-08-22 on three independent
surfaces, and replaced by periodicity detection rather than a smaller number --
see `docs/planning/Pitch-Discrimination-Without-A-Floor.md`. Kept because the
derivation below is why no floor could have worked, and that reasoning outlives
the constant.
**Severity: HIGH.** `detect::candidates` was expected to produce *nothing* on a
dense list. It produces **confident wrong records** instead, grouping N rows into
one. Silent-wrong-target, which is the failure class this project keeps finding.
**Where:** `detect::candidates::record_pitch`, via `RECORD_PITCH_FLOOR_PX`.

## The prediction, and how it was wrong

On 2026-08-22 I predicted, before recording, that a table with a 33px row pitch
would defeat `RECORD_PITCH_FLOOR_PX = 120.0`: no difference would clear the
floor, `record_pitch` would return `None`, and no positional candidates would
surface. Declining is bad but honest.

**That is not what happens.** Measured while screening test surfaces:

| surface | nodes | true pitch | pitch at the 120px floor |
|---|---|---|---|
| File Explorer, Projects folder | 121 | 32.0px | **128.0px** (4x) |
| File Explorer, System32 | 182 | 28.4px | **140.5px** (5x) |
| ftp.gnu.org directory index | 3000+ | 26.0px | **130.0px** (5x) |

Every one returns a **harmonic of the true pitch**, and none declines.

## Why it is not luck — the arithmetic is forced

For a list of `n` rows at pitch `p`, every pairwise y-difference is `k*p`. The
floor `F` admits only `k >= ceil(F/p)`. Support for `k*p` is `n - k`, which
strictly decreases as `k` grows, so the most-supported surviving difference is
always the smallest admitted one:

```
record_pitch  ->  ceil(F / p) * p
```

Check it: 120/32 = 3.75 -> 4, and 4*32 = 128. 120/28.4 = 4.23 -> 5, and
5*28.4 = 142 (measured 140.5). 120/26 = 4.6 -> 5, and 5*26 = 130.

The "prefer the smallest among equally-supported clusters" rule was written to
beat exactly this, and it cannot help here: support is not equal, it is strictly
ordered, and the fundamental has been excluded from the vote before the rule
runs.

**So on any list denser than the floor, `record_pitch` returns a wrong pitch
deterministically.** Not sometimes. Always.

## What that costs

`assign_records` divides by that pitch, so four or five real rows collapse into
one "record". The Rule of 3 then counts those merged blocks as records, and the
user is shown a candidate claiming a repeating pattern across records that do not
exist. The collision check does not catch it: rows merged this way sit at
different bands within the false record, so nothing collides.

## What a fix must not do

**It must not simply lower the floor.** The floor exists because on OrderFlow's
card layout the within-record spread (116px) exceeded the between-record gap
(99px), so field offsets would win the vote and be mistaken for record spacing.
Removing it reintroduces that. The two failures pull in opposite directions and a
single constant cannot separate them — which is the real finding here, and the
reason this is filed rather than patched with a smaller number.

The shape of an answer is to stop treating the fundamental as unavailable:
detect the pitch with **no floor**, then decide whether the winner is a record
pitch or a within-record offset using something other than magnitude — for
instance whether dividing by it produces groups of consistent size and
composition. That is a real piece of design work, not a constant.

## Reproducing

```
cargo run --example pitch_candidate_probe -- <window-title-substring>
```

It reports the tree size, the walk time against the 400ms cap, the true pitch
with no floor, and the pitch the shipped floor yields. Any surface whose two
pitch numbers differ by an integer factor is this issue.

## Related

* `docs/planning/Filtered-Post-Hoc-Confirmation.md` — where the floor is
  described as "the weakest number in the build". It is weaker than that
  described: it does not merely fail on dense lists, it corrupts them.

## Fixed 2026-08-23

`RECORD_PITCH_FLOOR_PX` is gone. `record_pitch` now takes the period that
explains the layout most economically: coverage at or above `COVERAGE_THRESHOLD`
(0.80), at least three blocks, then fewest offset clusters and smallest period.

**The defect is closed structurally, not by tuning.** Coverage is monotone --
`coverage(p) >= coverage(k*p)` -- so a fundamental that passes always beats its
own multiples, and no harmonic can be returned while its fundamental qualifies.
`a_dense_list_never_returns_a_harmonic` pins it across five pitches.

The four layouts in the table above now resolve to their true pitch, pinned in
`the_record_pitch_matches_four_real_layouts`. The threshold that replaced the
floor is a **ratio**, which is the actual fix: a length cannot serve page
densities that differ by more than a factor of ten, and a ratio is unchanged by
scale.
