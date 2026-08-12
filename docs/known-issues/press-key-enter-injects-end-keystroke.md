# `press_key` with Enter injects a hidden `{END}`, which moves the cursor in a grid

**Status: CLOSED, 2026-08-11 — guarded, not fixed upstream.** The defect is real
and remains in `terminator-rs`; what changed is that Paradigm can no longer walk
into it silently. `press_key` is now called in `src/` (it was not when this doc
was written), so the exposure moved from latent to real-but-contained, and the
containment is a test rather than a comment. See "The decision".
**Affected:** `terminator-rs` 0.23.35,
`platforms/windows/element.rs:1161-1170` (line numbers re-verified against the
published 0.23.35 source, 2026-08-12). The upstream report is drafted and ready
to paste under "Upstream report, ready to file"; only submission remains, which
needs a personal GitHub account. Any caller of
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

## Upstream report, ready to file

Everything needed is below; it could not be submitted from this session (filing
needs a personal GitHub account). Paste as-is.

**Where:** `mediar-ai/terminator` — **confirmed, not assumed.** The crate
manifest for `terminator-rs` 0.23.35 gives
`repository = "https://github.com/mediar-ai/terminator"`, and the injection is
terminator's own code at `src/platforms/windows/element.rs:1161-1170`. It is
*not* in the `uiautomation` crate: that crate only provides `send_keys`, which
sends exactly what it is given. The `{LEFT}{END}` preamble is added by
terminator before calling it.

**Title:** `press_key` silently injects `{LEFT}{END}` before any Enter/Return,
relocating the cursor in grids and spreadsheets

**Version:** `terminator-rs` 0.23.35. Verified present in the published source.

**Platform:** Windows. Demonstrated in Google Sheets; the mechanism is not
Sheets-specific — it affects any target where `End` is a navigation key.

### The defect

`UIElement::press_key` sends two extra keystrokes before the requested key
whenever the key name contains `ENTER` or `RETURN`:

```rust
// Dismiss inline autocomplete before pressing Enter/Return
// This prevents unwanted autocomplete suggestions (e.g., Chrome address bar) from being accepted
let key_upper = key.to_uppercase();
if key_upper.contains("ENTER") || key_upper.contains("RETURN") {
    let _ = self.element.0.send_keys("{LEFT}", 10);
    let _ = self.element.0.send_keys("{END}", 10);
}

self.element.0.send_keys(key, 10)
```

The intent is clear from the comment and is reasonable **for its stated case**:
in a browser address bar, `Left` then `End` dismisses an inline autocomplete
suggestion and restores the caret. In a single-line text field it is invisible
and harmless.

**In a grid it is neither.** `End` is a navigation key there — in Google Sheets
it moves the cursor toward the last column of the data region. So
`press_key("{Enter}")` does not mean "commit this cell"; it means "move
somewhere else, then commit".

**There is no way to opt out through the public API.** The condition is a
substring test on the key name, and the only two spellings the underlying crate
accepts for that key — `ENTER` and `RETURN` — both match. There is no other name
that reaches `VK_RETURN`.

### Expected vs actual

**Expected:** `press_key("{Enter}")` sends Enter.

**Actual:** it sends `{LEFT}`, then `{END}`, then Enter — and reports success.
Nothing in the signature, the name, or the documentation suggests three
keystrokes are sent.

### Reproduction

1. Open a Google Sheets document (any grid where `End` navigates).
2. Select cell `A1`.
3. With terminator, type a value into the cell editor and commit it with
   `element.press_key("{Enter}")`.
4. Repeat for a second value.
5. **Export the sheet as CSV** — do not read the position back from the UI that
   produced it.

### Measured evidence

Ground truth is Sheets' own CSV export, not the accessibility tree:

```
  "111,,,,,,,,,,,,,,,,,,,,,,,,,alpha"
  ",,,,,,,,,,,,,,,,,,,,,,,,,betagamma"
```

`111` is in `A1`, where it was typed. `alpha` is in **column 26 — `Z1`** — and
`betagamma` in `Z2`. **Twenty-five empty columns of drift**, in a document where
the only navigation performed between edits was a commit.

