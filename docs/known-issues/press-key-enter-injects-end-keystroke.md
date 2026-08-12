# `press_key` with Enter injects a hidden `{END}`, which moves the cursor in a grid

**Status: CLOSED, 2026-08-11 — guarded, not fixed upstream.** The defect is real
and remains in `terminator-rs`; what changed is that Paradigm can no longer walk
into it silently. `press_key` is now called in `src/` (it was not when this doc
was written), so the exposure moved from latent to real-but-contained, and the
containment is a test rather than a comment. See "The decision".
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

> **Superseded later the same day.** This section was true when written and is
> kept for the severity correction it records. It is no longer accurate: `src/`
> now calls `press_key` in `replay::grid_type`, so the "zero occurrences" finding
> below has expired. See "The decision" for the current position.

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

## The decision (2026-08-11)

### The premise changed first: this is no longer unreachable

This doc concluded that `press_key` appears zero times in `src/`. **That is no
longer true.** `replay::grid_type`, added later the same day to replay Sheets
cell edits, calls it twice:

| Call | Injects `{LEFT}{END}`? | Safe? |
|---|---|---|
| `name_box.press_key("{Enter}")` | **yes** | yes — inside a text field those are caret moves |
| `committer.press_key("{Tab}")` | no — "Tab" contains neither ENTER nor RETURN | yes |

So the injection now fires on **every grid replay**, harmlessly, by design. The
exposure is not "some future call site might do this"; it is "one edit to an
existing call site turns a working replay into a silent wrong-cell write". That
is a materially different risk from the one this doc was opened with, and it
narrows the choice.

### Not patching the dependency

Vendoring or `[patch]`-ing `terminator-rs` was rejected on two grounds, neither
of them squeamishness:

* **No precedent.** `Cargo.toml` has no `[patch]` section and no `path`/`git`
  dependencies; everything is a pinned registry version. (`rusqlite`'s
  `bundled-sqlcipher-vendored-openssl` is a crate feature, not a patched
  dependency.) Introducing dependency patching means re-applying and re-verifying
  it on every version bump, forever.
* **Disproportionate to the exposure.** Both existing call sites are already
  correct. A patch would buy nothing today and cost maintenance indefinitely.

### Not fixing it upstream either — though it should be reported

The workaround is correct for its stated case and wrong applied unconditionally;
it belongs behind an opt-in flag. That is a real upstream bug and worth filing.
But an upstream fix lands on someone else's timeline, against a version this
project pins, so it cannot be the thing that protects this codebase. **Filing it
remains a genuine open item** — see below — it is just not the containment.

### What was done: a guard that fails, where the mistake would be made

A comment cannot fail, and this doc already had one at the exact call site. So
the rule is now executable:

* `press_key_injects_navigation(key)` states the rule — any key naming ENTER or
  RETURN is prefixed with `{LEFT}{END}` — with the measurement behind it.
* `GRID_COMMIT_KEY` names the commit key instead of hard-coding `"{Tab}"` inline.
* `the_grid_commit_key_cannot_relocate_the_cursor` asserts the commit key is not
  one that injects, and separately pins the rule itself so the assertion cannot
  rot into a tautology.
* `the_name_box_enter_is_deliberate_and_safe` records why the *same key* is
  correct in one call and wrong in the other — the distinction most likely to be
  lost by someone later "making the two consistent".
* A `debug_assert!` at the call site, for anyone reading the code rather than the
  tests.

**The guard was verified to fail.** Substituting `"{Enter}"` for
`GRID_COMMIT_KEY` produces:

```
test replay::tests::the_grid_commit_key_cannot_relocate_the_cursor ... FAILED
assertion failed: !press_key_injects_navigation(GRID_COMMIT_KEY)
```

A green test that cannot go red would have been the same as the comment it
replaced.

### Why this is the proportionate answer

The defect cannot be removed from a pinned dependency without owning a patch, and
it does not need to be: it is harmless wherever this codebase calls it today. The
whole risk was that a future edit would reintroduce it *silently*. A failing test
at that exact edit removes the silence, which is the property that mattered.

## What to do about it

Not fixed this session; this is the record, not the change.

- [x] ~~**Do not use `press_key` with Enter against grids.**~~ Encoded as
      `GRID_COMMIT_KEY` plus `press_key_injects_navigation`, and enforced by a
      test that was verified to fail when the key is swapped for Enter.
- [x] ~~**Add a guard if replay ever grows a key-press path.**~~ It grew one —
      `grid_type` — and the guard landed with it.
- [ ] **Raise it upstream.** Still worth a `terminator-rs` issue: the workaround
      is right for a browser address bar and wrong applied to every Enter, so it
      belongs behind an opt-in. Not something this session can file, and
      deliberately not the containment — an upstream fix arrives on someone
      else's timeline against a version this project pins.
- [ ] **Re-check on upgrade.** Pinned to 0.23.35 by observation. If the version
      moves, confirm whether the preamble is still unconditional — and note the
      guard above would NOT notice if upstream silently widened the injection to
      other keys, since it only knows about ENTER and RETURN.

## Related

* `complex-web-grid-capture-unreliable.md` — the investigation this surfaced in,
  where it explains the column-Z drift and confounded the editor-watcher
  prototype until it was removed.
