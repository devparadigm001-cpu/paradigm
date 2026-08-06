# Record Mode captures any app's activity, not just the one being recorded

**Status:** confirmed, working as built — this is a product gap, not a bug.
**Affected:** Record Mode capture generally (`src-tauri/src/capture`), all
platforms.
**Found:** 2026-08-05, during Phase 1 Step 11a's live proof test.
**Severity: LOW.** Not blocking anything. Worth a later-pass decision.

## Summary

`start_record_session()` hooks OS-level input and `ApplicationSwitch` events
system-wide. It has no concept of "the app the user meant to record" — every
click, keystroke, and window switch on the machine is captured for the
duration of the session, regardless of which window is on top.

## What was observed

During Step 11a's proof, a real recording session picked up a genuine click
on Cursor's own chat panel UI — not the app under test — and stored it as a
real playbook step:

```
playbook_id: 1ef92b5f-c321-40fe-af61-eb28b252a7e1
step_order:  1
action_type: click
target.name: "Ran Enumerate top-level windows to find the badge window"
app:         Cursor.exe
```

Confirmed by reading the stored `playbook_steps` row directly, not just from
the review screen. The capture pipeline did exactly what it's designed to
do — this click genuinely happened while the session was active. The gap is
that nothing scoped capture to the app the user actually cared about.

## Why it matters

Nothing currently stops a user's own incidental interaction with unrelated
tooling (their IDE, a terminal, a chat app, etc.) from silently becoming a
step in a recorded workflow if it happens mid-session. The review screen
lets a user delete such steps after the fact (and Step 11a's proof did
exactly that for an unrelated notification-click step), but there is no way
to prevent it up front, and a user who doesn't scrutinize every captured
step could ship a playbook with unrelated noise baked in.

## Possible later-pass directions

- [ ] A way to pause/resume capture without stopping the session.
- [ ] Default-exclude the app's own window (and maybe common dev tools) from
      capture, on top of the existing `ExclusionList` mechanism.
- [ ] Optionally scope a session to a single target app/window at start time.
