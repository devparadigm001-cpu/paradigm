# Replay verification: design, prototype, and what it would actually have caught

**Status:** design + analytic prototype. **Nothing shipped**, and no change made
to `replay/mod.rs` — deliberately, per the scope this was investigated under.
**Affected:** would touch `src/replay/mod.rs`, and for one of the three designs
also `src/capture` and the stored payload shape.
**Found / written:** 2026-08-09, motivated by four independently-discovered
defects sharing one failure shape.
**Severity of the gap being addressed: HIGH.** Four separate bugs this project
each ended with replay reporting success while doing the wrong thing. Nothing in
the pipeline ever asks whether a replayed action produced what the recording
expected.

> **Naming note.** This file keeps the name it was requested under, but the
> single highest-value check turned out to be a **pre**-execution one. Read
> "verification" here as covering both.

## The motivating pattern

| Bug | Doc |
|---|---|
| Window-switch misattribution | `type-action-misattributed-after-window-switch.md` |
| Multi-line duplication | `multiline-document-capture-duplicates.md` |
| Selector ambiguity | `replay-window-selector-ambiguity.md` |
| Substring name matching | `selector-matching-precision.md` |

Four different mechanisms, one signature: **wrong target or wrong content,
reported as success.** Each was invisible until a human noticed bad output.

## The distinction that decides everything

A verifier can ask two very different questions, and they are not
interchangeable:

* **Design A — did replay do what the recording *said*?** Compare the effect of
  the action against its own payload. For typing: `after == before + payload`.
* **Design B — did the world end up as it did when the recording was *made*?**
  Compare the resulting state against state captured at record time.

**A cannot see a wrong instruction.** If the recording says the wrong thing,
replay faithfully executes it and A confirms the faithful execution. Three of
the four bugs are exactly that case.

A third, cheaper check sits before either:

* **Target check — is the element we resolved the one that was recorded?**
  Compare the resolved element's name against the recorded name for *equality*,
  where matching is currently *containment*.

## Prototype results, against real recorded values

Implemented as pure predicates and run against the values actually recorded when
each bug was live (`text_capture_probe -- verify`). Pure functions rather than a
live replay, because the pre-fix behaviour is already captured in the docs and
re-creating it would add risk without adding evidence.

```
  bug                              A(action)  B(outcome)  B+context  target
  ----------------------------------------------------------------------------
  multi-line duplication           no         YES         YES        no
  window-switch misattribution     no         no          YES        no
  substring selector collision     no         no          no         YES
  selector ambiguity               no         no          no         no
```

### Design A catches nothing — 0 of 4

This is the result worth leading with, because A is the intuitive design and the
one this investigation was pointed at first.

Multi-line duplication, using the real pre-fix payloads:

```
step 1: before ""           payload "alpha line"             after "alpha line"                     -> A passes
step 2: before "alpha line" payload "alpha line\nbeta line"  after "alpha linealpha line\nbeta line" -> A passes
```

A passes both. Replay appended precisely what the payload said; the payload was
the thing that was wrong. **A verifies replay's obedience, and obedience was
never the problem.**

### Design B catches the multi-line duplication

The recording's own field ended holding `"alpha line\nbeta line"`. Replay
produces `"alpha linealpha line\nbeta line"`. Comparing against the recorded
end-state flags it immediately.

### Only recorded *context* catches the window-switch misattribution

Here both content checks pass — the right text really did reach the right field:

```
type step  A: passes
type step  B (content only): passes
type step  B (with foreground context): FLAGS IT  ("msedge.exe" at capture vs "Calculator" at replay)
```

The defect was *where* the step happened, not *what* it wrote. Content
verification of any kind is blind to it. Only comparing the surrounding
context — which application was in front — distinguishes right from wrong.

### The target check catches the substring collision, and prevents it

```
recorded "Paradigm"
resolved "Paradigm Text Capture Probe and 63 more pages - Personal - Microsoft Edge"
-> FLAGS IT
```

