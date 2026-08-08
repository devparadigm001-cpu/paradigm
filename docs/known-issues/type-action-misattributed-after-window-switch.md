# Typing was attributed to the wrong window after an application switch

**Status: FIXED** (2026-08-08). Found and fixed the same day, so this was never
open as a tracked issue — it is filed as a record of the defect, the evidence,
and the fix.
**Affected:** `src-tauri/src/capture/mod.rs` — `observe_text`. Any session where
the user typed into a field and then switched applications without first
pressing Enter or clicking elsewhere in the original window.
**Found:** 2026-08-08, during Step 12 testing (session
`record-403c9787-3859-4c0d-896b-94c9979cdd5d`).
**Severity: was HIGH.** Not because it broke a run, but because it did not:
replay reported **7/7 succeeded** while typing into the wrong application. A
loud failure would have been safer.

## Summary

`observe_text` handled `Click` and `Keyboard` events and let everything else
fall through to `None`. `WorkflowEvent::ApplicationSwitch` was in that
"everything else", so **a window switch never reached the text watcher**.

The watch therefore survived the switch. `to_candidate` recorded the switch as
a `navigate` action, and the still-open watch was flushed later by some
unrelated event — emitting a `type` action that carried the **old window's**
element while sitting **after** the navigate in the sequence.

Replayed in that order, capture navigates to the new application and then types
into the previous one.

## Evidence

### The real session

`record-403c9787-3859-4c0d-896b-94c9979cdd5d`: the user typed three lines into
Notepad, then switched to Google Docs before the Notepad field had flushed. The
resulting `type` action was stored with Notepad's selector —
`role:document|name:"Text editor"` — despite appearing after the
navigate-to-Google-Docs step.

Replay `6c91a66a-225b-45d9-977c-411513ac8806` finished with status **Completed,
7/7 succeeded**, and typed the payload back into Notepad.

### Reproduced deterministically

`cargo run --example text_capture_probe -- windowswitch` drives the same shape:
type into a browser field, then switch applications with **no Enter and no click
elsewhere** — nothing that would flush first. Calculator is the second window
because it has no text fields, so nothing can be typed into it by accident.

Before the fix:

```
  [1] click     role=edit    name="FieldA"      source_app="msedge.exe"
  [2] navigate  role=Window  name="Calculator"  source_app="Calculator"
  [3] type      role=edit    name="FieldA"      source_app="msedge.exe"
      payload="switchtest0123456789"
```

After:

```
  [1] click     role=edit    name="FieldA"      source_app="msedge.exe"
  [2] type      role=edit    name="FieldA"      source_app="msedge.exe"
      payload="switchtest0123456789"
  [3] navigate  role=Window  name="Calculator"  source_app="Calculator"
```

## The fix

`observe_text` now flushes the active watch on `ApplicationSwitch`:

```rust
WorkflowEvent::ApplicationSwitch(e) => {
    watcher.flush(e.metadata.timestamp.unwrap_or_else(now_ms))
}
```

Correct ordering falls out of the existing pump rather than needing new
machinery: it already admits `observe_text`'s candidate before the event's own
candidate, so the typing lands ahead of the navigate — the order it actually
happened in.

This is the same shape as `stop_session`'s existing end-of-session flush: a
signal that the field will get no further input, so record it now with the
context it was typed in.

## Relationship to the no-settle race

Related but distinct, and worth keeping separate when reading
`text-input-capture-truncation.md`.

Both are "the flush happens too late, carrying stale context". The no-settle
race is about **typing speed** relative to flush timing — the baseline is read
after the text lands. This is about a **window switch** arriving before any
flush, so the flush happens at the right moment for the *watcher* but the wrong
moment for the *user's intent*.

Fixing one does not fix the other; they were fixed separately.

## Verification

| Check | Result |
|---|---|
| Window-switch scenario, before | typing after the switch, old window's element — **reproduced** |
| Window-switch scenario, after | typing before the switch, correct element — **correct** |
| A–E web trials, 6 runs | **A–D 12/12**; no fabricated action for the pre-filled field |
| Multi-line delta fix, click-away | 2 actions, replay matches exactly |
| Multi-line delta fix, Tab | 2 actions, replay matches exactly |
| Suite | 76 lib, 8 db_encryption, 12 ipc_commands, 2 replay_aborted pass |
| Clippy | no warnings in changed files |

### A note on trial E, for whoever next works on the no-settle race

E — the no-settle trial — scored **0/9 in this session**, against the **4/10**
recorded in `text-input-capture-truncation.md`.

**This fix is not responsible.** A direct control isolating it measured E at
**0/3 with the fix removed** and **0/6 with it applied** — identical. The delta
fix in the same area cannot affect E either: every code path for E's shape emits
the whole value, because the delta branch requires a `baseline_trusted` baseline
that only a post-flush re-watch sets, and E never gets one.

The most likely explanation is environmental and is offered as a lead rather
than a finding: the probe opens a browser tab per run, and roughly 25 runs
during this session grew the browser from about 67 tabs to 92. E is the only
trial with no settle, so its margin is set by how quickly the click event is
processed, and a larger UIA tree makes that slower. That is correlational, not
established — a clean browser would test it in one run.

The practical point: **the 4/10 figure is environment-sensitive** and should not
be treated as a fixed property of the defect.

## Why it matters

The failure mode is the dangerous kind this project keeps meeting: silent,
plausible, and self-consistent. Every individual step looks right — a real
navigate, a real type action with a real selector and the correct text. Only the
*pairing* of the two is wrong, and nothing in validation can detect it, because
a playbook that navigates and then types is entirely ordinary.

Replay reporting **7/7 succeeded** while writing into the wrong application is
worse than a failure would have been. A user reviewing that run has no signal
that anything went wrong.

Switching applications mid-task is also not an edge case. It is the normal shape
of the work this product automates — copy from one place, paste into another.

## Next steps

- [ ] **Consider whether other events imply focus loss.** Window minimise,
      window close, and session-level focus changes are not currently treated as
      flush triggers. `ApplicationSwitch` was the one with evidence behind it;
      the others are untested rather than known-good.
- [ ] **Consider asserting window consistency at compile time.** A `type` step
      whose target belongs to a different application than the preceding
      `navigate` is suspicious, and cheap to detect once. That would have caught
      this defect as a validation error rather than a silent wrong write.
