# Capture is unreliable against complex web grids (Google Sheets)

**Status:** two distinct problems.
**Finding 2 (cell capture AND replay) is FIXED, 2026-08-11** — `capture/grid.rs`
captures cell edits with correct attribution and clean text, and `replay`'s
`grid_type` reproduces them into a different document through the Name Box, with
no coordinates involved. Both verified against CSV exports. See "Finding 2 FIXED"
and "Replay of grid edits: implemented".
**Finding 1 (clipboard) is now scoped and partly closed, 2026-08-11.** It turned
out to be **narrower than recorded**: pasting into an ordinary text field is
already captured. What remains is pasting into a grid cell, which is confirmed
still lost — and is now *counted and surfaced* rather than silent. Modelling
clipboard content itself is deliberately not done; the reasoning is under
"Finding 1, re-measured".
**Affected:** `src-tauri/src/capture` — clipboard handling (absent) and typed
text against `combobox`-role grid cells.
**Platform:** Windows. Observed in Google Sheets. Other grid UIs untested.
**Found:** 2026-08-06, during Phase 1 Step 12 — the first real human end-to-end
test of the whole app.
**Severity: HIGH for the product, not blocking Step 12.** Spreadsheets are a
prime target for the repetitive data-entry work this product exists to automate,
so unreliable capture there is a gap in the core use case rather than an edge
case. It is explicitly *not* a blocker in the way the text-truncation defect
was: Step 12 ran, completed, and produced informative results — **this finding
is one of those results.**

## Summary

Every probe before Step 12 targeted simple `<input>` elements or Notepad. The
first session against a real, canvas-rendered, virtualized grid surfaced two
problems that no prior test could have caught:

1. **Clipboard operations produce no captured action at all.** Copy and paste —
   the actual mechanism of the user's task — are invisible to the pipeline.
2. **Typed text into Sheets' cell editor is frequently empty or garbled, and
   cell attribution drifts** between the click and the type action.

Both were found by a human doing a real task, not by a probe.

## Evidence

### Finding 1 — clipboard operations are not captured

> **Partly superseded 2026-08-11.** The session evidence below is accurate, but
> the generalisation is not: pasting into an ordinary text field **is** captured
> now, as a side effect of the 2026-08-06 direct-read change. Only grid-cell
> pastes are still lost. Re-measured both ways — see "Finding 1, re-measured".

Session `record-17bbca6a-4baf-4fe2-bfba-dbb1df918e24`:

| | |
|---|---|
| Captured actions | 60 |
| Unmapped raw events | 450 |
| `type` actions | **1** |

The user's task was copying data from Notepad and pasting it into Sheets. The
single `type` action was a date the user re-typed by hand — **not** from a
paste. The copy and paste operations themselves produced no captured action of
any kind, so the moment of data transfer, which is the entire substance of the
task, is missing from the recording.

#### On the suspected cause — partly confirmed, partly not

The hypothesis going in was that `Ctrl+C`/`Ctrl+V` are classified as neither
trigger keys nor typing keys in `capture/text.rs`. Checking the code, that is
half right, and the wrong half matters:

* **Not trigger keys — confirmed.** `is_trigger_key` is only `0x0D` (Enter) and
  `0x09` (Tab).
* **Not typing keys — false.** `Ctrl+C` and `Ctrl+V` arrive as key codes `0x43`
  (`C`) and `0x56` (`V`), both inside `is_typing_key`'s `0x41..=0x5A` range. And
  nothing in `src/capture` reads `ctrl_pressed` at all — verified by grep. So a
  paste currently increments the keystroke counter **exactly as if the user had
  typed a literal `V`**, inflating the count and, since the emit condition is
  "value changed OR keystrokes seen", potentially triggering an emit for the
  wrong reason.

**The more direct cause is upstream of the key handling entirely**
(`src/capture/mod.rs:91`):

```rust
record_clipboard: false,
```

Clipboard recording is switched off in the `WorkflowRecorderConfig`, with the
comment "Not needed for Phase 1 and each is another source of captured content
we would have to gate." No clipboard event is ever emitted, so no amount of
key-classification work will surface a paste on its own. Any fix starts here.

Neither cause is confirmed as *the* explanation for the observed recording —
they are what the code says, not what was measured on session 17bbca6a.

### Finding 2 — empty/garbled payloads and cell attribution drift

Session `record-50020dc5-99d6-487a-9266-9818ffce8cd3`, manual typing only, no
clipboard involved. Three `type` actions were captured against `combobox`-role
cells, and **2 of the 3 carried empty or corrupted payloads**:

* one payload empty;
* one reading `"19"` plus a stray invisible character, rather than the value
  actually typed.

Separately, and possibly a different problem: clicks and types were sometimes
attributed to **different cells** — a click on `C1` followed immediately by a
`type` action logged against `B1`. Sheets identifies "which cell" through its
Name Box accessibility element, which appears to update asynchronously; at the
moment a type action completes it does not reliably reflect the truly-focused
cell.

This resembles the no-settle race in
[text-input-capture-truncation.md](text-input-capture-truncation.md) — a value
read before the UI has caught up — but against a `combobox`-role grid cell
rather than a standard `<input>`. **Do not assume they are the same defect.**
The differences are substantial enough to matter: a canvas-rendered virtualized
grid, a role that is not `edit`, and a separate element (the Name Box) standing
in for the target's identity. A stray invisible character in the payload is also
unlike anything the `<input>` investigation produced. Whether it is one root
cause or two is an open question, and answering it is the first task below.

### Secondary: already-tracked issue confirmed in the wild

Session `17bbca6a` also captured a `navigate` action to **Paradigm's own
window** mid-recording. That is a live, real-world confirmation of the
system-wide capture scope documented in
`docs/known-issues/record-mode-unscoped-system-wide-capture.md` — recorded here
only as corroboration; see that doc for the issue itself.

Note that doc currently exists on the `frontend-dev` branch only, so the link
above will not resolve from `backend-dev` until the branches meet.

## The mechanism, measured (2026-08-10)

`text_capture_probe -- sheets` opens a blank `sheets.new` document and inspects
what UIA actually exposes. Two runs, identical results, including runtime ids.
No fix attempted — this establishes what a fix would be working with.

The two standing hypotheses made opposite, checkable predictions. If cells are
canvas-rendered, moving between them changes nothing about the focused element,
because there is no per-cell element to change to. If the Name Box is an async
indirection, a per-cell identity *does* exist and the question is only when it
settles.

### H1 — canvas-rendered cells: CONFIRMED, and more completely than expected

The entire Sheets browser window is **56 accessible nodes** to depth 12. Printed
in full, the document subtree is:

