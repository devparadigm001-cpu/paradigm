# Replay resolves each window selector independently, so steps drift between windows

**Status:** confirmed from a real recording and replay. Root cause identified in
the replay code. **Not fixed.**
**Affected:** `src-tauri/src/replay/mod.rs` — `navigate`, and target resolution
generally. Any playbook whose steps address a window by a generic,
content-dependent title.
**Platform:** Windows. Observed with Notepad; nothing about the mechanism is
Notepad-specific.
**Found:** 2026-08-08, during Step 12 testing (session
`record-07aff175-a055-4d79-836b-119820f3ad0d`, replay run
`3abcb8a5-6c7e-445c-8cbf-f5978e6d8070`).
**Severity: HIGH.** Not because the run failed — because it didn't. Replay
reported **Completed, 12/12 steps succeeded** while typing into the wrong window
and duplicating content. This is the third instance this session of the same
failure class: silent, plausible, self-consistent, and reported as success.

## Summary

Every step resolves its own target from scratch. Two steps that the recording
intended to mean *the same window* can therefore land on *different* windows,
because nothing in a replay run remembers which concrete window a selector
resolved to earlier.

The recording was correct. The capture ordering was correct — including the
mid-recording switch to Paradigm, which the window-switch fix earlier the same
day handled properly. What went wrong happened entirely at replay time.

## Evidence

The user recorded typing two lines into an existing `Untitled - Notepad` window,
with a window switch in between.

| Step | Selector | Resolved to |
|---|---|---|
| 1 (navigate) | `role:Window\|name:"Untitled - Notepad"` | a **new, blank** Notepad window — not the recording's |
| 4 (type) | — | typed the first line into that **wrong** window |
| 10 (navigate) | `role:Window\|name:"*draft note for testing - Notepad"` | the **original** window — correctly |
| 11 (type) | — | typed the full accumulated payload into the original window, which already held content |

Result: `"draft note for testing"` appears **twice**, and the first line went to a
window that had nothing to do with the task. Status: **Completed, 12/12
succeeded.**

### Why the two selectors differ, and why that is nobody's fault

This is the part worth understanding, because it is not a capture defect.

At the moment step 1 was recorded, the window genuinely *was* called
`Untitled - Notepad` — no text existed in it yet. By the time step 10 was
recorded, the user had typed, so Notepad had retitled the window to
`*draft note for testing - Notepad`. Capture recorded what was true at each
moment. Both selectors are accurate records.

The trouble is that **a Notepad window's title is a function of its contents**,
so the same window has different names at different points in one recording, and
a blank one has a name that any other blank one also has.

## Root cause

`replay::navigate` receives only the desktop and the step's payload:

```rust
async fn navigate(desktop: &Desktop, payload: &StepPayload, mk: ...) -> StepOutcome {
    if let Some(selector) = payload.selector.as_deref() {
        match desktop.locator(selector).first(Some(LOCATE_TIMEOUT)).await {
```

There is no run-scoped state of any kind — a search of `replay/mod.rs` for a
cache, map, or resolved-target structure finds nothing. Every step performs a
fresh, independent lookup.

Two consequences follow directly:

1. **A generic selector can match a window the run created itself.** Replay
   activating `Untitled - Notepad` may find, or cause, a blank window that is not
   the one the recording meant.
2. **Nothing ties step 1's window to step 10's window.** They are separate
   lookups producing separate results, and the playbook has no way to express
   "the same window as before".

## How this differs from the window-switch misattribution bug

Worth stating plainly, because the two look similar and are filed next to each
other.

`type-action-misattributed-after-window-switch.md` was a **capture-time** defect:
the flush happened too late, so a `type` action was recorded carrying the wrong
window's element. The recording itself was wrong.

This is a **replay-time** defect. The recording is correct — right actions, right
order, right selectors for the moments they were taken. Replay then fails to keep
those correct steps pointing at a consistent set of windows.

The window-switch fix was working correctly in this very session; it is not
implicated.

## Why it matters

The failure is invisible from every vantage point a user has. The run reports
success. Each step reports success, truthfully — a window *was* activated, text
*was* typed. The playbook validates, because a navigate followed by a type is
entirely ordinary. Only the identity of the window differs from what was meant,
and nothing records that intent.

The consequence is content written into the wrong document and duplicated into
the right one. For a product whose purpose is repetitive data entry, writing to
the wrong target while reporting success is close to the worst available outcome:
it is the one failure a user cannot catch by reading the run report.

It is also not an exotic setup. Any application that titles its windows after
their contents — editors, browsers, mail clients, spreadsheets — produces
recordings with this shape, and any workflow that opens a fresh document has a
generic-titled window in step 1.

## Next steps

- [ ] **Give a replay run a notion of target identity.** Resolve a window
      selector once and reuse the concrete window for later steps that mean the
      same one, instead of re-resolving independently. The open design question
      is how a playbook expresses "same window as step 1" when the two steps were
      captured with different titles — which likely means capture must record the
      linkage, not just the selectors.
- [ ] **Prefer identifiers that do not change with content.** A window title that
      mutates as the user types is a poor key. Process id continuity, or a
      window handle held for the run, would be stable across exactly the change
      that broke this. Note that process id has its own limits — Windows 11
      Notepad hands new launches to an existing instance, measured while
      investigating the multi-line duplication.
- [ ] **Consider refusing to act on an ambiguous selector.** If a generic window
      selector matches more than one candidate at replay time, failing loudly is
      better than silently choosing one. That would have turned this run into an
      honest failure instead of a false success.
- [ ] **Reproduce it in a probe.** This is recorded from a live session and read
      out of the code; it has not been driven deterministically the way the
      capture defects were. A probe would confirm the mechanism and give any fix
      something to verify against.
