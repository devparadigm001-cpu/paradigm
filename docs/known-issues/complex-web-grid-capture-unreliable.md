# Capture is unreliable against complex web grids (Google Sheets)

**Status:** two distinct problems, both **confirmed against real recordings**.
The clipboard cause is established from the code. **The grid mechanism is now
measured against live Google Sheets** — see "The mechanism, measured". Of the two
standing hypotheses, one is confirmed, one is refuted, and two symptoms that
neither predicted are now explained. **Still not fixed.**
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

### What this means for a fix

Not a fix, but the constraints any fix inherits:

* **There is no per-cell element to target, at rest.** Any design that assumes
  replay can locate "cell D3" by selector is unworkable as things stand. The
  only per-cell element that ever exists is the editor, and only while editing.
* **The Name Box is a usable, fast source of cell identity** — via `text()` on
  the child `Edit` of the `"Name box (Ctrl + J)"` group. It was the suspect;
  it turns out to be the most reliable signal measured here.
* **Sheets has a screen-reader mode** that is off by default and was not enabled
  for these runs. Whether it materialises a real cell tree is untested and is
  the obvious next question, because it would change every constraint above.

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
- [ ] **Test Sheets' screen-reader mode.** Off by default, not enabled for the
      measured runs, and the single change most likely to alter every constraint
      above: if it materialises a real per-cell accessibility tree, targeting
      cells becomes possible and the fix looks completely different. Ask this
      before designing anything.
- [ ] **Establish whether a committed cell value is readable at all.** The probe
      could not verify the typed text after Enter, and could not distinguish "the
      commit did not happen" from "committed values are invisible to UIA". That
      distinction decides whether replay can ever verify what it wrote into a
      grid.
- [ ] **Explain the `Z3` element (runtime id `461295`).** Reported as the cell
      position after commit instead of the expected `D4`, identically across two
      runs and two documents. Reproducible, so not noise, and currently not
      understood.
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
