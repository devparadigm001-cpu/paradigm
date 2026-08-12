# Selector names match by substring, so a selector can resolve onto the wrong element

**Status:** **RESOLVED** (2026-08-12) — confirmed at the source, reproduced with
a real production selector on a real desktop, and closed at replay. Replay does
not merely veto a bad match: it *selects* the element whose name equals the
recorded one, tolerating only a leading `*`, and refuses when several qualify.
The library's containment matching is unchanged and **deliberately so** — see
"The decision" below, which settles the exact-vs-containment question this file
opened. See also `post-execution-verification-design.md`.
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

## The decision: exact matching stays where it is (2026-08-12)

The question this file opened — *should Paradigm's selector layer match names
exactly rather than by containment* — is answered **yes, and it already does**.
The remaining question was *where in the pipeline*, and the answer is **no
further change**: the current two-layer design is the final architecture.

### The premise that had gone stale

This file described the exact-name check as a mitigation that "catches the
consequence, not the cause" — a veto applied after resolution. That is no longer
what the code does. `replay::resolve_recorded` enumerates the candidates, keeps
those whose name *is* the recorded one, and **returns that element**;
`replay/mod.rs:693` and `:919` then act on it, discarding whatever `first()`
picked. Exact matching is the selection mechanism, not a safety net over it. A
legitimate replay that `first()` mis-resolved onto a containment decoy now
succeeds *because* of the exact match, where the earlier veto-only version
refused it.

So Paradigm's own construction/comparison layer is already exact. What follows is
why pushing that exactness further upstream is not an improvement.

### The invariant that makes containment correct underneath it

Resolution is a **generator** and an **acceptor**:

* **C**, the candidate set — what the library's containment match yields.
  Measured, not assumed: `contains_name` is
  `NameFilter { casesensitive: false, partial: true }`
  (`uiautomation-0.22.2/src/core.rs:1823`), evaluated as
  `element_name.to_lowercase().contains(&condition_name.to_lowercase())`
  (`filters.rs:85-93`). Case-**insensitive** containment.
* **A**, the accepted set — `resolved_is_recorded_target`: exact equality, or
  equality after stripping one leading `*`.

Replay acts on the single member of **C ∩ A**. Correctness requires **A ⊆ C** —
every name the acceptor would accept must be one the generator can actually hand
it. Both accepted forms contain the recorded name, so this holds.

**The generator being wider than the acceptor is not the defect. It is the
required direction.** A narrower generator cannot improve C ∩ A; it can only
withhold candidates the acceptor wanted. This is now pinned by
`every_accepted_name_is_one_the_library_could_have_produced`, which brute-forces
the implication over a generated corpus plus the real measured names. The failure
it guards is silent: a future tolerance that is not a containment-subset — say,
stripping a leading `"Draft "` — would read as working while the generator never
produced such a candidate for it to accept.

### Why exact matching earlier is a regression, not a fix

**It is achievable.** Contrary to the assumption that this needs a patched
dependency, terminator exposes an exact-matching selector form:
`Selector::Attributes` compares with `!=` on lowercased property values
(`engine.rs:1519-1524`), reachable from a selector *string* via `attr:`. So the
two prior rejections of dependency patching
(`press-key-enter-injects-end-keystroke.md`, `terminator-multi-monitor-visibility.md`)
do not decide this one. It was rejected on its own merits:

1. **It cannot change any outcome.** The acted-on element is the unique member of
   C ∩ A. Narrowing C to any set still ⊇ A leaves C ∩ A identical. Exact
   generation is a re-implementation of the same predicate at a more expensive
   layer.
2. **It would drop the `*` tolerance.** A = {recorded, `*`recorded}; an exact
   generator yields only {recorded}. The `*` case is a *measured* false positive
   (`DriftProbe …` → `*DriftProbe …`). Exact generation turns a currently-working
   replay into "matched nothing". Recovering it needs a second query and a union
   — whose result is, again, exactly what containment-then-filter already gives.
