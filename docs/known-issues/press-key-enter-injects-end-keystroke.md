# `press_key` with Enter injects a hidden `{END}`, which moves the cursor in a grid

**Status:** confirmed in the library source and reproduced against live Google
Sheets, with the resulting misplacement verified in the saved document.
**Latent in Paradigm, not live** — see "Is this shipping today?", which corrects
the assumption this doc was opened with.
**Affected:** `terminator-rs` 0.23.35,
`platforms/windows/element.rs:1160-1169`. Any caller of
`UIElement::press_key` with a key naming Enter or Return, against a target where
`End` means something.
**Platform:** Windows. Demonstrated in Google Sheets; the mechanism is not
Sheets-specific.
**Found:** 2026-08-10, while prototyping Sheets cell capture. It was corrupting
the measurements of that investigation, which is how it surfaced.
**Severity: HIGH if ever reached, and it is currently not reached.** The failure
mode is a silent write to the wrong cell. The reason it is not a live incident is
narrow and could be erased by a one-line change — see below.

## The defect

Every `press_key` whose key string contains `ENTER` or `RETURN` sends two other
keystrokes first:

```rust
// Dismiss inline autocomplete before pressing Enter/Return
// This prevents unwanted autocomplete suggestions (e.g., Chrome address bar) from being accepted
let key_upper = key.to_uppercase();
if key_upper.contains("ENTER") || key_upper.contains("RETURN") {
    // Press left arrow to dismiss any inline autocomplete suggestion
    let _ = self.element.0.send_keys("{LEFT}", 10);
    // Press End to return cursor to end of text
    let _ = self.element.0.send_keys("{END}", 10);
}
```

The intent is reasonable and the comment says so plainly: it is a workaround for
inline autocomplete in a browser address bar, where `Left` then `End` dismisses
the suggestion and restores the caret. In a single-line text field that is
invisible and harmless.

**In a grid it is neither.** `End` is a navigation key there. In Google Sheets it
moves the cursor toward the last column of the data region. So the sequence is
not "commit this cell" but "move somewhere else, then commit".

There is no way to opt out through the public API. The condition is a substring
test on the key name, and the only two spellings the underlying crate accepts for
that key — `ENTER` and `RETURN` — both match. Passing the key by any other name
does not reach `VK_RETURN`.

## Evidence

Reproduced with `text_capture_probe -- sheetsedit`, typing values into Google
Sheets and committing with `press_key("{Enter}")`. Ground truth is Sheets' own
CSV export, not the UI that produced the reading:

```
  "111,,,,,,,,,,,,,,,,,,,,,,,,,alpha"
  ",,,,,,,,,,,,,,,,,,,,,,,,,betagamma"
```

`111` is in `A1`, where it was typed. `alpha` is in **column 26** — `Z1` — and
`betagamma` in `Z2`. Twenty-five empty columns of drift, in a document where the
only navigation performed between edits was a commit.

This was initially mistaken for a phantom accessibility element reporting a fake
position, because the element carried a stable runtime id across documents. The
CSV settles it: the data physically went to column Z. The position was real and
the automation put it there.

The control is `text_capture_probe -- sheetsclean`, identical except that it
commits with `{Tab}` — which commits a cell edit and contains neither `ENTER` nor
`RETURN`, so it is sent verbatim:

```
    row 1   "apple,banana,cherry"
```

`A1`, `B1`, `C1`. No drift, three runs, identical. The only difference between
the two experiments is which key committed the edit.

## Is this shipping today?

**No, and the reason is worth stating precisely rather than assuming either way.**

`grep -rn "press_key" src/` returns **zero occurrences**. Paradigm's replay never
calls it. The only place replay sends text or keys is `replay/mod.rs:851`:

```rust
element.type_text(text, false)
```

and `type_text` takes an entirely different path in the library — it calls
`send_text`, which adds no preamble:

```rust
self.element.0.send_text(text, 10)
```

So a recorded `type` action carrying a newline types a newline. Nothing injects
`{END}`.

The exposure is therefore **latent**: the defect is real and reachable, but no
shipped code path reaches it. Everything that hit it in this investigation was
probe code.

This was checked because the opposite was assumed. The framing that opened this
work was that the bug "affects any existing playbook that types into a grid and
presses Enter today". It does not, because replay does not press Enter — it types
text. Recorded plainly here so the severity is not inherited from a premise
nobody verified.

## Why it still matters

Three reasons this is worth a document rather than a footnote:

1. **One plausible change makes it live.** Any future work that reaches for
   `press_key` to commit a field, submit a form, or press Enter after typing —
   an obvious thing to want — turns a latent defect into a live one with no
   warning. The failure is silent: the key is sent, the action reports success,
   and the data is in the wrong cell.
2. **It already cost real investigation time.** It corrupted every cell-targeting
   measurement in the Sheets work, produced a "Z3 artifact" that was written up
   as unexplained, and made a prototype look unreliable when the prototype was
   fine. See `complex-web-grid-capture-unreliable.md`.
3. **It is invisible from the call site.** `press_key("{Enter}")` reads as one
   keystroke. Nothing in the signature, the name, or the documentation suggests
   three are sent.

## What to do about it

Not fixed this session; this is the record, not the change.

- [ ] **Do not use `press_key` with Enter against grids.** Use `{Tab}` where it
      commits, or `type_text`, which sends exactly what it is given. If Enter is
      genuinely required, the injection has to be worked around at a lower level.
- [ ] **Add a guard if replay ever grows a key-press path.** The cheapest form is
      a lint or a wrapper that refuses `press_key` with Enter, so the decision is
      forced at the call site rather than discovered in a spreadsheet.
- [ ] **Raise it upstream.** The workaround is correct for its stated case and
      wrong in general; it belongs behind an opt-in flag rather than applied to
      every Enter unconditionally. Worth a `terminator-rs` issue.
- [ ] **Re-check on upgrade.** Pinned to 0.23.35 by observation. If the version
      moves, confirm whether the preamble is still unconditional.

## Related

* `complex-web-grid-capture-unreliable.md` — the investigation this surfaced in,
  where it explains the column-Z drift and confounded the editor-watcher
  prototype until it was removed.
