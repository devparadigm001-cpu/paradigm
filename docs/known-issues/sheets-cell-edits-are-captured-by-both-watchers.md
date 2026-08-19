# A Sheets cell edit is captured twice, by both watchers, one of them partial

**Status:** open, real defect. Found 2026-08-18 from a live recording.
**Severity: HIGH.** It affects **every** Google Sheets recording, produces a
duplicate `Type` step for every cell edited, and one of the two carries a
**half-typed value**. Replaying it types the same cell twice, the first time
with an incomplete value.
**Where:** the overlap between `capture::text` (`TextFieldWatcher`) and
`capture::grid` (`GridCellWatcher`), joined in `capture::observe_text` /
`capture::observe_grid`.
**Not** caused by the key-up sampling work of 2026-08-17. Nothing in that change
touched `capture::text`. This is pre-existing and was simply never looked for.

## What happens

One cell edit produces two `Type` actions. From a real recording, cell A2, the
user typing `Harbor Point Traders`:

```
step 12  type  A2  "Harbor Point T"        detail: read from element (full value), 2 keystroke(s) over 464ms
step 15  type  A2  "Harbor Point Traders"  detail: grid cell editor, 43 keystroke(s) over 4289ms
```

And cell B2, typing `Ceramic Mug Set`:

```
step 24  type  B2  detail: read from element (full value), 26 keystroke(s) over 4242ms
step 25  type  B2  detail: grid cell editor, 35 keystroke(s) over 4489ms
```

The two `detail` formats are exact signatures of their producers:
`read from element (…)` is `capture::text` (`text.rs:483`), `grid cell editor, …`
is `capture::grid` (`grid.rs:653`).

**The text watcher's copy is not merely a duplicate, it is wrong.** Step 12
records `"Harbor Point T"` — the value as it stood partway through typing. It
saw only **2 keystrokes over 464ms** of a 4.3-second edit, then emitted a
snapshot of the element at emit time. A payload that looks like a complete value
and is not.

## The mechanism

The two watchers were designed to be disjoint. `capture/mod.rs` states the
assumption outright:

> *"A grid cell has no element for the watcher above to follow — it is created by
> typing and destroyed by committing — so it gets its own path."*

That is false for Google Sheets. Its hidden cell editor **is** a real element,
and it is a `ComboBox` — which both watchers accept:

| | rule | verdict on a Sheets cell |
|---|---|---|
| `capture::grid` | `is_cell_editor`: `role == "ComboBox"` and the name looks like a cell ref (`grid.rs:180`) | tracks it |
| `capture::text` | `combobox` is in the followable-text-role list (`text.rs:124`, `:699`) | tracks it |

`capture::text` cannot *start* a watch on a ComboBox from a keystroke — a test
pins that (`text.rs:684`, `keystroke_startable_role("ComboBox")` is false) — but
a **click** starts one, and clicking the cell is how a user begins editing. The
recording shows exactly that: step 11 is `click A2 role:combobox`, and the text
watch begins there. B2 has two such clicks, steps 22 and 23.

The pump then admits both, independently, on the same events:

```rust
if let Some(typed) = observe_text(&pump_watcher, &event) { … s.admit(typed) }
if let Some(cell)  = observe_grid(&pump_grid,     &event) { … s.admit(cell)  }
```

Neither knows the other fired.

## Scope

Every Sheets recording where the user **clicks** a cell before typing, which is
the normal way to edit one. Three cells were edited in the sample recording;
A2 and B2 were clicked and both doubled, C2 was not clicked and produced a
single clean step:

```
step 37  type  C2  detail: grid cell editor, 8 keystroke(s) over 3047ms
```

That contrast is the strongest evidence for the mechanism: the doubling tracks
the click, not the typing.

## Why it matters

* **Replay types the cell twice.** The first write is a partial value.
* **The Rule-of-3 count is inflated.** `detect` collapses by destination cell,
  so this does not currently double a record count — but any consumer counting
  `Type` actions is wrong by 2×.
* **It is invisible on the review screen.** Two steps for the same cell look
  like the user typed twice, and there is nothing on screen to tell them apart
  (see "The diagnosability gap" below).

## What a fix must not do

**It must not simply drop `combobox` from `capture::text`'s role list.** That
list exists because a ComboBox is a legitimate text field in ordinary
applications; removing it silently stops capturing those.

**It must not pick a winner by payload length or "completeness".** The text
watcher's value is a snapshot, so it is *sometimes* complete and sometimes not —
choosing the longer one would be right by accident and wrong silently.

**It must not deduplicate after the fact by (cell, timestamp) proximity.** The
two emissions are 3.8 seconds apart in the A2 case. Any window wide enough to
catch that is wide enough to merge two genuine edits of the same cell.

The likely direction is that the grid path should *claim* an element: if
`capture::grid` is tracking a cell editor, `capture::text` should not also watch
that element. The watchers already share a session; what they lack is a way to
say so.

## Reproducing

1. Start Record Mode. Open a Google Sheet.
2. **Click** a cell, type a multi-word value, commit with Enter.
3. Stop. The review screen shows two `Type` steps for that cell.
4. To see which watcher produced which, the `detail` string is required — and it
   is not on screen. Save the playbook, then
   `cargo run --example dump_playbook -- <id> %APPDATA%\com.amitj.paradigm`.

## The diagnosability gap that made this expensive

`detail` **never crosses the IPC boundary.** `CapturedActionView`
(`commands.rs:49-61`) has no `detail` field — `view_of` drops it — so no screen
can show it and no command exposes it. It is only persisted on compile
(`compile/mod.rs:221-222`, into `action_payload_json`).

The consequence, paid in full on 2026-08-18: the only way to tell these two
`Type` steps apart was to **save the diagnostic recording as a playbook**, dump
the database, read the strings, and delete the playbook again. Before that
detour was found, one live recording was lost to a dismissed review screen,
because a dismissed summary is unrecoverable (`dismissCaptureSummary` clears
frontend state only, and nothing re-fetches it).

**Adding `detail` to `CapturedActionView` and rendering it is a three-line
change** and would have made this defect readable on screen the moment it
appeared. Recorded here because the same gap will otherwise cost the same
detour on the next capture defect.

## Related

* `docs/known-issues/a-grid-edit-restarts-and-re-emits-the-same-value.md` — the
  other duplicate-emission mechanism found in the same recording, in
  `capture::grid` alone.
* `docs/known-issues/click-targets-carry-half-typed-cell-content.md` — found in
  the same recording.
* `docs/known-issues/multiline-document-capture-duplicates.md` — the earlier
  duplicate-emission defect in `capture::text`, same family, different cause.
