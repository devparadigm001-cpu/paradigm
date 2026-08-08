# Multi-line documents capture cumulative text, so replay duplicates it

**Status:** confirmed by probe; root cause understood. Not fixed.
**Affected:** `src-tauri/src/capture/text.rs` — `TextFieldWatcher::flush`, on
any `Document`-role (multi-line) editing surface. Single-line `Edit` fields are
unaffected.
**Found:** 2026-08-07, immediately after Notepad capture began working at all
(see "Relationship to the Document-role fix").
**Severity: MEDIUM.** Capture is no longer silently empty for these surfaces —
the text is recorded and correct. What is wrong is that successive actions each
carry the *whole* document rather than what was added, so replaying a
multi-line recording writes duplicated content into a real file.

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

- [ ] **Confirm the replay behaviour empirically.** The duplication is inferred
      from stored payloads, not observed. Record a two-line Notepad session,
      replay it, and compare the resulting file against the original.
- [ ] **Decide what a multi-line action should carry — this is the real
      design question.** Options, none obviously right:
      *the delta since the last flush* (replay appends; needs the delta to be
      well-defined when the user edits in the middle rather than only at the
      end); *the whole document with replace-semantics on replay* (requires
      replay to clear the surface first, which is destructive if the recording
      and the replay target start from different contents); or *one action per
      document, emitted only at the end* (simple, but loses the intermediate
      structure that makes a playbook reviewable).
- [ ] **Decide whether Enter should remain a trigger key for multi-line
      surfaces.** In an `<input>`, Enter means "done". In a document it means
      "new line" and is not a completion signal at all — which is why this
      recording split into two actions rather than one in the first place.
- [ ] **Check the same behaviour on other multi-line surfaces** — a browser
      `<textarea>`, WordPad, VS Code — to establish whether "Document role"
      is the right predicate for this handling or merely the one Notepad
      happens to report.
