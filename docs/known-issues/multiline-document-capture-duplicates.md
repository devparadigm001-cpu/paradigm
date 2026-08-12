# Multi-line documents capture cumulative text, so replay duplicates it

**Status: FIXED for the append case** (2026-08-08), and **verified on a real
`Document`-role surface** (2026-08-11 — see "Document-role verification").
One caveat remains and is stated in "What is not fixed": mid-edit scenarios
still emit the full value.
**Affected:** `src-tauri/src/capture/text.rs` — `TextFieldWatcher::flush`.
**Not** limited to `Document`-role surfaces, contrary to this document's first
version: a web `<textarea>` reports role `"Edit"`, identical to a single-line
`<input>`, and duplicated exactly the same way.
**Found:** 2026-08-07, immediately after Notepad capture began working at all
(see "Relationship to the Document-role fix").
**Confirmed in real use:** 2026-08-08, a live user session replayed a recording
that typed "Weekly summary draft" **twice**. Until then the duplication was
inferred from stored payloads; it is now observed end to end.
**Severity: was MEDIUM.** Capture was never silently empty for these surfaces —
the text was recorded and correct. What was wrong is that successive actions
each carried the *whole* document rather than what was added, so replaying a
multi-line recording wrote duplicated content into a real file.

## Summary

`flush()` emits the field's entire current text. For a single-line `<input>`
that is exactly right: the whole value *is* the value the user intends, and
replay setting it wholesale reproduces the result. For a multi-line document
every flush re-reads the entire document, so the second action contains the
first action's text as well as the new text.

The emit model has no concept of "what changed since the last flush". It was
designed against single-value fields, where that concept is unnecessary.

## Evidence

`cargo run --example text_capture_probe -- notepad`, driving the Step 12 human
sequence — click once, type a line, press Enter, type a second line, stop:

```
captured 4 action(s), 161 unmapped event(s)
  navigate  role=Window       name="Untitled - Notepad"
  click     role=document     name="Text editor"
  type      role=document     name="Text editor"
            payload="first line typed by the notepad probe"
  type      role=document     name="Text editor"
            payload="first line typed by the notepad probe\rsecond line typed by the notepad probe"
```

The first `type` is correct. The second carries **both** lines, not just the
second one.

### Why two actions, and why the second accumulates

Enter (`0x0D`) is a trigger key. `key_pressed` flushes on it and then re-watches
the same element with a fresh baseline, so that continued typing becomes a
second action rather than being lost:

* **flush 1**, on Enter — current text is line 1, baseline was empty, so it
  emits `"first line…"`. Correct.
* **re-watch** — baseline is re-read as `"first line…"`.
* **flush 2**, at session end — current text is now
  `"first line…\rsecond line…"`. That differs from the baseline, so an action is
  emitted carrying **the whole document**.

The comparison against the baseline correctly decides *whether* to emit. It has
no bearing on *what* is emitted, which is always the complete value:

```rust
let changed = current != watched.initial;
let typed_into = watched.keystrokes > 0;
if !changed && !typed_into {
    return None;
}
// ... payload: Some(current)   <- the entire field, not the delta
```

### What replay would do

Compiled to a playbook, those two steps type `"first line…"`, then type
`"first line…\rsecond line…"` into the same surface. Because typing into a
document appends at the caret rather than replacing the contents — unlike
setting an `<input>`'s value — the result is line 1 followed by line 1 again
and then line 2. The recording looks plausible at every individual step, which
is the same silent-and-self-consistent failure shape as the original truncation
defect.

This has not been confirmed by an actual replay run; it is inferred from the
stored payloads and from how `replay` types text. Confirming it is the first
task below.

## Relationship to the Document-role fix

**This is a design gap in the emit model, not a defect in the role-recognition
fix.** Adding `document` to `TEXT_ROLES` is what made these surfaces capture
anything at all — before it, the same session produced zero `type` actions and
the accumulation was invisible because nothing was emitted. The fix is a strict
improvement and is correct on its own terms; it exposed a limitation that was
always latent in `flush`, and would have appeared for any multi-line surface the
watcher was ever pointed at.

The web A–E trials pass 5/5 across three runs with exactly one correct action
per field, confirming single-line behaviour is unaffected.

## The fix (2026-08-08)

`flush` now records **what was added** since watching began, rather than the
whole field.

### Why not "stop flushing on Enter for multi-line fields"

That was the leading hypothesis and it cannot be implemented: **there is no
signal for "this control is multi-line".** A `<textarea>` reports role `"Edit"`,
byte-identical to a single-line `<input>` (measured), and terminator exposes no
multiline property — no accessor, and the attribute bag carries `AutomationId`
only. Keying the fix on `role == "document"` would have fixed Notepad and left
every web textarea duplicating, which is the same defect with less visibility.

A behavioural substitute — on Enter, check whether a newline appeared — was
rejected as racy: the keyboard event can reach us before the app has processed
the key, so the first Enter in a fresh field is a coin flip.

Emitting the delta needs no such distinction, which is why it was chosen.

### The correctness argument

