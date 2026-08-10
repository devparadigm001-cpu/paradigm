# Selector names match by substring, so a selector can resolve onto the wrong element

**Status:** confirmed at the source and reproduced with a real production
selector on a real desktop. **Not fixed** — this session established whether the
risk is real, deliberately without attempting a remedy.
**Affected:** every selector the product builds. `src/compile/mod.rs`
(`selector_for`) constructs `role:…|name:…`, and `src/replay/mod.rs` resolves it.
The matching itself is `terminator-rs`.
**Platform:** Windows / UI Automation.
**Found:** 2026-08-09, by accident, while identifying three selectors that
resolved suspiciously fast during the Gmail picker investigation.
**Severity: HIGH.** A selector can resolve **quickly, successfully, and onto the
wrong element**, and nothing in the result distinguishes that from a correct
match. Replay then acts on the wrong target and reports success — the same
silent-wrong-success class as the window-switch misattribution and the
multi-line duplication.

## Summary

`role:X|name:Y` does not match elements *named* Y. It matches elements whose name
**contains** Y. So a stored selector will happily resolve onto any unrelated
element that happens to have the recorded name as a substring — including
elements in other applications entirely.

The original theory was that this only bites on very short names. **That is
wrong**, and the real exposure is broader: the risk is a name being a substring
of some *longer* string on screen, which window titles produce constantly because
they embed document and page names.

## Evidence

### 1. Confirmed at the source, and flagged upstream as undecided

`terminator-rs`, `src/platforms/windows/engine.rs:1204`:

```rust
if let Some(name) = name {
    // use contains_name, its undetermined right now
    // wheather we should use `name` or `contains_name`
    matcher_builder = matcher_builder.contains_name(name);
}
```

`contains_name`, not exact `name`. The upstream comment says the choice is
unresolved, so this is neither a bug in that library nor a guarantee — it is an
open decision the product currently depends on.

### 2. A real production selector resolving onto the wrong element

`examples/text_capture_probe -- selectors <app_data_dir>` reads the **stored**
selectors out of the on-device database and resolves each against the live
desktop, comparing what it lands on against what was recorded:

```
  absent    role:Window|name:Untitled - Notepad          -> not on screen now
  OK        role:document|name:Text editor               -> exact match
  OK        role:document|name:Text editor               -> exact match
  OK        role:document|name:Text editor               -> exact match
  COLLISION role:Window|name:Paradigm
            recorded name : "Paradigm"
            resolved to   : "Paradigm Text Capture Probe and 63 more pages - Personal - Microsoft Edge"
                            (role="Window")

  selectors tested            : 5
  resolved exactly            : 3
  resolved to SOMETHING ELSE  : 1
  not present right now       : 1
```

**One selector in five landed on the wrong window.** Replaying that playbook as
it stands would activate a browser instead of the Paradigm app, and every
subsequent step would act in the wrong place.

#### How much of this instance is self-inflicted — stated plainly

The browser window title contained "Paradigm" because a probe page from this
session is called *Paradigm Text Capture Probe*. So this specific collision was
partly manufactured by the testing.

That does not rescue the finding, for two reasons. The mechanism does not care
where the string came from — **any** window whose title contains "Paradigm"
collides, and a user with a `Paradigm plan.docx` open or a tab titled
`Paradigm — roadmap` reproduces it exactly. And the colliding selector is the
application's own window name, which is not an unusual or badly-chosen target;
it is what capture records for a navigate step.

### 3. What capture actually records — the original worry was misplaced

The survey of stored steps:

| action | recorded name | length |
|---|---|---|
| navigate | `Untitled - Notepad` | 18 |
| click | `Text editor` | 11 |
| click | `Text editor` | 11 |
| type | `Text editor` | 11 |
| navigate | `Paradigm` | 8 |

**Zero of five names are 6 characters or fewer.** Capture records full labels,
not fragments, so the "capture produces short generic names" hypothesis is
refuted.

The collision came from an 8-character name that is a **prefix of a longer
title**. That reframes the risk: it is not name *length* that matters but whether
the name appears inside other on-screen text. Window titles are the most exposed
surface, because they routinely embed the name of whatever document, page, or
project is open — which is exactly the kind of string a user's other windows also
contain.

## Relationship to the window-selector ambiguity bug

Related, and worth keeping separate — see
`replay-window-selector-ambiguity.md`.

That bug is about **several windows matching one selector**, where replay picks
one silently. This is about **a selector matching an element that was never the
target at all**, which can happen even when exactly one thing matches.

They compound: substring matching increases the candidate pool, which makes
ambiguity more likely. But a fix for one does not fix the other. Notably, the
ambiguity investigation closed with the conclusion that the library exposes no
way to ask "how many things does this selector match" — and this finding adds
that it also does not distinguish "matched what you meant" from "matched
something containing what you meant".

## Why it matters

The failure is silent by construction. `Locator::first()` returning `Ok` means
"something matched", and nothing in that result carries whether it was the right
something. Replay acts, reports success, and the step log shows a plausible
selector resolving normally.

This is the fourth distinct route to the same outcome found in this project —
wrong content or wrong target, reported as success. The others are recorded in
`type-action-misattributed-after-window-switch.md`,
`multiline-document-capture-duplicates.md`, and
`replay-window-selector-ambiguity.md`. That recurrence is itself the finding
worth acting on: the pipeline has no verification step anywhere that asks
"did this do what the recording meant?", so every one of these failure modes is
invisible until a human notices wrong output.

## Next steps

- [ ] **Decide whether selectors should match names exactly.** The obvious
      remedy. The cost is that exact matching is brittle where names carry
      volatile suffixes — a window title that gains an unsaved-changes marker
      (`*Untitled - Notepad`) would stop matching. Worth measuring how often real
      captured names are stable enough for exact comparison before committing.
- [ ] **Consider anchoring instead of exact matching** — requiring the recorded
      name to match the full element name, or the beginning of it, rather than
      appearing anywhere inside it. That would have rejected the browser window
      for `name:Paradigm` while still tolerating suffix drift.
- [ ] **Prefer selectors that do not rely on names at all where possible.** A
      navigate step already carries a process name (added
      2026-08-09 for the ambiguity work, currently unused by replay). Matching a
      window by process plus role would not have collided here.
- [ ] **Survey a larger body of real captures.** Five stored steps is a thin
      sample, and one collision in five is a rate with no confidence behind it.
      Re-run `-- selectors` against a database with substantially more recorded
      playbooks before treating the frequency as meaningful.
- [ ] **Do not treat this as fixed by the ambiguity work.** They interact but are
      separate defects with separate remedies.
