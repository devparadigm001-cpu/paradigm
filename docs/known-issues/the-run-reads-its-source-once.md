# The run reads its source once, not per record

**Status:** open by design — a deliberate, explicitly user-confirmed tradeoff.
**Decided:** 2026-08-16, by the user, in full knowledge of what is given up.
**Where:** `run::surfaces::open_for`, `source::csv_snapshot::CsvSnapshot`.

> This is not an oversight, and it is not a bug someone forgot to fix. It was
> asked for, with the cost stated up front. If you are reading this because a
> run wrote a stale value, that is the known consequence described below — not a
> new defect.

## What changed

A run whose source is fetched by CSV export now takes **one** export, when the
run's surfaces are opened, and answers every subsequent `peek`, `read` and
`advance` from that one body for the rest of the run.

Previously it re-exported the sheet before **every record**, so each record was
a genuinely live read.

## What you give up

**If the source data changes after a run starts, later records may use outdated
values.** The run is working from a copy taken at the moment it began. Edit row
9 while the run is on row 3, and the run will write what row 9 held at the
start, not what it holds when it gets there.

The tradeoff was accepted on the grounds that most real usage does not involve
editing the source while a run is in progress.

## What you get, measured

The same 9-record sheet, source reads only, two trials each:

| | `open_for` | walk (9 records) | per record | completed |
|---|---|---|---|---|
| per-record fetch | 0.97s | 51.9s | 10.39s | **5 of 9, then failed** |
| per-record fetch | 0.69s | 55.3s | 9.22s | **6 of 9, then failed** |
| one export | 2.66s | 0.0s | 0.00s | 9 of 9 |
| one export | 7.42s | 0.0s | 0.00s | 9 of 9 |

Reproduce with `text_capture_probe livereadtest`.

Two things worth reading off that table rather than just the speed:

* Once the body is in hand, a record read is **free** — no browser, no file, no
  wait. The whole cost moved into `open_for` and is paid once.
* **The per-record path did not finish a nine-record sheet in either trial.** It
  stopped with "the export never downloaded" after 5 and 6 records. Back-to-back
  exports degrade: each one spawns a browser window, and the ones already open
  interfere with resolving the next. So this change is not only a speed
  tradeoff — it removes a reliability failure that had gone unmeasured, because
  earlier timing runs sampled a few records rather than walking a whole sheet.

`open_for` grew by roughly the cost of one export (~1-2s; 7.42s in a trial where
the browser was cold and cluttered). It also now blocks for that fetch, which is
new, and is why the fetch sits at the surfaces seam rather than inside a
`SourceReader` method — see below.

## Effect on §4.5 drift detection: none, and here is why

The obvious worry is that a cached snapshot makes the source-side drift check
compare the cache against itself, leaving it inert. **Checked directly, and that
is not what happens.**

`run::background` reads the source shape exactly once, before the run loop
starts:

```rust
let live_source = reader.shape().unwrap_or(...);   // background.rs:304
...
match crate::run::drift::check(&conn, playbook_id, template, &live_source, &live_destination)
...
let result = run_with_control(...);                 // the loop starts AFTER
```

`run_with_control` — the record loop itself — never calls `shape()` at all.
Confirmed by search: there is no `shape()` call in `run/mod.rs`.

So the source shape is sampled once, at run start, and compared against the
shape recorded at detection time. A body exported at `open_for` is taken at
essentially that same moment, so the check compares **the source's real shape at
run start** against the recorded shape — exactly as it did before. Nothing about
that comparison became self-referential.

What is genuinely lost is narrower than "drift detection": a shape change that
happens *mid-run* is invisible. But it was invisible before too, because nothing
re-checked shape mid-run. **The per-record fetch bought fresher values, not
fresher drift detection.** Values are what this change trades away.

## The destination write path is unchanged

Explicitly: **writes remain live, with their existing verification unchanged.**

`SpreadsheetWriter` was not touched. It still does
`dismiss_editor` → `goto` → `type_here` → read-back-and-compare, per write,
against the real destination cell. Nothing about the destination is cached,
snapshotted, or assumed. The change is confined to how the run obtains source
values.

Verified end to end rather than argued: `text_capture_probe tworundoc` drove a
real nine-record run between two separate documents and checked the result by
CSV export of the destination, not by what the run reported about itself —
9 of 9 rows matched, with no Downloads accumulation.

## Why `CsvSnapshot` rather than a second reader

The reader that did per-record fetching (`source::csv_live::CsvLiveReader`) is
**gone**, not modified. Once it fetches once and serves from cache, it differs
from `CsvSnapshot` in exactly one respect: who calls `fetch_export_blocking`.
Keeping both would have meant two near-identical implementations of the same
interface, differing only in a line of plumbing.

So the fetch moved up to `open_for`, which already knows which document this is,
and `CsvSnapshot` — already the scan reader, already tested — serves the run
too. That also puts the blocking fetch in an `async fn` where blocking is
visible and paid once, instead of hidden inside a sync trait method, which is
what caused the earlier "Cannot start a runtime from within a runtime" panic.

`CsvSnapshot`'s own module docs used to argue *for* per-record freshness on the
grounds that a run "interleaves reads with writes, pauses for §4.5 corrections,
and can sit waiting on a human for minutes." That reasoning is still sound as a
description of the risk. It was weighed and overridden deliberately.

## The guard against this quietly reverting

`source::csv_snapshot::tests::one_export_body_serves_the_whole_run` walks all
nine records of a fixture body and asserts they all come from it. If per-record
fetching is ever reintroduced, `advance` will have to do more than increment,
and that test is where it surfaces.

## Reproducing

```
text_capture_probe livereadtest    # source-read timing, before/after
text_capture_probe tworundoc       # full run, verified by destination CSV export
```
