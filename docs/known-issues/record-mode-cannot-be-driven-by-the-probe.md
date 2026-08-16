# Record Mode cannot be started by the probe

**Status:** open, testing limitation only — no product defect.
**Found:** 2026-08-16, while trying to verify the `set-focus` permission fix.

## What happens

`text_capture_probe` can click **Start recording** in the real app, and the
click lands — `click_app_button` returns cleanly. No recording starts, no badge
window appears, and the screen still shows "Start recording".

The cause is the accessibility permission gate. `toggle` routes a start through
`requestPermissionGatedAction(start)`, and the gate does not release for a
probe-driven click the way it does for a human one. Every other Record Mode
path is reachable from a probe; starting is not.

## Why it matters

It blocks automated verification of anything downstream of a live recording,
which is a real slice of the app:

* the recording badge window existing at all;
* `openRecordingBadgeWindow`'s `existing` branch, and therefore `setFocus`;
* discard-then-restart, and any other sequence beginning with a real recording.

Those have to be verified by hand, and a commit claiming otherwise would be
claiming more than it measured.

## A path that looks equivalent and is not

`openProofWindow` has the same shape as the badge opener —

```js
const existing = await WebviewWindow.getByLabel(label);
if (existing) { await existing.setFocus(); return; }
```

— and it has a plain button, so clicking it twice looks like a way to exercise
`setFocus` without a recording. It is not. Its label is
`` `proof-${Date.now()}` ``, generated fresh on every call, so `getByLabel`
never finds one and the `existing` branch is unreachable by construction. Two
clicks produce two windows and prove nothing.

Recorded so the substitution is not attempted again.

## The set-focus fix, and how it was actually confirmed

`c96afec` granted `core:window:allow-set-focus`, closing a gap that had existed
since `f855e24` (Step 11a) — the same commit that introduced `setFocus` and last
touched the capabilities file, without ever granting it.

That commit flagged one thing it could not do: reproduce the failure live, for
the reason above. It rested on the code trace, the config diff, and
`allow-set-focus` being present in `gen/schemas/capabilities.json` after a
rebuild.

**That gap is now closed by direct user verification.** The user ran the exact
reported sequence — discard a recording, then start a new one — through the real
app and confirmed it completes cleanly with no permission error. So the fix is
verified end to end; only the *automation* of that verification remains
unavailable.

## If it is ever worth automating

The gate is the thing to look at, not the click. `useAccessibilityPermissionGate`
decides whether `start` runs, and a probe would need it to resolve the way it
does for a real user. Whether that is a test hook, a real permission grant, or
something else has not been investigated.
