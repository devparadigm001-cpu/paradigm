# A permanent correction did not affect the run it was made during

**Status:** FIXED 2026-08-14, and re-driven live to confirm it.
**Probe:** `text_capture_probe -- uicorrection permanent`

## What was measured

A supervised run paused on row 3, which was missing its mapped column `C`. The
user pointed at column `E`, confirmed, and chose **"The format changed — use
column E from now on"**. Then resumed.

```
stored mapping source columns: ["D", "E"]
repointed C -> E in the template: true      <- the correction WAS applied

Sheet2 CSV
  A3=""          expected "GLOBEX LTD"   <- the record they stopped to fix
  A4="Initech"   expected "INITECH INC"
  A5="Umbrella"  expected "UMBRELLA PLC"
```

So the template was genuinely repointed and **the run in progress ignored it
entirely**, including for the record that triggered the pause.

## Why

`run::background::spawn` takes a `CompiledTemplate` **by value**, and the run
loop reads its mapping from that clone for the life of the batch.
`apply_permanent_correction` writes to the database. Nothing re-reads the
template mid-run, so the change lands for the *next* run and not this one.

The one-off path does not have this problem, and the contrast is the point: a
one-off goes into `RunCorrections`, which is a live handle the loop consults
per record. Same panel, same click, and only one of the two answers reaches the
running batch.

## Why it matters more than "it applies next time"

The record was **paused for correction**. The user was looking at row 3, told
the system where the value actually was, and row 3 was then written blank. The
panel's own wording — "use column E from now on" — does not suggest "starting
after the row you are currently fixing".

Worse, the row is now in `workflow_processed_rows`, so a re-run skips it: the
blank is durable and the correction the user made never reaches it at all.

The one-off path handles the same click correctly, which makes the difference
between the two scopes arbitrary from the user's point of view rather than
meaningful.

## The fix, and what it does NOT claim

Option (1) below was implemented: when the panel has a `sourceRow`, a
permanent correction ALSO applies a one-off for that record. Re-driven live:

```
A2="Acme"        before the correction
A3="GLOBEX LTD"  the record the user corrected -- was BLANK before the fix
A4="Initech"     same run, still the snapshot mapping
A5="Umbrella"    same run, still the snapshot mapping

Sheet2 after a SECOND run over new rows
A6="TYRELL CORP"  repointed mapping
A7="SOYLENT LLC"  repointed mapping

stored mapping source columns: ["D", "E"]
```

**Rows 4 and 5 still read the old column, and that is not a miss.** The loop
takes the template by value at `spawn`, so a permanent correction repoints the
STORED mapping while the running batch keeps its snapshot. The fix repairs the
record the user was paused on; the new mapping takes effect from the next run.

That distinction is why the verification runs a second batch over new rows --
rows 2-5 are already in the ledger, so a re-run skips them and the repointed
mapping cannot be observed there at all. Asserting it inside the first run
would have been asserting something the design does not do.

## Candidate fixes, as considered

1. **Apply both when permanent is chosen mid-run.** When the panel has a
   `sourceRow`, a permanent correction also adds a one-off for that record, so
   the record in hand is fixed and every later run uses the new mapping.
   Smallest change, matches what the user was told, and needs no run-loop
   change — but it means one click writes to two places.
2. **Re-read the template per record.** Correct in general and costs a database
   read per record; it also changes the loop's contract from "the mapping is
   fixed for the batch" to "the mapping can move under you", which several
   existing guarantees are worded against.
3. **Refuse the permanent scope mid-run**, offering only one-off while a record
   is paused, with permanent available from the drift path. Honest but narrows
   what §4.5 offers.

(1) is the recommendation. It is not implemented, because it should be verified
by re-driving the permanent path live rather than assumed, and that is a full
run.

## What this does NOT affect

The one-off path is correct and proven live in the same session — including the
row-after proof, which is what distinguishes a scoped correction from a changed
mapping:

```
A2="Acme"        before the correction
A3="GLOBEX LTD"  corrected, from column E
A4="Initech"     UNAFFECTED, still column C
A5="Umbrella"    UNAFFECTED
```