```
  Document "Untitled spreadsheet - Google Sheets"
    Pane ""
```

That is all of it. No `DataItem`, no `Table`, no `Grid`, no rows, no columns.
**Zero elements anywhere in the window have a name that looks like a cell
reference.** For comparison, the browser's own chrome — toolbar, tab bar, address
bar — accounts for most of the other 54 nodes.

Moving the cursor confirms it. The focused element across five cell positions:

```
  start              role=Edit  id=152289  name="" value="" bounds=(0,0,1,1)
  after Right        role=Edit  id=152289  name="" value="" bounds=(0,0,1,1)
  after Right        role=Edit  id=152289  name="" value="" bounds=(0,0,1,1)
  after Down         role=Edit  id=152289  name="" value="" bounds=(0,0,1,1)
  after Left         role=Edit  id=152289  name="" value="" bounds=(0,0,1,1)

    distinct focused-element ids   : 1
    distinct focused-element names : 1
    distinct focused-element bounds: 1
```

Focus never moves. It sits on a **1×1 pixel unnamed hidden input** — the same
runtime id `152289` in both runs, in two different documents — which is where
Sheets receives keystrokes. There is nothing per-cell to attribute an action to,
because at rest no cell exists as far as the accessibility tree is concerned.

### The cell editor: a ComboBox that exists only while editing

This is the part neither hypothesis predicted, and it explains the original
finding's puzzling `combobox` role.

The moment typing starts, an element **appears** that was not in the tree before:

```
  before typing      role=Edit      id=152289  name=""   bounds=(0,0,1,1)
  +1 ms   focused    role=ComboBox  id=298118  name="D3" bounds=(2172,320,95,16)
                     text="7391\n"
```

A `ComboBox`, **named after the cell reference**, positioned at the cell's real
screen coordinates, carrying the typed text. So the `combobox`-role "cells" in
session `50020dc5` were never cells — they were the **cell editor overlay**,
which exists only during editing and is destroyed on commit.

That reframes Finding 2 entirely. Capture is not misreading a cell; it is reading
a transient editor whose lifetime is shorter than the action being recorded.

### H2 — asynchronous Name Box indirection: REFUTED

The Name Box does exist and does track the cursor, but it is not slow.

The element found by name is a `Group` (`"Name box (Ctrl + J)"`), whose child
`Edit` holds the reference. It tracked every move exactly: `B2` → `C2` → `C3` →
`D3`. Sampling every 25 ms after a keypress:

```
    before keypress            "C3"
    CHANGED at +1 ms           "D3"
    settled                    "D3"
```

`+1 ms` in one run, `+0 ms` in the other. **The Name Box is effectively
synchronous with the keypress**, and cannot account for attribution drift on any
timescale capture operates at. The hypothesis in the original write-up — "it
appears to update asynchronously; at the moment a type action completes it does
not reliably reflect the truly-focused cell" — does not survive measurement.

### Where the cell identity actually lives, and why that matters

The identity is reachable, but through an accessor that is easy to miss:

| Element | `name` | `value` | `text(0)` |
|---|---|---|---|
| Name Box input | `""` | `""` | **`"D3"`** |
| cell editor (while editing) | **`"D3"`** | `""` | `"7391\n"` |

Both are empty on `value`. The Name Box carries the reference **only** in
`text()`; the editor carries it **only** in `name`. So which cell an action
belongs to is available from two different elements through two different
accessors, and neither is `value`.

This is worth pinning down before any fix: capture builds a click's
`element_name` from `e.element_text` (`capture/mod.rs:299`) and a type action's
from `watched.name` (`capture/text.rs:319`). Those are different accessors on
different elements, which is exactly the shape that produces a click and a type
disagreeing about the cell. Stated as a lead, not a conclusion — this was
measured on the UIA side, and the recorder's mapping to `element_text` /
`watched.name` was read from the code, not instrumented.

### The stray invisible character, identified: U+FEFF

Session `50020dc5` reported a payload of `"19"` "plus a stray invisible
character", unlike anything the `<input>` investigation produced. It reproduces,
and it has a name. After committing with Enter, the editor's text reads:

```
    text="\u{feff}\n"
```

**U+FEFF**, the zero-width no-break space. Sheets seeds its hidden editor with
it. Any payload read from that element inherits it. That is a concrete, testable
cause for the corrupted-payload half of Finding 2, and it is unrelated to the
no-settle race in `text-input-capture-truncation.md`.

### What could NOT be established

Two things, recorded so they are not mistaken for settled:

1. **The typed value could not be verified as landing.** The probe's own premise
   check failed:

   ```
     did '7391' reach anything UIA can read: false
     PREMISE UNVERIFIED for phase D
   ```

   The text was visible in the editor *during* editing (`text="7391\n"`), but
   after Enter no readable element reports it. Two different explanations fit —
   the commit did not happen, or it happened and the committed cell value is
   simply not exposed to UIA (which would follow directly from H1) — and this
   run cannot separate them. The second is more likely given everything above,
   but "more likely" is not measured.

2. **An unexplained position after commit.** After Enter, the reported cell is
   `"Z3"`, not the expected `D4`, on an element with runtime id `461295` — the
   **same id in both runs, in two different documents**. A stable id across
   documents suggests a fixed element in Sheets' implementation rather than a
   real cursor position, but that is a guess. It reproduces exactly, so it is
   not noise, and it is not understood.

### Screen-reader support does not change the answer (2026-08-10)

The one untested variable flagged above — Sheets has an accessibility mode, off by
default — has now been checked. **It does not materialise cells.**

`text_capture_probe -- sheetsa11y` measures the same document before and after
toggling the mode, rather than comparing against the earlier session's numbers,
so the comparison controls for document, window and machine state.

The toggle needed confirmation that does not come from the thing being measured:
if it silently failed, "the tree did not change" would look identical to "the
mode changes nothing", and those are opposite findings. Sheets announces the
change itself, and that announcement is the independent evidence:

```
  sending {ctrl}{alt}z
  confirmed by Sheets: "Screen reader support enabled."  (after 676 ms)
```

With the mode confirmed on:

```
  measure                              before    after
  accessible nodes                        110      113
  role DataItem                             0        0
  role Table                                0        0
  role Grid                                 0        0
  role Cell                                 0        0
  role DataGrid                             0        0
  cell-reference-looking names              0        0
  distinct focused ids over 4 cells         1        1
  distinct focused names over 4 cells       1        1
```

Three nodes appeared, none of them a grid role. No element anywhere is named
like a cell reference. Focus still does not move between cells.

The mode is not inert — it swaps *which* hidden element receives focus:

