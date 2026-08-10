# Gmail's recipient picker captures but does not replay

**Status:** confirmed by a real replay run, with a control recording that
isolates it. **Root cause measured** (2026-08-09): the element exists and the
selector is correct — it simply arrives 6,743 ms after Compose, against an
8,000 ms locate budget. A latency problem, not an accessibility one.
**Fix applied** the same day: the locate budget is split, with elements now
allowed 15s. **Verification is incomplete, and the 6,743 ms figure has since
been corrected** — that is a cold-*application* cost, not the widget's render
latency, which is ~12 ms once Gmail is warm. The fix is insurance against a
condition that has been produced exactly once and never with the fix in place.
See "The fix" and "Correction".
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

## That question was tested. The answer is: two separate bugs (2026-08-09)

**Everything above this heading was an inference from two data points, and it
said so.** It has now been tested directly, and the shared-root-cause framing
does not survive. The category is real; the common mechanism is not.

### The two general hypotheses, both refuted

Tested on a controlled page (`text_capture_probe -- widgets`) where one property
varies at a time — something the real sites cannot offer.

**Hypothesis 1: ARIA roles map inconsistently to UIA roles, so selectors built
from them are fragile by construction. REFUTED.**

| Declared ARIA | UIA reports |
|---|---|
| `group` | `Group` |
| `combobox` | `ComboBox` |
| `grid` | `DataGrid` |
| `gridcell` | `DataItem` |
| `textbox` | `Edit` |
| `listbox` | `List` |
| `option` | `ListItem` |

Every one is the standard documented mapping. This inverts the reading of the
evidence: Gmail's `role:group` and Sheets' `combobox` are **faithful** records.
Those sites really do declare a recipient field as a group and a cell as a
combobox. Capture is not mistranslating anything — it is accurately recording an
unhelpful choice made by the site.

**Hypothesis 2: re-rendering replaces DOM nodes, invalidating element handles
between capture and replay. REFUTED.**

```
  before        id=282054  text="original"
  after mutate  id=282054  text="mutated"     (id same: true)
  after replace id=282054  text="replaced"    (id same: true)
```

The UIA runtime id was **identical** after the node was destroyed and recreated
via `innerHTML`, and re-resolving the selector found it. Re-rendering alone
cannot explain an "element not found" at replay.

(One incidental finding: the *held* handle then read `""` rather than erroring —
stale but not dead, a silent wrong answer rather than a detectable failure. Same
shape as the `Ok(0)` window handle and the swallowed `Err` recorded in
`replay-window-selector-ambiguity.md`.)

### Why the two bugs are structurally different

With both general mechanisms gone, the failures stop resembling each other:

| | Sheets | Gmail |
|---|---|---|
| Pipeline half | **capture** | **replay** |
| The element | exists; is read wrongly | **does not exist** when needed |
| Symptom | empty/garbled text, attribution drifting between cells | selector times out at 8s, not found |
| Remaining explanation | async Name Box indirection, canvas-rendered cells | the picker is not rendered until compose is open and focused |

Sheets is a **timing-and-indirection** problem while the element is present.
Gmail is an **existence** problem. If the picker only renders once compose is
open, the selector failing is not an accessibility defect at all — it is replay
failing to reproduce the precondition that creates the widget. That needs no
shared mechanism to explain, and the Gmail doc listed the possibility from the
start without eliminating it.

### The one thing they do share, and what it is not

Both captured a **container** role rather than an editable leaf:
`role:group|name:"To - Select contacts"`, and `combobox` cells. In both, the
element the user interacts with, the element holding the value, and the element
holding the identity are three different things — capture records the wrapper the
click event names.

That is a genuine structural pattern, and plausibly a general weakness. But it
explains **neither** failure: it would not produce "not found", and it would not
produce garbled text. Recorded as an observation worth revisiting, not as the
unifying cause.

### Recommendation

**Treat these as two independent bugs, each already documented, and do not build
a unified "complex widget" strategy for them.** A general fix would be machinery
for a general problem, and the evidence does not support one existing. Each doc's
own next steps remain the right work.

What survives as genuinely useful is the pair of negative results above. Both
were plausible, both were load-bearing for the "different addressing approach"
argument, and both are now closed with measurements rather than left as open
suspicions.

