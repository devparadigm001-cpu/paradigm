# Click steps are stored with half-typed cell content as their selector

**Status:** open, real defect. Found 2026-08-18 from a live recording.
**Severity: MEDIUM-HIGH for replay.** The stored selector names a string that
existed only for a moment during typing, so the step can never match on replay —
or worse, matches something else. A second, larger part of the same recording
stores clicks with **no name at all**.
**Where:** the `Click` path through `capture::to_candidate` into
`compile`'s selector construction.

## What happens

Two things, both visible in one real recording of 37 steps.

### 1. Selectors built from a value mid-typing

While the user typed `Harbor Point Traders` into cell A2, two `Click` steps were
recorded whose targets are *prefixes of what was being typed*:

```
step 13  click  selector: role:text|name:﻿Harbor Point
step 14  click  selector: role:text|name:Harbor Point T
```

The first begins with `U+FEFF`. That is the marker of Sheets' hidden cell
editor — `clean_cell_text` exists precisely because *"Sheets seeds its hidden
editor with `U+FEFF`"* (`grid.rs`). The `Click` path never calls
`clean_cell_text`, so the character is carried into `element_name` and straight
into the stored selector.

Four more of the same shape appear later, from the dashboard side:

```
steps 27, 28, 29, 31  click  selector: role:text|name:12
```

### 2. Clicks stored with no name whatsoever

**17 of the 37 steps** — steps 2–10, 18–21 and 33–36 — are stored as:

```json
{"name":null,"raw_role":"group","selector":"role:group"}
```

A bare `role:group` selector matches the first `group` element in the window.
That is 46% of the recording.

## Why the first one cannot work on replay

`role:text|name:﻿Harbor Point` describes a state that existed for a fraction of
a second while a user typed. On replay:

* the cell is empty at that point, so nothing carries that name;
* even if something did, the `U+FEFF` prefix must match byte-for-byte;
* and if the selector *does* match something, it matched by coincidence.

The failure is not "click the wrong thing occasionally". It is a step that is
structurally incapable of matching what it was recorded from.

## Why the second one is arguably worse

`role:group` is not wrong, it is *unconstrained*. It will match — the first
group in the window, whatever that is. A step that always matches something and
never the right thing fails silently, which is the failure mode this project
keeps finding. And at 17 of 37 steps it is not an edge case in this recording,
it is most of it.

This is the same family as `selector-matching-precision.md` and
`replay-window-selector-ambiguity.md`, but those describe *ambiguity between
candidates*. This is a selector with no distinguishing content at all.

## What is not established

**Whether these clicks correspond to real user clicks.** The recording was a
diagnostic session and the user's recollection was that they clicked dashboard
text; the stored names say the target resolved to Sheets editor content instead.
Both cannot be true, and which one is wrong is not settled.

One hypothesis was **ruled out** by measurement: that these were synthesised
from keystrokes. The recorder does manufacture `Click` events from Enter and
Space (`windows/mod.rs:1098` → `handle_activation_key_press_request` →
`:3155`), and `"Harbor Point Traders"` contains exactly two spaces, which
matched the two events suspiciously well. But that path is gated on the focused
element's role containing one of `button`/`menuitem`/`listitem`/`hyperlink`/
`link`/`checkbox`/`radiobutton`/`togglebutton` (`:3091-3098`), and the stored
role is `text`, which matches none. **These came from a real mouse-click path.**

So the open question is narrower: why did a real click resolve to an element
reporting a half-typed cell value? The likely direction is read latency — the
click's element is resolved asynchronously with a timeout, so a slow read can
return the state of a *later* moment. That is the same shape as
`type-action-misattributed-after-window-switch.md`, and it is a hypothesis, not
a finding.

## What a fix must not do

**It must not strip `U+FEFF` and call it fixed.** Cleaning the name makes the
selector *look* reasonable while still naming a transient value. The
`U+FEFF` is a symptom that identified the source; removing it removes the
evidence, not the defect.

**It must not fall back to coordinates when the name is empty.** Clicking by
position is the failure mode `terminator-multi-monitor-visibility.md` already
records.

**It must not drop unnamed clicks silently.** 17 of 37 steps here are unnamed;
discarding them would quietly delete most of a recording. If such a step cannot
be replayed safely, the recording should say so loudly — the project's
fail-loud-never-guess rule applies.

## Reproducing

1. Start Record Mode, open a Google Sheet, click a cell and type a multi-word
   value while the recording runs.
2. Stop, save the playbook, and
   `cargo run --example dump_playbook -- <id> %APPDATA%\com.amitj.paradigm`.
3. Read the `selector` field of the `click` steps. The review screen shows the
   element name but not the stored selector, so the dump is required.

## Related

* `docs/known-issues/sheets-cell-edits-are-captured-by-both-watchers.md` and
  `docs/known-issues/a-grid-edit-restarts-and-re-emits-the-same-value.md` —
  found in the same recording, and the note there about `detail` never crossing
  the IPC boundary applies to this investigation too.
* `docs/known-issues/selector-matching-precision.md`
* `docs/known-issues/replay-window-selector-ambiguity.md`
* `docs/known-issues/type-action-misattributed-after-window-switch.md`