| | focus host |
|---|---|
| mode off | `Edit` id `152289`, bounds `(0,0,1,1)` |
| mode on | `Group` id `176391`, bounds `(2014,-9878,5,3)` |

An offscreen 5×3 group at y = −9878 replaces the 1×1 input. Still one static
element for the whole grid, still no per-cell identity. That the focus host
changed at all is further evidence the toggle genuinely took effect.

**Canvas rendering persists regardless of the accessibility setting.** The
constraint below is therefore not a default-settings artifact that a user could
opt out of — it is the shape of the problem.

The setting was returned to its original state at the end of the run
(`"Screen reader support disabled."`), since it is a persistent per-account Docs
preference.

## Part 2: prototyping capture from the transient editor (2026-08-10)

Given that no persistent cell exists, the design question is whether the editor
overlay can be read *during its lifetime*. Prototyped with
`text_capture_probe -- sheetsedit` and `-- sheetswatch`, and validated against
Sheets' own CSV export rather than against the UI that produced the reading.

**Result: the mechanism is plausible and NOT validated.** One run read the editor
correctly 3/3 against the saved file; a second read it correctly 1/3. Both open
anomalies from Part 1 are resolved, and one of them turns out to have been
sabotaging every measurement in this investigation.

### The editor's lifetime, measured

Sampling the focused element every 25 ms through one edit:

```
  +0    ms  role=Edit       name=""   text="\n\n"
  +75   ms  role=ComboBox   name="A1" text="111"
  +245  ms  role=ComboBox   name="A1" text="111\n"
  (Enter sent at +1295 ms)
  +1340 ms  role=ComboBox   name="Z1" text="﻿\n"
```

The editor appears **75 ms** after typing begins, already carrying the correct
cell name and clean text, and survives until commit — a read window of roughly
1.2 s. That is ample.

**The U+FEFF is a post-commit artifact, not a property of the text.** During
editing the text is clean `"111"`. Only after commit does the element read
`"﻿\n"` — *and* report a different cell. So a read taken slightly too late
produces a garbled payload **and** a wrong cell attribution, from one mistake.
Part 1 treated those as possibly two problems; this suggests a single cause,
which is a lead worth carrying rather than a proven claim.

### The decisive negative: the existing pipeline sees none of it

A real `CaptureSession` was run across three cell edits:

```
  capture produced 1 action(s), 52 unmapped event(s)
    navigate  role="Window" name="Untitled spreadsheet - Google Sheets …"
```

**Zero `type` actions.** Fifty-two events arrived and none mapped to a cell edit.
So this is not a matter of tuning when capture reads — the event-driven pipeline
does not currently surface the editor at all. Any fix has to add a mechanism, not
adjust an existing one.

### Anomaly 1 resolved: the values do commit

Verified against the CSV export, which is the saved document rather than the UI:

```
  "111,,,,,,,,,,,,,,,,,,,,,,,,,alpha"
  ",,,,,,,,,,,,,,,,,,,,,,,,,betagamma"
```

All typed values were present in the saved file, in both runs. Part 1 could not
distinguish "the commit did not happen" from "committed values are invisible to
UIA". It is the second: **commits work; UIA simply cannot see cell contents.**

### Anomaly 2 resolved: `Z3` was a real position, and we caused it

The CSV settles it. `alpha` really is in column 26 of row 1, `cherry` really is in
column 26 of row 3. The `Z` readings were **not** a phantom element reporting a
fake position — the data physically landed in column Z. Part 1's guess that a
stable runtime id implied a fixed sentinel element was wrong.

The cause is in the automation, not in Sheets. `terminator-rs`
`platforms/windows/element.rs:1163` — every `press_key` whose key contains
`ENTER` sends two keystrokes first:

```rust
if key_upper.contains("ENTER") || key_upper.contains("RETURN") {
    let _ = self.element.0.send_keys("{LEFT}", 10);
    let _ = self.element.0.send_keys("{END}", 10);
}
```

An inline-autocomplete workaround for browser address bars. In a spreadsheet,
**`{END}` moves the cursor toward the last column of the data region** — column Z
once anything is out there. So every `press_key("{Enter}")` in these probes was
silently relocating the cursor before committing.

Two consequences worth separating:

* **For this investigation:** every Sheets probe that pressed Enter had its cell
  navigation sabotaged. That is why intended cells were never reached, and it
  contaminates the attribution measurements below.
* **For the product:** `press_key` with Enter is not safe against grid targets in
  general. Worth checking wherever replay sends Enter, since the same two
  keystrokes would be injected into a user's spreadsheet.

### The prototype, and why it is not validated

The rule tested: while focus is on a ComboBox whose name parses as a cell
reference, remember `(name, text)`; when that stops holding, emit one action with
the last remembered pair, U+FEFF stripped.

Against ground truth it did not hold up consistently:

| run | navigation | editor reads matching the saved file |
|---|---|---|
| `sheetsedit` | arrow keys | **3/3** |
| `sheetswatch` | Name Box | **1/3** |

In the second run the Name Box navigation demonstrably failed — the intended
cells `B2`, `D5`, `C9` were never reached — and one reading came back as
`"\nbanana"`, a stray newline leaking in from the navigation keystrokes. So the
disagreement is confounded: it does not separate "the editor's name is an
unreliable attribution" from "the probe's own key driving was too chaotic for the
editor to be reporting anything stable".

That confound is the `{END}` defect above, now filed separately as
`press-key-enter-injects-end-keystroke.md`.

### Re-run without the confound: the mechanism works

`text_capture_probe -- sheetsclean` repeats the experiment with the corrupting
factor removed. Getting to a clean commit took two attempts, and the failed one
is worth recording because it looked like a fix:

* **`type_text("\n")`** injects nothing — but does not commit either. The editor
  stayed open and accumulated `"apple\nbanana\ncherry"` in one cell while the
  exported CSV stayed **empty**. It removed the confound by removing the thing
  being measured.
* **`press_key("{Tab}")`** commits a cell edit and contains neither `ENTER` nor
  `RETURN`, so it is sent verbatim. That is the clean commit.

With Tab, over three runs:

```
  typed      watcher cell watcher v  csv at cell  match
  apple      A1           apple      apple        true
  banana     B1           banana     banana       true
  cherry     C1           cherry     cherry       true

  watcher cell AND value confirmed by the saved file: 3/3
```

Nine of nine across three runs, checked by parsing the watcher's reported cell
reference into row and column and reading that exact position out of the exported
CSV — so both halves are verified, not just that the value appears somewhere.

**The mechanism is sound.** The transient editor reports the correct cell and the
correct value, and the earlier 1/3 result was the `{END}` defect moving the cursor
out from under the measurement, not the editor lying. Attribution via the editor's
`name` is no longer an open question.