**Confidence: moderate, not high.** Two mechanisms are refuted and the failure
shapes clearly differ, but the remaining site-specific explanations were not
themselves confirmed against the live sites. The single check that would most
change this verdict: **does Gmail's picker exist at replay time?** If it does,
and the selector still fails, Gmail moves back into the same family as Sheets.

That check has since been run — see the next section. The verdict stands.

The category lesson is unaffected and still worth keeping: every one of these was
found by a human on a real application, never by a probe. That is a
testing-coverage gap, not a mechanism.

## Measured: the element exists, and arrives at 6.7s (2026-08-09)

The doc's own open question — *does the picker exist at replay time, or is it
genuinely not rendered?* — was answered directly against live Gmail
(`text_capture_probe -- gmailpicker`: clicks Compose once, then polls the
accessibility tree on a schedule; never types, never sends).

```
================ VERDICT ================
  FOUND at   6743ms  role:Group|name:To - Select contacts     <- the recorded selector
  never found         role:Group|name:To recipients
  never found         role:Edit|name:To recipients
  FOUND at     16ms   role:ComboBox|name:To recipients
  FOUND at     16ms   role:Edit|name:To
  FOUND at     16ms   role:Button|name:To
```

**The exact selector from the failing run does exist.** It appears 6,743 ms after
Compose is clicked and persists at every later poll out to 62 s.

### Both candidate theories are refuted

* **"The widget is never rendered under that selector"** — no. It renders, and
  the recorded selector is correct for it.
* **"Replay never reproduced a precondition that creates the widget"** — no.
  Clicking Compose is sufficient; nothing else was needed.

### The sharpened diagnosis: replay-time latency against a fixed budget

`LOCATE_TIMEOUT` is 8,000 ms. The element arrives at 6,743 ms. **The margin is
1,257 ms — about 19%.**

That is the whole failure. Not an accessibility defect, not a missing
precondition: replay's fixed budget is barely wider than the widget's own render
latency, so anything that slows the run — a busier machine, earlier steps still
settling, a colder Gmail load, network variance — pushes past 8s and the step
reports "element not found". The original run
(`3abcb8a5-…`, timed out at 8s) is exactly what that looks like.

This reframes the fix from "address the widget differently" to "the timeout is
too tight for real web applications, and possibly should not be a fixed number
at all".

### The 16 ms selectors: confirmed false matches (2026-08-09)

Three selectors reported found at **16 ms**, before the compose window could
plausibly have rendered. They were recorded as suspected false matches and left
unresolved. **They have since been resolved, and they are false matches — but not
on Gmail UI. They are not in the browser at all.**

Resolving each with no compose window open:

```
role:Edit|name:To
    RESOLVED role="Edit"   name="Write your prompt to Claude"
    bounds  x=679 y=881 917x26

role:Button|name:To
    RESOLVED role="Button" name="Microsoft Store pinned"
    bounds  x=921 y=1020 56x61
```

The first is **Claude Code's own prompt input**. The second is a **Windows
taskbar button**. Both still resolve with Gmail showing no compose window, which
settles it: neither has anything to do with the recipient picker.

`role:ComboBox|name:To recipients` did not resolve in either state, so this run
cannot classify it.

#### The mechanism, which matters beyond this bug

**Name matching is substring-based.** `name:To` matched "Write your prompt **to**
Claude" and "Microsoft S**to**re pinned". A two-character name will match almost
anything on the desktop.

That has a consequence worth stating plainly: **a selector can resolve quickly,
confidently, and completely wrongly**, and nothing in the result distinguishes
that from a correct match. A short `name:` fragment is not a selector so much as
a substring search across every element on screen.

It also disposes of the worry this check was raised to settle. There was no
faster route to the picker being ignored by capture, so capture is not choosing
a slow selector when a fast one exists. The 15 s budget remains the right lever
for this bug.

That broader question has since been investigated and the risk is **confirmed
real** — a stored production selector (`role:Window|name:Paradigm`) was shown
resolving onto a browser window on a live desktop. It also corrected the theory:
the exposure is not short names but names that are substrings of longer on-screen
text, which window titles produce constantly. Filed separately as
`selector-matching-precision.md`.