Replay types each payload at the caret **without clearing** — `type_text` with
`use_clipboard: false` routes to `send_text`, key by key. So concatenating a
field's payloads is exactly what a replay writes. Cumulative payloads therefore
*must* duplicate; deltas compose.

| Flush | Baseline | Field now | Emitted before | Emitted now |
|---|---|---|---|---|
| 1 (Enter) | `""` | `alpha line` | `alpha line` | `alpha line` |
| 2 (exit) | `alpha line` | `alpha line\nbeta line` | `alpha line\nbeta line` | `\nbeta line` |

### A regression this caused, caught by the A–E suite

The first implementation subtracted the baseline **unconditionally**, and
regressed every settled trial from 5/5 to **0/5 across three runs**. Payloads
came back as `"0123456789abcdefghi"` — the leading letter eaten.

The cause is that `initial` is read when the *click event is processed*, which
lands roughly one character after typing begins even with a 400 ms settle.
Subtracting that baseline silently removes the characters that arrived before
it. That trades visible duplication for invisible truncation, which is strictly
worse: too much text is obvious, missing text is not.

The repair is that a delta may only be subtracted from a baseline we can trust:

* set by a **click** — read at click-processing time, possibly late →
  **untrusted**, emit the whole value, exactly as before this change;
* set by our **own re-watch immediately after a flush** — no gap for characters
  to slip into → **trusted**, emit the delta.

Duplication only ever arises from the second, so this fixes it without touching
the path every single-flush field depends on. The distinction is carried by
`Watched::baseline_trusted`.

### Verification

| Check | Result |
|---|---|
| Duplication reproduced, unfixed (`<textarea>`) | `"alpha linealpha line\nbeta line"` (30 chars) vs 20 in field — **fail, as expected** |
| Same scenario, fixed | `"alpha line\nbeta line"` — **replay matches exactly** |
| A–E regression, 3 runs | **A–D 12/12**; E 1/3 (pre-existing no-settle race, measured 4/10 before this work) |
| Pre-filled field, 3 runs | no fabricated action |
| Exit path: click-away | 2 actions, replay matches exactly |
| Exit path: Tab | 2 actions, replay matches exactly |
| Full suite | 76 lib, 8 db_encryption, 12 ipc_commands, 2 replay_aborted pass; `ipc_pipeline` red on the separate no-settle race |

## Document-role verification (2026-08-11)

Verified. The evidence is the `notepadgrid` run recorded in
`text-input-capture-truncation.md` § "Notepad verification (2026-08-11)", three
identical runs against real Notepad:

```
  anchored on "paradigm-probe-….txt - Notepad", role="Document", verified empty

    type  role=document  name="Text editor"  payload="alpha line\r"
    type  role=document  name="Text editor"  payload="beta line\r"
    type  role=document  name="Text editor"  payload="gammaburst"

  concatenated payloads         : "alpha line\rbeta line\rgammaburst"
  actually in the Notepad buffer: "alpha line\rbeta line\rgammaburst"
```

That run was commissioned for a *different* fix — the no-settle keystroke-focus
race — so the question is whether it incidentally covers this one. It does, and
not by coincidence: it drives **this document's own Evidence sequence**.

* **The surface is right.** `role="Document"`, the exact path this item names,
  anchored on a window whose emptiness was verified before typing.