What remains unproven is everything between this and a working feature: this is a
polling loop in a probe, not an event-driven capture path, and it was measured on
short single-line values typed by automation rather than on real human editing.

### What a fix would need, on current evidence

* **A new capture path.** The existing pipeline emits nothing for cell edits, so
  detection has to be added.
* **A commit-edge trigger.** The value must be taken from the last observation
  *before* the editor is recycled; one read too late yields U+FEFF at the wrong
  cell.
* **U+FEFF stripping**, which is cheap and already prototyped
  (`clean_cell_text`).
* **An Enter that does not send `{LEFT}{END}`**, or grid targets will keep moving
  under the automation.

## Finding 2 FIXED: grid cell capture is implemented (2026-08-11)

`capture/grid.rs` — `GridCellWatcher`, wired into the pump alongside
`TextFieldWatcher`. Capture went from **zero** `type` actions for Sheets cell
edits to **4 of 4**, each attributed to the correct cell with clean text,
confirmed against the document's own CSV export. Two runs, identical.

**Finding 1 (clipboard) is untouched and still open.**

### The three design questions, answered by measurement

**1. Is there an existing event that detects the editor?** No. `Click` never
names it, because the editor is created by *typing*. And `KeyboardEvent` carries
`metadata.ui_element: None` — measured across a driven Sheets session, **0 of 15
key-downs** carried an element. So there was nothing in the event stream to hook,
and capture had to gain its own focused-element resolution and a `Desktop`
handle. This is new machinery, not a tuned existing path, and the measurement is
why.

**2. Does it fit `TextFieldWatcher`?** No — and the reason is structural rather
than cosmetic. `combobox` is already in `TEXT_ROLES`, so the existing watcher
would happily accept the editor. But it **reads its element when the edit ends**,
and a cell editor does not survive that moment. A flush-time read lands after the
overlay is recycled and returns `U+FEFF` attributed to a different cell — which
is exactly the corruption the original recordings showed. The read model is
inverted, so this is a parallel watcher, not a merge: it samples while the editor
is alive and emits the **last good sample**. Everything downstream is shared —
same `ActionCandidate`, same `admit`, same exclusion gate.

**3. What signals "safe to read"?** Nothing does, so keystrokes are the clock.
The prototype polled at 25 ms; that is unnecessary in the real pipeline because
key-downs already arrive exactly when the value can change. Sampling on key-down
is event-driven, costs one focused-element resolution per printable key, and
guarantees a sample exists from *before* the commit keystroke. An edit ends on a
trigger key (Enter or Tab), on the editor reporting a different cell, or on
session stop.

### The gate caught it before the grid did

Worth recording, because the first integration run looked like total failure and
was nothing of the kind:

```
  1 action(s), 58 unmapped event(s)
  4 exclusion(s):
    "type" UnidentifiedSource
    "type" UnidentifiedSource
    "type" UnidentifiedSource
    "type" UnidentifiedSource
```

Zero captured actions — but **four exclusions**, one per cell edit. The watcher
had detected, sampled and emitted all four; `CapturedStream::admit` then dropped
them, because it fails closed on an action whose source app cannot be named and a
keyboard-only cell edit produces no `Click` to name it with.

The fix supplies the missing fact rather than weakening the gate: the watcher
already holds the resolved element, so it reads the owning application and window
off it — the same identification a click performs. Fail-closed is intact.

Printing exclusions alongside actions is what made this a five-minute diagnosis
instead of a hunt. "Produced nothing" and "produced things that were rejected" are
opposite diagnoses with opposite fixes, and the run output could not previously
tell them apart.

### Result, verified against the saved document

```
  5 action(s), 58 unmapped event(s)
  0 exclusion(s):
    navigate  role=Window     name="Untitled spreadsheet - Google Sheets …"
    type      role=ComboBox   name="A1" payload="apple"
    type      role=ComboBox   name="B1" payload="banana"
    type      role=ComboBox   name="C1" payload="cherry"
    type      role=ComboBox   name="D1" payload="date"

  row 1   "apple,banana,cherry,date"

  cell + payload confirmed by CSV : 4/4
  payloads containing U+FEFF      : 0
```

Four cells in sequence, so moving between cells is covered, not just a single
edit. The CSV is the arbiter: the cell each action names is indexed directly and
compared, so both halves are checked rather than just "the value appears
somewhere".

### Regression: existing capture is unchanged

This sits on the same pump as every other capture path, and runs a
focused-element resolution on every key-down in every application, so it was
checked rather than assumed.

| Check | Result |
|---|---|
| `replaycheck` — web `<input>` capture then replay | **PASS**, 4/4 steps, `type` still `role=edit`, no duplicate action |
| `multiline` — `<textarea>` across Enter | **PASS**, 2 type actions, replay reproduces exactly |
| `windowswitch` — attribution across an app switch | **PASS**, typing still ordered before the switch |
| `notepadgrid` — Notepad, `Document` role | **PASS** ×3, buffer reconstructed exactly, 0 grid actions |
| `cargo test` (94 unit + 23 integration) | pass, except the known pre-existing `ipc_pipeline` failure |
| `cargo clippy --all-targets` | 0 errors |

The important negative in the first three: **no spurious `type` action appeared**
anywhere. `sample()` returns `None` unless the focused element is a `ComboBox`
whose name parses as a cell reference, so every non-grid context exits after one
role check.

`ipc_pipeline::full_pipeline_over_ipc` still fails with the same assertion and the
same three actions as before this change, and reports `excluded_count: 0` — so
the grid path neither fixed nor worsened it. It tracks
`text-input-capture-truncation.md`.

#### Notepad: verified, on the fourth attempt

`text_capture_probe -- notepad` cannot answer this. It compares the focused
element's pid against the pid it launched, and Windows 11 Notepad hands new
launches to an existing instance, so the window belongs to a process it did not
spawn — the same pid instability the window-identity work measured. It is right
to refuse, and forcing it through would mean typing into a window it cannot
vouch for.

`-- notepadgrid` identifies the surface by its own properties instead: window
title contains "Notepad", a role `capture::text` accepts, and a buffer verified
**empty** so the probe never types into real work. It also *activates* the
window rather than assuming it took focus, which is why the original never
converged — focus was sitting on an unrelated `Button` throughout.

**Three independent runs, identical:**

```
  after activating "Untitled - Notepad": focus role="Document" window="" text_len=0
  anchored on "Untitled - Notepad", role="Document", verified empty

  4 action(s), 0 exclusion(s), 49 unmapped
    navigate  role=Window     name="Untitled - Notepad"
    click     role=document   name="Text editor"
    type      role=document   name="Text editor" payload="alpha line\r"
    type      role=document   name="Text editor" payload="beta line"

  concatenated payloads         : "alpha line\rbeta line"
  actually in the Notepad buffer: "alpha line\rbeta line"
  actions from the GRID path    : 0
```

