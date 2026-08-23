# An action cannot say which window it happened in

**Status:** open, found 2026-08-22 in session record-1a2123c0.
**Severity: MEDIUM.** It makes positional record detection wrong whenever a
recording touches two windows of one application, which is the ordinary shape of
browser work: a page in one tab, a spreadsheet in another.
**Where:** `CapturedAction::source_app` is the process name, and
`CapturedAction` carries nothing finer.

## The measurement

A natural OrderFlow recording produced a candidate claiming **4 records** for a
page that shows three. `Pending` sits at y234, y407 and y580, and nowhere else.

`detect::candidates::assign_records` derives one record pitch from every
position in the recording. This session mixes an OrderFlow page (x≈2000–2700)
with a Google Sheets window (x≈3000–3650), and dividing that pooled range by a
single pitch manufactures a record that was never on screen.

Partitioning by application was added on the same day and **changed the result
not at all**:

```
step   1 click     target="Harbor Point Traders"   app=msedge.exe
step  12 click     target="A2"                     app=msedge.exe
```

Both windows are `msedge.exe`. The partition is correct — it stops a desktop
application's coordinates being pooled with a browser's — and it cannot help
here.

## The fix: wire the discarded page_identity URL onto the action

`capture::grid::page_identity` computes a URL for the position path, falling
back to the window title, and it runs on every `Ctrl+C` and `Ctrl+V`. It is used
to build the source position and then **discarded**. That value is the
discriminator this needs, and storing it is the fix.

Three things that have to hold, each of which is why this is not a one-line
change:

1. **It must ride an existing read.** `page_identity` runs on clipboard keys,
   not on every click, and the clicks are what carry the positions being
   grouped. Paying for a fresh walk per click is what
   `position-reads-dominate-an-ordinary-copy-paste-session.md` measures at
   ~432ms. Either the value is cached per window and reused, or the click path
   reuses the id already read for `last_click`.
2. **It must survive into the stored playbook**, because `candidates` also runs
   after the fact over saved steps -- `examples/candidates_from_recording.rs`
   depends on that.
3. **A missing discriminator must decline, not default.** An action that cannot
   say which window it belongs to should stay out of every positional group
   rather than joining the largest one.

## What a fix must not do

**It must not use the window TITLE as the discriminator.** A title changes as
the user works — `two-documents-in-one-window-cannot-both-resolve.md` and
`two-open-spreadsheets-kill-the-formula-bar.md` both record what happens when
titles are trusted — and a discriminator that changes mid-recording splits one
window into several, which is the same class of error as the one being fixed.

**It must not be read on a new walk.** `position-reads-dominate-an-ordinary-copy-paste-session.md`
measures what an extra traversal costs on the click path. Whatever is stored has
to come from a read that already happens.

**It must not silently group when the discriminator is missing.** An action with
no window identity should decline to join a positional group rather than default
into the largest one.

## Reproducing

1. Record a transfer between a web page and a browser-hosted spreadsheet.
2. Save it, then:
   `cargo run --example candidates_from_recording -- <id> %APPDATA%\com.amitj.paradigm`
3. Compare the positional candidate's record count against the number of records
   actually visible on the page.

## Related

* `docs/planning/Filtered-Post-Hoc-Confirmation.md` — the pipeline this breaks,
  and the recording it was found in.
* `docs/known-issues/element-bounds-are-viewport-relative-so-scrolling-moves-them.md`
  — the other reason a positional record count can be wrong. This one was
  initially mistaken for that one.

## It manufactures whole candidates, not just an inflated count (2026-08-23)

The Amazon session record-6ca9bd5a produced five candidates. Three are
spreadsheet columns and correct. **The other two exist only because of this
defect.**

The Amazon page's own click positions are
`{-220, -3, 0, 44, 80, 212, 229, 349, 482, 513, 536, 548, 634, 661}`, and
`record_pitch` over that set returns **`None`** — no period, no records, which
is right, because the user opened one product at a time and the page positions
do not repeat.

Pool in the Google Sheets window from the same `msedge.exe` process, adding
`{310, 312, 342, 372}`, and a period appears: **315px at coverage 0.82**, barely
over the 0.80 threshold, against the 1.00 a real list scores. Under it:

```
  y=  -3, 0    record 0, band 220px      <- Amazon
  y= 310, 312  record 1, band 220px      <- the Sheets GRID CANVAS
  y= 634       record 2, band 220px      <- Amazon
```

That band is `cand-4`, 29 actions — and its steps are dominated by clicks at
`x3002 y310`, which is the spreadsheet canvas. `cand-5` is the same shape: one
Amazon product title at y661 grouped with three clicks on the browser's **Back**
button at `x1925 y44`.

**These are not systematic incidentals.** A systematic incidental is a real
repeated element that the user genuinely touches once per record -- the
`Pending` status in the OrderFlow recording is one. These are unrelated elements
from two different windows, grouped by a period that exists only because their
coordinate systems were mixed.

So the consequence is worse than the four-records-from-three already recorded
here: on a page with **no** repeating structure, pooling can invent one.