**The control isolates the cause to exactly one variable.** An identical run
committing with `{Tab}` instead — which also commits a cell edit, and contains
neither `ENTER` nor `RETURN`, so it is sent verbatim:

```
    row 1   "apple,banana,cherry"
```

`A1`, `B1`, `C1`. No drift, three runs, identical. The only difference between
the two experiments is which key committed the edit.

This was initially misdiagnosed as a phantom accessibility element reporting a
fake position, because the element carried a stable runtime id across documents.
The CSV settles it: the data physically went to column Z. The position was real,
and the automation put it there.

### Why this is particularly dangerous for an automation library

1. **It is silent.** The key is sent, `press_key` returns `Ok`, and the run
   reports success. There is no error to catch and nothing in the return value
   distinguishes a correct commit from a commit 25 columns away.
2. **It corrupts data rather than failing.** In a spreadsheet the result is a
   write to the wrong cell in a real document — not a no-op, not an exception, a
   wrong value in the wrong place that persists.
3. **It is invisible from the call site.** `press_key("{Enter}")` reads as one
   keystroke, so the behaviour cannot be inferred from the calling code and will
   not be found by review.
4. **It cost real investigation time.** It corrupted every cell-targeting
   measurement in an unrelated investigation and made a working prototype look
   unreliable, until the CSV export was used as ground truth.
5. **Automation is exactly the context where the workaround is least needed.** A
   programmatic `press_key` on a spreadsheet cell has no inline autocomplete to
   dismiss; the workaround targets a browser address bar interaction that a grid
   commit never involves.

### Suggested fix

Any of these resolves it; they are listed cheapest-first:

1. **Make it opt-in.** A `press_key_raw()`, or a parameter/builder flag such as
   `dismiss_inline_autocomplete: bool` defaulting to `false`. Callers that want
   the address-bar behaviour ask for it.
2. **Scope it to the case it was written for.** `press_key` already fetches
   `control_type` immediately above this block (`element.rs:1155`). Applying the
   preamble only for an address-bar-like target — e.g. an `Edit`/`ComboBox` in a
   browser window, and never for a grid, document, or spreadsheet control —
   would keep the intended benefit and remove the damage.
3. **At minimum, document it.** If the behaviour must stay unconditional, say so
   in the `press_key` doc comment, since it cannot currently be discovered
   without reading the source.

Option 1 is preferable: the current behaviour is correct for a narrow case and
wrong applied universally, which is what a flag expresses.

### Note for the maintainer

A caller that needs a genuine Enter today has no workaround through the public
API — every accepted spelling of the key matches the substring test. Downstream
code must avoid the key entirely (committing with `{Tab}` where the target
allows it), which is not always possible.

## What to do about it

Not fixed this session; this is the record, not the change.

- [x] ~~**Do not use `press_key` with Enter against grids.**~~ Encoded as
      `GRID_COMMIT_KEY` plus `press_key_injects_navigation`, and enforced by a
      test that was verified to fail when the key is swapped for Enter.
- [x] ~~**Add a guard if replay ever grows a key-press path.**~~ It grew one —
      `grid_type` — and the guard landed with it.
- [ ] **Raise it upstream.** Still worth a `mediar-ai/terminator` issue: the
      workaround is right for a browser address bar and wrong applied to every
      Enter, so it belongs behind an opt-in. **The report is now written and
      ready to paste** — see "Upstream report, ready to file", including the
      repo confirmation that the injection is terminator's own code and not the
      `uiautomation` crate's. Still not filable from this session: submitting
      needs a personal GitHub account. Deliberately not the containment — an
      upstream fix arrives on someone else's timeline against a version this
      project pins.
- [ ] **Re-check on upgrade.** Pinned to 0.23.35 by observation. If the version
      moves, confirm whether the preamble is still unconditional — and note the
      guard above would NOT notice if upstream silently widened the injection to
      other keys, since it only knows about ENTER and RETURN.

## Related

* `complex-web-grid-capture-unreliable.md` — the investigation this surfaced in,
  where it explains the column-Z drift and confounded the editor-watcher
  prototype until it was removed.