Document-role capture is unchanged — two `type` actions still reconstruct the
buffer exactly, across an Enter — and the grid path contributed **no action and
no exclusion**. The role is still `document`, not `ComboBox`.

The property is also pinned as a unit test, `every_non_grid_surface_is_ignored`,
against `Document`, `Edit`, `Button`, and `ComboBox`es named `"Menus"` and
`"Zoom"` — which exist in Sheets' own window. The live run and the invariant
cover each other: one proves it against the real pump once, the other holds on
every build.

**A false alarm worth recording.** These runs were first reported as "did not
complete — four runs past timeout with no output", and a hang was diagnosed:
depth-50 traversal into a ~198 MB Notepad document. All of that was wrong. The
runs were **succeeding**; they were slow, and Rust block-buffers stdout when it
is piped to a file, so nothing was visible until the process exited. The bounded
version later measured the enumeration this was blamed on at **236 ms**.

The lesson is the one this investigation keeps relearning in new costume: *no
output* is not evidence of *no progress*. A probe whose output cannot be seen
until it exits cannot distinguish "stuck" from "still working", and the absence
got read as data — the same shape as the swallowed errors catalogued in
`replay-window-selector-ambiguity.md`.

**Still open: the cost.** The scripted activity in these runs is roughly 25
seconds, and they took several minutes. `GridCellWatcher` resolves the focused
element on every key-down, and this machine had seven Notepad windows open by the
end. That is a plausible cause and is *not* established — there is no
before/after comparison, because the grid path was already in the build for every
run. It does raise the per-keystroke cost item below from theoretical to
suspected.

## Replay of grid edits: implemented (2026-08-11)

`replay/mod.rs` — `grid_type`. A recorded Sheets cell edit now replays into a
**different, empty document** and lands in the right cell with the right value.
Verified against that document's CSV export, 3/3, twice.

### The problem it had to solve

Every other step shape is *resolve a selector, act on what it finds*. A grid cell
has nothing to resolve: the ComboBox editor is created **by** typing, so the
element that receives the text cannot also be the element located beforehand.

### Entry is through the Name Box, not coordinates

The obvious answer was coordinate clicking — and it is the fragile one, since
scroll position, zoom, frozen panes and window size all move a cell's pixels
while its reference stays the same.

It is not necessary. The **Name Box** — the reference field left of the formula
bar — is a real, persistent `Edit`, and Part 1 measured its text tracking the
cursor within 0–1 ms. Giving it a reference and pressing Enter moves the cursor,
which is how a keyboard user reaches a cell:

```
  via set_value          box reads "B2"
  cursor on B2: true
  editor during typing: role="ComboBox" name="B2"
  ...
  row 2   ",apple,,"
  row 5   ",,,banana"
  row 9   ",,cherry,"

  values landing in the INTENDED cell: 3/3
```

So replay stays **element-based**. No coordinates anywhere.

An earlier attempt at Name Box navigation (in `sheetswatch`) appeared to fail and
was written off; it had committed with `type_text("\n")`, which submits nothing,
so the mechanism was never actually on trial.

### Capture already records enough — no companion fix needed

The captured action carries `element_name` = the cell reference (`"A1"`), which
is exactly what Name Box entry consumes. Nothing extra had to be recorded: no
coordinates, no grid indices. The one addition on the replay side was parsing
`target.raw_role` out of the stored payload, which `compile` was already writing.

### Two details that only measurement would have found

**`type_text` appends; `set_value` replaces.** Typing a reference into the Name
Box produced `"A1"` → `"A1B2"` → `"A1B2D5"` — never a valid reference, so Enter
did nothing and the cursor never moved. The probe's own verification caught this
and refused to type, rather than filling whatever cell happened to be selected.

**The commit must go to the element focused *after* typing.** Sending `{Tab}` to
the element resolved before typing made `press_key` focus it first, which
abandoned the edit instead of committing it. The symptom was precise: **every
cell landed except the last one**, twice — because each pending edit was being
committed by the *next* step's Name Box navigation, and the final step has no
next step. Re-resolving focus before the commit took it from 2/3 to 3/3.

That failure is worth keeping in mind: replay reported all three steps
`executed`, and two thirds of the data arrived. A run that is *mostly* right is
exactly the shape this project keeps finding hardest to notice.

**Enter is still avoided on the grid.** The commit is `{Tab}`, because
`press_key` injects `{LEFT}{END}` before any Enter and `{END}` relocates the
cursor in a grid — see `press-key-enter-injects-end-keystroke.md`. Inside the
Name Box those same keys are harmless caret moves, which is why Enter is fine
there and not on the grid.

### Verified end to end

`text_capture_probe -- sheetsroundtrip` records real cell edits, compiles and
stores them, then replays into a **freshly created** document — deliberately not
the recorded one, where replay could pass by doing nothing:

```
  captured:  type role=ComboBox name="A1" payload="alpha"
             type role=ComboBox name="B1" payload="bravo"
             type role=ComboBox name="C1" payload="charlie"

  replay status: completed   [1] executed  [2] executed  [3] executed

  replay document CSV:  row 1  "alpha,bravo,charlie"

  cells reproduced correctly: 3/3
```

The export is rendered server-side, so it reports what Sheets **saved**, not what
is on screen. A 10 s sync wait was added before exporting after a first run
missed only the final cell — which looks identical to a failed last step. That
turned out not to be the cause, but the wait stays because the two are otherwise
indistinguishable.

### Regressions

| Check | Result |
|---|---|
| `cargo test` | 98 pass |
| `cargo clippy --all-targets` | 0 errors |
| `replaycheck` — ordinary web replay | **PASS**, 4/4 |
| `ordinary_steps_do_not_take_the_grid_path` | unit test: `Edit`, `Window`, `Document`, and a `ComboBox` named `"Menus"` all keep the normal path |
| `a_playbook_recorded_before_raw_role_existed…` | old payloads have no `raw_role`, so the grid branch cannot fire |

One `replaycheck` run failed first with `FAILED (selector is ambiguous)` on
`role:edit|name:FieldA`. That was **not** a regression: repeated probe runs had
left several copies of the probe page open, so the selector really did match more
than one element and tonight's ambiguity check refused it — correctly. With the
duplicates closed it passes 4/4. Worth recording as the first time that check
fired on something other than a scenario built to trigger it.

### Honest limits

* **Only Sheets, and only where the Name Box exists.** The mechanism depends on a
  persistent reference field. Excel Online and other grids are untested.