### The fix: split the locate budget (2026-08-09)

`LOCATE_TIMEOUT` was one constant used for two different jobs. It is now two,
because the jobs have different shapes:

| Constant | Value | Used by | Why |
|---|---|---|---|
| `WINDOW_LOCATE_TIMEOUT` | 8s (unchanged) | `navigate` | A window either exists or it does not; waiting longer buys nothing, and `navigate` falls back to `activate_application`, so a miss is not fatal |
| `ELEMENT_LOCATE_TIMEOUT` | **15s** (was 8s) | click / type | Elements in web applications render lazily. This is the path the picker failed on — `role:Group` is not a window |

15s is ~2.2× the measured 6,743 ms. 3× was considered and rejected: the
measurement came from a loaded machine (90+ browser tabs, three days' uptime),
so 6.7s is likely near the slow end rather than the median.

**A blanket increase was rejected deliberately.** Raising the window budget too
would slow every honest "window not found" for no benefit, since window
realisation was already shown to fit inside 5s.

#### The cost, measured rather than asserted

A longer budget means a genuinely wrong selector takes longer to fail honestly.
That was predicted and then measured:

* `tests/replay_aborted.rs`, which drives a deliberately-unfindable selector,
  went from **8.62s to 15.40s**.
* A nonexistent selector against the 15s budget took **16.57s** — a ~10%
  overshoot, so the locator does not honour the deadline precisely. The real
  cost of an honest failure is closer to 16.5s than 15s. Reproduced at
  **16.70s** on a later run, so the overshoot is consistent rather than noise.

This is accepted because the opposite error is worse: failing a step whose
element was merely slow fails the entire run and forces the user to re-record.
Waiting is an annoyance; a false failure destroys the work.

#### Verified: no regression elsewhere

82 lib, 8 `db_encryption`, 15 `ipc_commands`, 2 `replay_aborted` pass. A–D
8/8 across two probe runs, both multi-line exit paths exact, window-switch
ordering correct. (`ipc_pipeline` remains deliberately red on the unrelated
no-settle race.)

#### Correction: 6,743 ms is not the widget's render latency

Two follow-up runs were needed, and the second overturned the explanation above
this section. Both are recorded because the correction is the useful part.

**Run 2 (warm, accidental).** The picker was found at **19 ms**. The empty draft
from run 1 was still open, so Gmail restored the compose window and the widget
was already rendered before the click. Measured a warm widget; proved nothing.

**Run 3 (widget-cold, deliberate).** The probe gained a pre-flight that closes
any leftover compose window — only after reading its body and confirming it
empty, aborting otherwise — and then *gates* on the picker being absent before
clicking Compose:

```
  no compose window open
  cold-start precondition (picker ABSENT before clicking Compose): MET

  FOUND at     12ms  role:Group|name:To - Select contacts
```

**The precondition was met and the picker still appeared in 12 ms.** So 6,743 ms
is not what this widget costs to render. Widget-cold, it is ~12 ms — a **560×
difference** from the original measurement.

What actually differed in run 1: Gmail itself had been loaded 20 seconds earlier
and was still initialising. Runs 2 and 3 clicked Compose in an already-warm
Gmail. **Widget-cold and app-cold are different things, and only the first was
achieved.**

#### What this means for the fix

The fix stands, but its justification changes shape:

* The 6,743 ms figure is real — it was measured — but it is a **first-interaction
  cost of a cold Gmail application**, not a property of the picker.
* In the ordinary warm case the picker resolves in ~12 ms, so the 15 s budget is
  not routinely stressed. It is insurance against the cold-app case.
* The observed spread is **12 ms to 6,743 ms**, and nothing establishes 6,743 ms
  as the ceiling — it is one sample of one cold start. A budget sized at 2.2× a
  single unbounded-looking sample is defensible insurance, not a calculated
  bound, and should be described that way.

#### Still not verified: an app-cold replay

Reproducing 6,743 ms needs Gmail genuinely cold — the tab closed, or a fresh
browser profile — not merely the compose window closed. That was not achieved,
so **the fix has never been exercised against the condition it was written
for.**

What *is* established: the widget exists, the selector is correct, the warm path
is fast, and the failure-cost of the longer budget is measured. What is not: that
15 s actually covers a cold-app start, because that start has been produced
exactly once, by accident, before the fix existed.

**Recorded as an open verification. Not claimed as done.**

### Effect on the "two separate bugs" verdict: none

The verdict stands. This sharpens Gmail's half rather than moving it:

| | Sheets | Gmail (now) |
|---|---|---|
| Pipeline half | capture | replay |
| Failure | element present, value/attribution misread | element present, **arrives after the budget** |
| Remedy | correct read timing and identity indirection | a longer or adaptive locate budget |

Still different halves, still different remedies. Nothing here brings them
closer together — if anything the diagnoses have diverged further, since Gmail's
is now a plain latency-budget question with no accessibility component at all.

### A theme worth watching, explicitly not a mechanism

Gmail's failure does rhyme with two defects fixed earlier the same session: the
no-settle race (a baseline read *before* the text lands) and the window-switch
misattribution (a flush read *after* the context changed). The common shape is
**one measurement taken at one fixed moment against an asynchronous UI**.

That is offered as a lens for reading future bugs, **not promoted to a
mechanism**. This document already records what happened the last time a shared
shape was treated as a shared cause: the "dynamic widget" theory was asserted
across two findings and did not survive being tested. A third grouping made on
resemblance would repeat that mistake. If this theme is ever to be more than an
observation, it needs the same treatment — a controlled test that could refute
it.

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

- [x] ~~**Determine why the selector does not resolve.**~~ **Answered.** The
      element is present and the selector is correct; it arrives 6,743 ms after
      Compose. Neither "absent" nor "different identity".
- [x] ~~**Check whether the widget needs interaction before it exists.**~~
      **Answered.** Clicking Compose is sufficient; no further precondition.
- [x] ~~**Test whether the 8s timeout is the binding constraint.**~~
      **Answered, and it is** — 6,743 ms against an 8,000 ms budget leaves a
      19% margin. The note that it "would be embarrassing to redesign selector
      strategy over a timeout that was merely short" turned out to be the right
      instinct.
- [x] ~~**Decide what the locate budget should be.**~~ **Done** — split into
      `WINDOW_LOCATE_TIMEOUT` (8s) and `ELEMENT_LOCATE_TIMEOUT` (15s). See
      "The fix".
- [ ] **Confirm the fix against an app-cold Gmail.** Two attempts failed to
      reproduce the condition. Closing the compose window is not enough — that
      yields 12 ms, because the *application* is still warm. Reproducing
      6,743 ms needs the Gmail tab closed entirely or a fresh browser profile.
      Until then the fix has never been exercised against the case it was
      written for.
- [ ] **Establish whether 6,743 ms is anywhere near the ceiling.** It is one
      sample of one cold start. The observed spread is 12 ms to 6,743 ms with no
      upper bound established, so 15s is insurance rather than a calculated
      bound. Several cold starts would say whether it is the right size.
- [ ] **Consider a readiness signal instead of a deadline.** If failures recur
      at 15s, a larger number is the wrong answer. Waiting for the element to
      appear, with a generous ceiling, would decouple the common case from the
      worst case — and would also fix the ~10% overshoot measured on the
      deadline itself.
- [x] ~~**Check whether the 16 ms selectors address the same element.**~~
      **Answered: they do not.** `role:Edit|name:To` resolves to Claude Code's
      prompt input and `role:Button|name:To` to a Windows taskbar button, both
      outside the browser entirely. Capture is not ignoring a faster selector.
- [ ] **Consider whether substring name matching is a liability elsewhere.**
      Surfaced by the above: `name:To` matched "Write your prompt **to** Claude"
      and "Microsoft S**to**re pinned". Every selector the product builds uses
      the same matching, and short element names are common, so a selector can
      resolve fast and confidently onto the wrong element with nothing in the
      result to signal it. Untested beyond this instance; worth its own look.
- [x] ~~**Decide whether the JS-widget pattern warrants a general
      investigation.**~~ **Done, and the answer is no** — see "That question was
      tested". The shared-cause theory was refuted; these are two independent
      bugs.
- [ ] **Add a dynamic-widget target to routine probe coverage.** Every probe to
      date uses plain `<input>` elements or Notepad. All three findings above
      required a human on a real application to surface, which is the actual
      gap.