* **The duplication mechanism was exercised, twice.** Duplication arises from
  flush-then-re-watch on a trigger key (see "Why two actions, and why the second
  accumulates"). The probe presses **Enter twice**
  (`text_capture_probe.rs:6690`, `:6702`), producing three flushes — so the
  re-watch path ran on both, and `baseline_trusted: true` (`capture/text.rs:365`)
  is reachable only from that path.
* **The payloads are visibly deltas, which is the fix's signature.** Pre-fix,
  flush 2 would have carried `"alpha line\rbeta line"` and flush 3 the whole
  document. Observed instead: `"beta line\r"` and `"gammaburst"`. This is the
  distinguishing observation — a pure timing fix would not change *what* a flush
  emits.
* **The assertion is the duplication assertion.** The probe computes
  `concat(payloads) == buffer` (`text_capture_probe.rs:6743-6747`, `:6791`).
  By this document's own correctness argument — replay types each payload at the
  caret without clearing, so concatenating payloads is exactly what a replay
  writes — that comparison *is* the replay-equivalence check. Cumulative payloads
  cannot satisfy it; they are strictly longer, which is precisely the 30-vs-20
  failure shape the `<textarea>` reproduced before the fix.

**Scope, stated honestly.** This closes the append case on a `Document` surface,
which is exactly the scope of what was fixed. It does not exercise mid-edit,
still unfixed below. And it establishes replay-equivalence by concatenation
rather than by a live replay into Notepad; the live-replay leg was run on the
`<textarea>` ("replay matches exactly", in the fix's verification table). No
re-run was needed to close this item.

## What a multi-line action carries: settled (2026-08-12)

**The delta since the last flush.** Shipped, tested, and verified on both a web
`<textarea>` and a real `Document`-role surface. This is the final decision.

The three options this document listed were not equally live. **Replay has no
clearing mechanism**, and that single fact eliminates two of them.

`replay::type_text` calls `element.type_text(text, false)`. With
`use_clipboard: false` that routes to the library's `send_text`, which sends the
string key by key **at the caret, without clearing the surface first**. So what a
replay writes into a document is the concatenation of the payloads it types.
Nothing in the replay path can replace a document's contents, and nothing was
built to.

That makes the decision an identity rather than a preference:

* **Delta since the last flush — chosen.** Payloads concatenate to exactly the
  recorded document, because each carries only what was added since the previous
  flush. Verified by concatenation against the live buffer on Notepad
  (`"alpha line\r"` + `"beta line\r"` + `"gammaburst"` = the buffer, three
  identical runs) and by a real replay on a `<textarea>`.
* **Whole document with replace-semantics — rejected.** It requires replay to
  clear the surface before typing, which does not exist. Building it would mean
  destroying content in the replay target, which is unsafe whenever the target
  starts from different contents than the recording did — and a document
  automation tool cannot assume it starts from empty. Cumulative payloads without
  clearing are precisely the duplication bug this file documents.
* **One action per document, emitted at the end — rejected.** It would compose
  correctly, since one payload trivially concatenates to itself. It was rejected
  for what it costs elsewhere: a playbook becomes a single opaque blob, losing
  the per-step structure that makes it reviewable and editable before replay,
  which is a core property of the product. It also cannot represent a document
  the user edits, leaves, and returns to.

Note what this decision does **not** cover: a delta is only well-defined for an
**append**. Mid-edit still emits the full value, which remains open below and is
unaffected by settling this question.

## What is not fixed

**Mid-edit still emits the full value.** A delta is only well-defined for an
append. Editing in the middle, deleting, or replacing a selection falls back to
emitting the whole field — which is precisely the pre-fix behaviour, so nothing
regressed, but nothing improved either. A recording that edits into the middle
of an existing paragraph and is flushed twice can still duplicate.

~~**The `Document` path was not re-tested this session.**~~ **Resolved
2026-08-11 — see "Document-role verification" below.** The obstacle recorded
here was real: Windows 11 hands a fresh `notepad.exe` launch off to an
already-running instance — measured directly, focus landed on pid 18552 while
the probe had launched 9596 — so Notepad could not be targeted without risking
typing into a window the probe did not own. An earlier attempt demonstrated the
risk concretely: an unscoped `role:Document` search matched a Spotify tab in the
user's browser and typed probe text into it.

That obstacle was solved rather than worked around: launch `notepad.exe` with a
**uniquely-named empty file**, so the window title is unambiguous *and* the
buffer is empty by construction rather than by hope, then refuse to type unless
the anchored surface is a Notepad window with a verified-empty body
(`text_capture_probe.rs:6640-6660`).

## Why it matters

Notepad is the simplest possible multi-line target, and the one a first-time
user is most likely to try. Anything document-shaped — a text editor, a code
editor, a rich-text composer, an email body — has the same structure, and these
are ordinary targets for the kind of repetitive drafting this product automates.

The failure is quiet in the way that matters most: every captured payload is
*correct text*, correctly attributed to the right element. Nothing in validation
can flag it, because a playbook whose second step contains its first step's text
is perfectly valid. The error only becomes visible when a replay writes doubled
content into a real document.

## Next steps

- [x] **Confirm the replay behaviour empirically.** Done — a live user session
      on 2026-08-08 replayed a recording that typed "Weekly summary draft"
      twice, and the probe reproduced it on a `<textarea>` before the fix.
- [x] **Verify the fix against a `Document`-role surface.** Done 2026-08-11 —
      three identical `notepadgrid` runs against real Notepad, `role="Document"`,
      two Enters, payloads emitted as deltas and concatenating to the buffer
      exactly. See "Document-role verification" above. Covered by a run
      commissioned for the no-settle race fix, which drives this document's own
      Evidence sequence; no separate run was needed.
- [ ] **Decide whether mid-edit deserves a real answer**, or whether
      full-value-on-mid-edit is acceptable indefinitely.
- [x] ~~**Decide what a multi-line action should carry — this is the real
      design question.**~~ **Decided 2026-08-12: the delta since the last
      flush**, which is the shipped and tested behaviour. It is not one of three
      live options — it is the only one consistent with how replay actually
      works. See "What a multi-line action carries: settled".
- [ ] **Decide whether Enter should remain a trigger key for multi-line
      surfaces.** In an `<input>`, Enter means "done". In a document it means
      "new line" and is not a completion signal at all — which is why this
      recording split into two actions rather than one in the first place.
- [ ] **Check the same behaviour on other multi-line surfaces** — WordPad, VS
      Code — to establish whether "Document role" is the right predicate for this
      handling or merely the one Notepad happens to report. The `<textarea>`
      (role `"Edit"`) and Notepad (role `"Document"`) legs are both now measured,
      which is already evidence *against* role being the right predicate: the
      same delta logic is correct on both, keyed on `baseline_trusted` rather
      than on role. That is the intended design — see "Why not 'stop flushing on
      Enter for multi-line fields'" — so this item is now about confirming the
      generalisation, not about choosing a predicate.