* **The cell reference is absolute.** A playbook recorded against `B2` writes to
  `B2` on replay regardless of what the sheet looks like. That is correct for
  fixed-layout data entry and wrong for anything positional; there is no notion
  of "the next empty row".
* **No verification that the value stuck.** `grid_type` confirms the cursor
  reached the cell before typing, but nothing afterwards re-reads the cell —
  because UIA cannot see committed cell values at all. Verification would need
  an export, which replay does not do.
* **Sheet-agnostic.** The reference carries no sheet name, so a multi-tab
  workbook replays into whichever sheet is active.

### The per-keystroke cost, measured (2026-08-11)

`GridCellWatcher` resolves the focused element on every printable key-down, in
every application, not just grids. That was flagged as *suspected* overhead after
the Notepad runs took minutes for ~25 seconds of scripted activity — on a machine
with seven Notepad windows open, with no user-facing symptom reported.

The A/B: the same driven A–E web trial suite, run twice, once with the grid
watcher compiled out (temporarily, via a `cfg` on its call in the pump).

| | `observe_key` calls | total | mean/keystroke | trial score | actions |
|---|---|---|---|---|---|
| grid watcher **in** | 105 | 233.1 ms | **2.22 ms** | 5/5 | 13 |
| grid watcher **out** | 0 | 0 ms | — | 5/5 | 13 |

**Confirmed non-issue.** 2.22 ms sits against human inter-keystroke intervals of
roughly 100–250 ms, so it is on the order of 1–2% of the gap between keys — and
the work happens in the async pump, which is not what the typist waits on.
Capture quality was byte-identical between the arms: same score, same action
count.

So the earlier suspicion was wrong, and the reason it looked plausible is worth
keeping: the Notepad slowness that prompted it was never attributed to anything.
It coincided with seven open Notepad windows, one holding a ~198 MB document, and
this measurement makes the grid watcher an unlikely explanation for minutes of
wall-clock.

Two honest limits. This is a **mean over 105 keystrokes**; no maximum was
captured, so a rare slow resolution would not show here. And it was measured in a
browser — the non-grid context the question was about — not on the loaded machine
where the original observation came from.

The counters that produced these numbers (`grid::timing`, two relaxed atomics)
are kept, so the question can be re-answered rather than re-argued.

### What is NOT fixed

* **Replay.** A captured Sheets edit records the cell and the text, and nothing
  makes it replayable — there is no element for a selector to resolve to, which
  is the whole finding of Part 1. Recording is now correct; reproducing is not
  solved.
* **Clipboard (Finding 1).** Untouched.
* **Anything but Sheets.** The cell-reference shape is the discriminator, so this
  works for grids that name their editor after the cell. Excel Online and other
  grids remain untested.
* **Cost.** One focused-element resolution per printable key-down, in every
  application — measured at 2.22 ms mean and judged a non-issue; see "The
  per-keystroke cost, measured". Still no maximum captured, and not measured on a
  heavily loaded machine.

### What this means for a fix

Not a fix, but the constraints any fix inherits:

* **There is no per-cell element to target, at rest.** Any design that assumes
  replay can locate "cell D3" by selector is unworkable as things stand. The
  only per-cell element that ever exists is the editor, and only while editing.
* **The Name Box is a usable, fast source of cell identity** — via `text()` on
  the child `Edit` of the `"Name box (Ctrl + J)"` group. It was the suspect;
  it turns out to be the most reliable signal measured here.
* **Sheets' screen-reader mode does not help.** Tested with the mode confirmed
  on: no grid roles, no cell-reference names, focus still static. The
  no-per-cell-element constraint is not something a setting can lift.

## Finding 1, re-measured (2026-08-11)

The brief asked for the design decision first and the fix second, and to confirm
what capture actually does with a paste before designing around the recorded
belief. Doing that in the other order would have designed for a problem half of
which no longer exists.

### What capture actually does now — two different answers

**Pasting into an ordinary text field is already captured.**
`text_capture_probe -- clipboardcheck` types into `FieldA`, copies, and pastes
into `FieldB`:

```
  FieldB actually contains: "clipsource42"
    type  role=edit  name="FieldA"  payload="clipsource42"
    type  role=edit  name="FieldB"  payload="clipsource42"
```

Not as a clipboard action — capture reads the destination's *value* directly, so
the data movement is recorded and replay can reproduce it by typing. This is a
side effect of the 2026-08-06 change that stopped trusting the recorder's
`TextInputCompleted`, and nobody re-checked Finding 1 afterwards. **The doc's
"clipboard operations produce no captured action at all" has been wrong for
ordinary fields for some time.**

**Pasting into a Sheets cell is still lost.** `-- sheetspaste` sets the clipboard
from outside the browser, pastes into a cell, and checks the exported CSV:

```
  saved document contains the pasted value: true
  capture recorded its content            : false
```

Confirmed in the context Finding 1 was originally observed in. The reason is
structural: a cell paste does not open the editor overlay, so the mechanism that
captures *typed* cell edits has nothing to sample, and there is no cell element
whose value could be read instead.

### The design question, and why clipboard content is not modelled

Capturing "user copied X, then pasted it into Y" as an action type is feasible
for a *single* cell — the Name Box gives the destination reference, and replay's
`grid_type` could reproduce it by typing. It was rejected anyway, on three
grounds:

1. **The realistic case is multi-cell and it would be silently wrong.** The
   session that produced Finding 1 was copying from Notepad into Sheets, which
   pastes a block across many cells. Recording that as one type action against
   the anchor cell reproduces something different from what the user did — and
   plausibly, which is the failure mode this project keeps finding hardest to
   catch.
2. **It puts clipboard contents into the store.** Capture is system-wide, so the
   clipboard may hold anything the user copied for unrelated reasons. That is a
   redaction-policy decision, not an implementation detail, and it is a
   meaningful privacy escalation to make as a side effect of a capture fix.
3. **A paste is not the same kind of thing as every other action here.** Every
   other action names an element it acted on. A paste is a data transfer whose
   source has no bearing on replay and whose destination may be a region rather
   than an element.

So the honest answer is option 2 from the brief: **do not capture clipboard
content, but stop losing the fact that a paste happened.**

### What was built

`CaptureReport::pastes_observed` counts `Ctrl+V` occurrences, surfaced through
`CaptureSummary` to the frontend. A recording with a non-zero count may be
missing data movement no action records. Deliberately a count and not a claim:
because field pastes *are* captured, a non-zero value means "check whether the
destinations were fields", not "data was definitely lost".