Cheapest of the three, needs no new stored data, and is the only one that
**prevents the wrong action instead of reporting it afterwards.**

### Nothing catches selector ambiguity

Two windows genuinely share the name, so the resolved element's name *equals* the
recorded one and every subsequent content check succeeds — against the wrong
window. Detecting it needs a candidate count, which the library does not expose;
that was established separately and remains true.

## Tradeoffs, honestly

### Design B has a cost that is not just engineering

Recording end-state means **capture stores more of the user's text than it does
today**. The redaction policy exists precisely because captured payloads can be
sensitive, and an end-state snapshot of a password field, a message body, or a
spreadsheet cell is the same class of content. Any implementation of B has to
route recorded state through the same redaction gate — and a redacted end-state
cannot be compared, so B silently loses coverage on exactly the fields where
being wrong matters most.

That is a real design tension, not a detail to settle later.

### Where to verify: every step, or some

Every step is the simple answer and probably the right one. The measured cost of
a UIA read is single-digit milliseconds (4–7 ms for `focused_element`, similar
for `text`), so before+after reads add perhaps 10–15 ms per step against steps
that already take hundreds. Selective verification would mean deciding which
steps are "risky", and three of the four bugs above looked unremarkable.

### What to do on failure

The redaction halt (Step 6) is the precedent: it aborts, because continuing past
a step that should not have run compounds the damage. The same reasoning applies
unevenly here:

* **Target check fails** — abort, clearly. Nothing has happened yet and we know
  we are about to act on the wrong element. This is the redaction-halt case
  exactly.
* **Outcome check fails** — the action has already happened. Aborting cannot
  undo it, but it does stop the remaining steps from compounding a run that has
  demonstrably diverged. Abort still looks right, with the failure recorded as
  its own outcome kind so it is distinguishable from "not found".
* **Either check is wrong** — a false positive aborts a working replay. This is
  the same over-cautious-rejection risk that sank the ambiguity fix, and it is
  the reason to prefer the target check first: it is a simple equality on data
  already in hand, with far less to go wrong than a state comparison.

## Honest assessment

**What this would have prevented:** 3 of the 4 motivating bugs, and only with
all three checks. No single check covers more than one.

**What it would not:** selector ambiguity. Also unaddressed is anything where the
recording is wrong *and* the world ends up matching it — a faithfully recorded
mistake.

**What the prototype does not establish.** It is analytic. The predicates were
run against recorded values, not against a live replay with verification wired
in. It shows the checks *would* have flagged these cases; it does not show what
false-positive rate they carry in normal use, which is the number that decides
whether this is shippable. On the evidence of this project, that rate is the
thing most likely to sink it.

**The strongest single recommendation:** implement the **target check** alone
first. It catches a confirmed production bug, needs no schema change, no stored
state, no redaction interaction, and prevents rather than reports. It is a small,
boundable change — unlike Design B, which touches capture, storage, redaction and
replay together, and which this project's history suggests would not land in one
attempt.

## Next steps

- [ ] **Prototype the target check against a live replay** and measure its false
      positive rate on playbooks that currently work. That number decides
      everything; without it this is a design, not a plan.
- [ ] **Decide whether exact name equality is too strict**, given titles that
      carry volatile suffixes (`*Untitled - Notepad`). Anchoring — the recorded
      name matching the whole name or its start — may be the usable middle.
      Overlaps with `selector-matching-precision.md`.
- [ ] **Settle the redaction interaction before building Design B.** If recorded
      end-state must be redacted for sensitive fields, B cannot verify those
      fields at all, and it is worth knowing that before paying for the
      implementation.
- [ ] **Do not treat 3-of-4 as the coverage of a shipped feature.** It is the
      coverage of three separate checks against four bugs already understood.
      Bugs not yet found are not represented, and the checks were designed with
      these four in view.