3. **It is more expensive and more fragile.** `Attributes` calls
   `get_property_value` per element per property across the tree, and matches
   `ControlType` via `property_value.to_string()` — an enum rendering, so `role`
   matching becomes brittle. It would also invalidate the derived
   `AMBIGUITY_DEPTH` argument, which is grounded specifically in
   `Selector::Role`'s `calculate_search_depth` behaviour.
4. **Construction-time changes reach the wrong population.** `selector_for` runs
   at compile time and the string is **persisted** into `action_payload_json`
   (`compile/mod.rs:119`). Changing it protects only newly recorded playbooks;
   every already-stored playbook keeps its `role:X|name:Y` and its containment
   behaviour. For a defect whose severity is precisely "already-stored selectors
   resolve onto the wrong thing", that is the wrong end of the pipeline. The
   replay-side acceptor covers old and new playbooks with no migration.

### The anchoring proposal was wrong, and the evidence is in this file

This file previously proposed anchoring — "requiring the recorded name to match
the full element name, **or the beginning of it**" — and claimed it "would have
rejected the browser window for `name:Paradigm`".

**It would have accepted it.** The recorded name is `Paradigm`; the colliding
title is `Paradigm Text Capture Probe and 63 more pages - Personal - Microsoft
Edge`. The recorded name *is* a prefix. Prefix anchoring fails against the exact
collision that motivated it. Suffix anchoring rejects that one but readmits
`Notepad` → `Untitled - Notepad`, which the shipped rule already rejects.

Equality-modulo-`*` is stricter than either anchoring direction, so anchoring is
not a middle ground between containment and exactness — it is strictly weaker
than what ships. Pinned by `anchoring_would_not_have_rejected_the_measured_collision`.

### Residual exposure, stated plainly

* **The acceptor is case-sensitive; the generator is not.** A title whose case
  drifts is generated but rejected, producing a loud `FailedWrongTarget` rather
  than a wrong action. Fail-safe, and left alone.
* **A blank recorded name refuses every replay of that step.** `selector_for`
  trims the name and drops it when empty, while `target_name` is stored
  untrimmed, so a whitespace-only name yields a role-only selector plus an
  unmatchable comparison key. Fail-safe (it refuses rather than acting), and not
  reachable from the click/type capture path, which trims via `non_empty`. Not
  worth code today; recorded so it is not rediscovered as a mystery.
* **`grid_type`'s `name:Name box` lookup is containment and unchecked**, because
  a grid cell has no recorded element name. Its verification is different in kind
  — the Name Box read-back at `replay/mod.rs:829` — and is covered by
  `press-key-enter-injects-end-keystroke.md`.

## Still open

- [ ] **Survey a larger body of real captures.** Five stored steps is a thin
      sample, and one collision in five is a rate with no confidence behind it.
      Re-run `-- selectors` against a database with substantially more recorded
      playbooks before treating the frequency as meaningful. This measures
      *exposure*, not the remedy — the remedy above does not depend on it.
- [ ] **Prefer selectors that do not rely on names at all where possible.** A
      navigate step already carries a process name (added 2026-08-09 for the
      ambiguity work, currently unused by replay). Matching a window by process
      plus role would not have collided here. Independent of this decision:
      it would shrink C, which the reasoning above shows cannot change a result,
      but it would make resolution cheaper and less dependent on volatile titles.
- [x] ~~Decide whether selectors should match names exactly.~~ Decided above.
- [x] ~~Consider anchoring instead of exact matching.~~ Rejected above, on this
      file's own evidence.

### Standing note — not a task

Deliberately not a checkbox: there is no work here to complete, and leaving it
as one implied pending work that does not exist.

**Do not treat this as fixed by the ambiguity work.** They interact but are
separate defects with separate remedies. Both are now closed, by two different
properties of the same enumeration — `resolved_is_recorded_target` decides
*which* element is the recorded one, and the candidate count decides whether
that answer is unique. A future change that removes one because "the other
already covers it" reopens a defect. See `replay-window-selector-ambiguity.md`.