Also fixed, which the doc listed separately as wrong on its own terms:
**Ctrl-modified keys no longer count as typing.** `Ctrl+V` arrives as key code
`0x56` — plain `V` — so it was inflating keystroke totals, and since a field is
emitted when "the value changed OR keystrokes were seen", it could trigger an
action for a reason that never happened. `ctrl_pressed` was previously read
nowhere in `src/`.

### Verified

| Case | Paste landed | Content captured | `pastes_observed` |
|---|---|---|---|
| web `<input>` | yes | **yes**, as a `type` on the destination | 1 |
| Sheets cell | yes (CSV) | no | **1** |

The Sheets row is the point: the gap is unchanged, but it is now visible in the
capture summary instead of being indistinguishable from a session where nothing
happened. Re-running `clipboardcheck` after the Ctrl change confirmed no
regression — both fields still captured exactly.

## Not the same bug as the Gmail picker (tested 2026-08-09)

These findings and the Gmail recipient picker
(`dynamic-contact-picker-replay-fails.md`) were grouped as instances of one
problem — "dynamic JS-rendered widgets do not present stable accessibility
elements". **That was an inference from two data points, never tested. It has
now been tested directly and does not hold.**

Two general mechanisms were proposed and both were refuted on a controlled page
(`text_capture_probe -- widgets`):

* **ARIA roles map inconsistently to UIA roles** — refuted. `group`→`Group`,
  `combobox`→`ComboBox`, `gridcell`→`DataItem` and the rest are all the standard
  documented mappings. Which inverts the reading: a Sheets cell reporting
  `combobox` is capture *faithfully* recording what Sheets declares, not a
  translation fault.
* **Re-rendering invalidates element handles** — refuted. A node destroyed and
  recreated via `innerHTML` kept the same UIA runtime id (`282054`), and
  re-resolution found it.

With those gone, the two bugs stop resembling each other. Sheets is a
**capture-time misread**: the element is present, and the text or the cell
attribution is wrong. Gmail is a **replay-time not-found**: the element is
absent when needed, most likely because the picker is not rendered until compose
is open.

**Recommendation: treat this as an independent bug and fix it on its own terms.**
The next steps below are the right work; no unified "complex widget" strategy is
warranted, because the evidence for a common mechanism did not survive testing.

Full analysis, including the one structural property the two do share and why it
explains neither failure, is in `dynamic-contact-picker-replay-fails.md` under
"That question was tested".

## Why it matters

Spreadsheet data entry is close to the archetypal task this product automates:
repetitive, structured, and exactly what someone would want to record once and
replay. Both findings break that use case in different ways, and both fail
quietly:

* **A missing paste** means the playbook contains the navigation and the clicks
  but not the data movement. Replaying it reproduces the shape of the task while
  doing none of the work, and nothing in validation can detect the gap — a
  playbook with no `type` action is perfectly valid.
* **An empty or garbled payload** puts wrong data into a real spreadsheet on
  replay. Cell attribution drift is worse still: correct data written to the
  wrong cell is the kind of error that survives review, because every individual
  step looks plausible.

This is also a lesson about the test strategy rather than only about Sheets.
Every probe to date used simple `<input>` elements or Notepad, and every one
passed. One human session against a real target surfaced two problems
immediately. Probe coverage has been measuring the environment it was built for.

## Absolute cell references are the decision (2026-08-12)

**Settled, not still open.** A grid edit replays to the cell reference it was
recorded against: `B2` records as `B2` and replays as `B2`. This is the final
behaviour, and `grid_type`'s Name Box navigation already implements it.

**Why absolute is right for the primary use case.** The product automates
repetitive drafting and form-filling against fixed-layout documents — an invoice
template, a weekly report, a data-entry sheet where the same field lives in the
same place every time. In all of those, "write the total into `D14`" is exactly
what the user means, and it stays correct however many times it runs. Absolute is
not a limitation there; it is the requirement.

**Relative/offset replay is a future feature, not a defect.** Append-style entry
— "add a row at the bottom of whatever is there now" — is a genuine and different
need, and absolute references cannot express it. That is worth building later. It
is recorded here as a **feature to consider**, and deliberately not as a bug,
because the current behaviour is not wrong: it is one of two valid semantics, and
it is the one the primary use case needs.

What such a feature would have to settle, so the note is useful rather than
decorative:

* **How the recording expresses intent.** The capture carries a bare cell
  reference and nothing that distinguishes "this exact cell" from "the next free
  row". Intent cannot be inferred after the fact from `B2` alone, so either the
  user states it or the recorder infers it from context — and inference here
  would be guessing at the user's meaning, which this project has repeatedly
  found to be the expensive kind of wrong.
* **What an offset is relative to.** The last written row, the end of a data
  region, or a named anchor are three different answers that disagree the moment
  the sheet is not shaped the way the recording assumed.

Neither is urgent, and neither blocks Phase 2.

## The sheet-name gap (2026-08-12)

**This is a defect, and it is separate from the decision above.** Deciding
absolute references does not resolve it — it sharpens it. "Always write `B2`" is
only well-defined once you know *which sheet's* `B2`.

Nothing in the pipeline carries a sheet identity. `looks_like_cell_ref`
(`capture/grid.rs:86`) caps the column run at three letters *specifically so
`"Sheet1"` does not parse as a cell*, and the tests pin that
(`cell_references_are_recognised_and_other_names_are_not` rejects `"Sheet1"`).
So capture records `B2`, replay types `B2` into the Name Box, and the Name Box
resolves it **within whatever tab is active at that moment**. A playbook recorded
on `Sheet2` and replayed with `Sheet1` in front writes to the wrong sheet, and
every step reports success.

That is the silent-wrong-target failure class this project has now hit five times
— not a design choice, and not something to bundle with the relative-replay
feature above. Bundling them would let a real defect inherit a feature's priority.

**Why it is not fixed today, stated honestly.** The likely fix is cheap *if* one
assumption holds: Sheets' Name Box accepts a **qualified** reference
(`Sheet2!B2`), in which case replay needs no separate sheet-selection step and
`grid_type` changes by one string. That assumption is **untested**. Both halves
need measurement against live Google Sheets:

1. **Does the Name Box accept `Sheet2!B2`** and move the cursor across tabs?
   Verified the way the rest of this document verifies things — by CSV export,
   not by reading the UI that produced it.
2. **Can capture obtain the active sheet name** at edit time? The sheet tabs are
   in the accessibility tree, but which element reports the *selected* one, and
   whether it is readable at the moment the editor appears, has not been checked.

Guessing either would produce exactly the kind of confident, unverified claim
this file has already had to retract once. The measurement is a `sheetsedit`-style
probe run, which creates a real spreadsheet in the signed-in Drive account — a
deliberate act, not something to fold into a routine sweep.

