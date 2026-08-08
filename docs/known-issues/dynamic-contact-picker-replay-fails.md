# Gmail's recipient picker captures but does not replay

**Status:** confirmed by a real replay run, with a control recording that
isolates it. Root cause not investigated.
**Affected:** replay of dynamic, JS-rendered widgets — observed on Gmail's
compose "To" field (`role:group`, name `"To - Select contacts"`). Capture
records it; `replay` cannot find it again.
**Platform:** Windows. Observed in Gmail compose. Other mail clients untested.
**Found:** 2026-08-07, during exploratory live human testing — the same kind of
real-user session as Step 12, though not Step 12 itself.
**Severity: MEDIUM-HIGH for the product, not a blocker.** Email is one of the
most common targets for the repetitive work this product automates, and a
playbook that cannot address a recipient cannot send anything. It is not a
Step 12 blocker: it was found in later exploratory testing rather than in the
core human end-to-end test, and the rest of the compose flow replays correctly.

## Summary

The recipient field can be recorded, but the selector that recording produces
does not resolve on replay. `role:group|name:"To - Select contacts"` timed out
after 8 seconds with "element not found", failing the run at its final step.

A second recording, in which the recipient was filled in **before** recording
started — so the picker was never part of the captured session — replayed the
subject and body correctly. The failure is therefore specific to interacting
with the contact-picker widget, not to the compose flow in general.

## Evidence

### The failing run

Run `7b99fae2-6935-4d35-8144-054a788b5d81`:

| | |
|---|---|
| Status | **FAILED** |
| Steps attempted | 6 of 6 |
| Failing selector | `role:group|name:"To - Select contacts"` |
| Failure | element not found, after an 8s timeout |

The step was reached — the preceding five steps executed — so this is not a
run that fell over early. Replay got as far as the recipient field and could
not find it.

### The control: same flow, picker excluded

A second session recorded the same compose task with the recipient **already
filled in before recording began**. The picker never entered the capture, and
that recording replayed the subject and body correctly.

Two recordings of the same underlying task, differing in whether the picker is
part of the playbook, with the failure tracking that difference exactly. That is
what makes this a widget-specific problem rather than a general fragility in the
compose flow, and it is stronger evidence than the failing run alone.

### What is not established

Why the selector fails to resolve has not been investigated. Several
explanations fit the evidence and have not been separated:

* the element may not exist at replay time in the state the recording captured
  it in — pickers of this kind often render only once focused or hovered;
* `role:group` with a display-string name may be an unstable identity that Gmail
  regenerates between sessions;
* the 8s timeout may simply be short for a widget that appears late.

Nothing here distinguishes "the element is gone" from "the element is there
under a different identity".

## The broader pattern — three real-world findings, one shape

This is the third finding from live human testing where a target that is not a
plain form field behaves unlike anything the probes cover:

1. **Sheets — clipboard operations invisible.** Copy/paste produced no captured
   action at all (`complex-web-grid-capture-unreliable.md`, Finding 1).
2. **Sheets — grid cells garbled and misattributed.** Typed text empty or
   corrupted against `combobox`-role cells, with clicks and types attributed to
   different cells (same doc, Finding 2).
3. **Gmail — recipient picker unreplayable.** This document.

The common thread is that **dynamic, JS-rendered widgets do not present stable
accessibility elements the way a plain `<input>` does.** A Sheets cell is a
`combobox` whose identity comes from an asynchronously-updating Name Box; a
Gmail recipient field is a `role:group` that replay cannot find again. Neither
behaves like the `role:Edit` inputs every probe has exercised, and both were
found only when a human used the real application.

Two honest qualifications on that claim:

* **It spans different halves of the pipeline.** The Sheets findings are
  *capture* failures; this is a *replay* failure. That is not one root cause,
  and the pattern should not be read as implying a single fix.
* **The sample is three findings across two applications.** That is enough to
  justify looking for a general problem; it is not enough to have established
  one.

A related fourth case is worth noting but is *not* the same pattern: Notepad
typing captured nothing because its `Document` role was unrecognised
(`multiline-document-capture-duplicates.md` and the fix that preceded it).
Notepad's element was perfectly stable — we simply did not accept it. The
shared lesson there is about probe coverage, not about widget dynamism.

The question this raises, and which is the real reason to file it: **is
"complex JS-driven web widget" a category that needs its own strategy**, rather
than three defects fixed one at a time? Element-based selectors assume a stable
tree. Where that assumption does not hold, a different addressing approach —
positional, relational, or vision-based — may be required regardless of how many
individual widget bugs are fixed.

## Secondary observation: `"est"` for `"Test"`

The same session captured the Subject field's payload as `"est"` where `"Test"`
was likely intended — a missing leading character.

The obvious reading is another instance of the no-settle typing race in
`text-input-capture-truncation.md`, and it may well be. **But the shape is
inverted from what that defect produces**, and that is worth recording rather
than glossing: the documented race keeps a leading *prefix* (`"probe-user"` →
`"p"`), whereas this dropped the leading character and kept the *remainder*.

An alternative fits better and should be ruled out first: capture now reads the
element's value directly, so a payload of `"est"` means the field itself
contained `"est"` — the `T` never reached it. That would be an input-delivery
artifact (the first keystroke landing before focus settled in the field, or the
app swallowing it), not a capture defect at all. In that case nothing in the
capture pipeline is at fault and the recording is a faithful record of what
actually happened.

Filed here as a footnote, not as a finding. See
`text-input-capture-truncation.md` for the race it may or may not belong to.

## Why it matters

Addressing an email is not an incidental step — it is the step that determines
who receives the result. A playbook that replays the subject and body but fails
at the recipient does not partially work; it does not work at all, and it fails
at the point where the consequences of getting it wrong are highest.

The failure mode is at least a loud one, which distinguishes it from most of the
capture defects filed so far: replay reports FAILED with a named selector and a
timeout, rather than quietly doing the wrong thing. A user sees that something
broke. That is worth noting because it makes this *safer* than the silent
corruptions, even though it is more visible.

Gmail is also not an exotic target. If the recipient picker cannot be automated,
a large share of the obvious use cases for this product are unavailable.

## Next steps

- [ ] **Determine why the selector does not resolve.** Inspect Gmail's compose
      DOM/accessibility tree at replay time and compare against what was
      captured. Distinguish "element absent" from "element present under a
      different identity" before proposing any fix — the two imply completely
      different solutions.
- [ ] **Check whether the widget needs interaction before it exists.** If the
      picker only renders once focused, replay may need to reproduce the
      focusing step rather than address the picker directly.
- [ ] **Test whether the 8s timeout is the binding constraint.** Cheap to
      check, and it would be embarrassing to redesign selector strategy over a
      timeout that was merely short.
- [ ] **Decide whether the JS-widget pattern warrants a general investigation.**
      Three findings across two applications is the point at which it is worth
      asking whether element-based addressing is sufficient for this class of
      target, rather than continuing to fix instances. This is a strategy
      question, not a bug fix.
- [ ] **Add a dynamic-widget target to routine probe coverage.** Every probe to
      date uses plain `<input>` elements or Notepad. All three findings above
      required a human on a real application to surface, which is the actual
      gap.
