# Capture is unreliable against complex web grids (Google Sheets)

**Status:** two distinct problems.
**Finding 2 (cell capture) is FIXED, 2026-08-11** — `capture/grid.rs` captures
cell edits with correct attribution and clean text, verified against the
document's own CSV export. See "Finding 2 FIXED". Replay of those edits is *not*
solved.
**Finding 1 (clipboard) remains open.** Its cause is established from the code;
no fix attempted.
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

#### The Notepad check: attempted properly, and still not measured

This was pursued rather than left as an assumption, and it did not come off. The
attempt is recorded because the reasons are specific and someone will try again.

`text_capture_probe -- notepad` aborts on its own precondition: it compares the
focused element's pid against the pid it launched, and Windows 11 Notepad hands
new launches to an existing instance, so the window belongs to a process the
probe did not spawn. That is the same pid instability the window-identity work
measured. The precondition is right to refuse, and forcing it through would mean
typing into a window the probe cannot vouch for.

`-- notepadgrid` was written to avoid that: it identifies the surface by its own
properties instead of a pid — window title contains "Notepad", role accepted by
`capture::text`, buffer verified **empty** — and activates the window rather than
hoping it takes focus, which is why the original never converged (focus was on an
unrelated `Button` throughout).

It did not complete either. **Four runs, each past its timeout with no output at
all.** Narrowing, in order:

* the window sweep used the default depth 50, which descends into every window's
  full subtree — including a Notepad holding a ~198 MB document. Reduced to
  depth 3, which is sufficient for top-level windows.
* the emptiness check called `text(0)`, the read that is expensive on a large
  buffer. Reordered so the window title screens first and that read only ever
  happens on a fresh `Untitled - Notepad`.
* every UIA call was then time-bounded, so a hang would report itself.

It still produced nothing, which places the stall before any bounded call. Each
attempt also launches a Notepad it never closes, so UIA had six instances to
traverse by the end — the environment degraded as the attempts continued, which
is the most likely reason they got worse rather than better.

**What replaced it.** The property the Notepad run would have checked is now a
unit test rather than an argument. `is_cell_editor(role, name)` is the single
decision that keeps this watcher out of every non-grid application, and
`every_non_grid_surface_is_ignored` pins it against `Document` (Notepad's role,
established by an earlier probe measurement and the reason `"document"` is in
`TEXT_ROLES`), `Edit`, `Button`, and `ComboBox`es named `"Menus"` and `"Zoom"` —
which exist in Sheets' own window.

That is a stronger guarantee than one live Notepad run, because it holds on every
build. It is **not** the same thing as having driven real Notepad typing through
the real pump, and that remains open.

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
  application. Not measured under load.

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

## Next steps

- [ ] **Decide how clipboard operations should be modelled at all.** Enabling
      `record_clipboard` is necessary but not sufficient: there is no `paste`
      value in the schema's `action_type` enum (`click`, `type`, `navigate`,
      `read`). Either a paste compiles down to a `type` carrying the pasted
      text — which puts clipboard contents into the store and therefore through
      the redaction policy — or the enum grows, which is a locked-schema
      migration. This is a design decision, not a code fix.
- [ ] **Stop treating `Ctrl`-modified keys as typing.** Independent of the
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
- [ ] **Drive real Notepad typing through the pump.** Still the one regression
      not measured live. Two probe modes and four runs did not complete — see
      "The Notepad check". The property is now pinned by
      `every_non_grid_surface_is_ignored`, which is stronger in that it holds on
      every build, but it is a unit test and not a live run. Retry on a machine
      with no large Notepad document open, and close each launched instance
      rather than letting them accumulate.
- [ ] **Make a captured grid edit replayable.** Capture is now correct; replay is
      not solved and is not close. There is no cell element for a selector to
      resolve to, so this needs a different addressing mechanism entirely —
      plausibly driving the Name Box, which was measured tracking the cursor
      within 0–1 ms.
- [ ] **Measure the per-keystroke cost.** One focused-element resolution per
      printable key-down, in every application, not just grids. Cheap in
      principle — it exits after one role check — but unmeasured under load.
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