**Until then, the exposure is bounded and worth stating:** single-sheet
workbooks, which is what every measurement in this document used, are unaffected.
The gap needs a multi-sheet workbook *and* a tab change between record and replay.

## Next steps

- [x] ~~**Decide how clipboard operations should be modelled at all.**~~
      **Decided: do not model clipboard content.** Multi-cell pastes would be
      recorded plausibly-but-wrongly, and storing clipboard contents is a
      redaction decision rather than a capture detail. Pastes are counted and
      surfaced instead. Original scope: Enabling
      `record_clipboard` is necessary but not sufficient: there is no `paste`
      value in the schema's `action_type` enum (`click`, `type`, `navigate`,
      `read`). Either a paste compiles down to a `type` carrying the pasted
      text — which puts clipboard contents into the store and therefore through
      the redaction policy — or the enum grows, which is a locked-schema
      migration. This is a design decision, not a code fix.
- [x] ~~**Stop treating `Ctrl`-modified keys as typing.**~~ Done --
      `ctrl_pressed` is now threaded into the watcher and Ctrl-modified keys no
      longer count as keystrokes. Original scope: Independent of the
      above, `is_typing_key` counting `Ctrl+V` as a literal `V` is wrong on its
      own terms. `KeyboardEvent` carries `ctrl_pressed`; `capture` currently
      ignores it.
- [x] ~~**Determine whether Finding 2 is the documented no-settle race or a new
      defect.**~~ **A new defect, and not one race but two separate causes.**
      Payload corruption is Sheets seeding its hidden editor with **U+FEFF**, not
      a value read too early. Attribution is not a settling problem either — the
      Name Box updates within 0–1 ms. See "The mechanism, measured".
- [x] ~~**Investigate cell attribution separately from payload corruption.**~~
      Done, and separating them was right: they have different causes entirely
      (U+FEFF seeding versus a transient editor element). Note the finding that
      reframes both — the `combobox` "cells" are the **cell editor overlay**,
      which exists only while a cell is being edited.
- [x] ~~**Test Sheets' screen-reader mode.**~~ **Tested; it changes nothing that
      matters.** With the mode confirmed on by Sheets' own announcement, the tree
      gains 3 nodes, none of them grid roles, and focus still never moves between
      cells. It swaps the hidden focus host from a 1×1 `Edit` to an offscreen
      `Group`, and that is all. See "Screen-reader support does not change the
      answer". The absence of per-cell elements is not a settings artifact.
- [x] ~~**Establish whether a committed cell value is readable at all.**~~
      **Commits work; UIA cannot see cell contents.** Verified against Sheets'
      CSV export — every typed value was present in the saved document. Replay
      cannot verify a grid write through the accessibility tree, but it can
      through an export.
- [x] ~~**Explain the `Z3` element.**~~ **Not a phantom — a real position we
      caused.** The CSV proves the data physically landed in column Z.
      `terminator-rs` sends `{LEFT}` then `{END}` before every Enter as a browser
      autocomplete workaround, and in a grid `{END}` jumps to the last column of
      the data region.
- [x] ~~**Re-run the editor-watcher prototype with a clean Enter.**~~ **Done —
      clean pass, 9/9 across three runs.** Committing with `{Tab}` instead of
      Enter removes the injected keystrokes; every value the watcher read landed
      in exactly the cell it named, verified by indexing the exported CSV at the
      parsed cell reference. The earlier 1/3 was the confound, not the mechanism.
- [x] ~~**Audit `press_key` with Enter across replay.**~~ Audited: `press_key`
      appears **zero** times in `src/`. Replay only calls `type_text`, which uses
      `send_text` and injects nothing, so the defect is latent rather than live.
      Filed as `press-key-enter-injects-end-keystroke.md`.
- [x] ~~**Add a capture path for the transient editor.**~~ **Done** —
      `capture/grid.rs`, wired into the pump. Zero → 4/4 cell edits captured with
      correct attribution and clean text, confirmed against the CSV export, two
      runs. See "Finding 2 FIXED".
- [x] ~~**Drive real Notepad typing through the pump.**~~ **Verified**, three
      identical runs: `type` actions still `role=document`, payloads reconstruct
      the buffer exactly across an Enter, and the grid path produced zero actions
      and zero exclusions. See "Notepad: verified".
- [x] ~~**Make a captured grid edit replayable.**~~ **Done** — `grid_type`
      reaches the cell through the Name Box, which is element-based rather than
      coordinate-based. Recorded edits replay into a fresh document, 3/3 by CSV,
      twice. See "Replay of grid edits: implemented".
- [x] ~~**Decide what a grid edit should mean on replay.**~~ **Decided
      2026-08-12: absolute, by design.** A playbook recorded against `B2` always
      writes `B2`, and that is the intended final behaviour — see "Absolute cell
      references are the decision". Relative/offset replay is a **future
      feature**, not a defect in this behaviour.
- [ ] **Carry the sheet name — reclassified as a defect, 2026-08-12.** A
      reference alone replays into whichever tab is active, so a multi-sheet
      workbook can be written to the wrong sheet with everything reporting
      success. Deliberately **not** deferred alongside relative replay: that is a
      choice between two valid behaviours, this is the silent-wrong-target
      failure class. Deferred because the fix is unmeasured, not because it is
      minor — see "The sheet-name gap" for the hypothesis and what must be
      measured.
- [x] ~~**Measure the per-keystroke cost.**~~ **Measured: 2.22 ms per keystroke,
      and a confirmed non-issue.** The suspicion did not survive an A/B — see
      "The per-keystroke cost, measured".
- [ ] **Confirm the accessor mismatch on the recorder side.** Cell identity lives
      in `text()` on the Name Box input and in `name` on the editor, never in
      `value`. Capture reads `e.element_text` for clicks and `watched.name` for
      types — different accessors on different elements, which would produce
      exactly the click/type disagreement observed. Read from the code, not yet
      instrumented against a live grid.
- [ ] **Test a second complex grid** — Excel Online, or any non-Sheets
      virtualized data grid — to establish how much of this is Sheets-specific
      versus general to grid UIs. Still worth doing, but note the scope has
      narrowed: the cross-application "widget category" theory was tested and
      refuted (see above), so this is now asking whether *grids* share a
      mechanism, not whether all dynamic widgets do.
- [x] ~~**Add a complex-grid target to routine probe coverage.**~~
      `text_capture_probe -- sheets` exists and runs against live Google Sheets.
      Note its cost: each run creates a blank "Untitled spreadsheet" in the
      signed-in Drive account and does not delete it, so it is a deliberate
      invocation rather than something to fold into a routine sweep.
