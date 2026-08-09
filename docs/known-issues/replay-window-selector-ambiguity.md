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

## Refuted approach: window identity continuity (2026-08-08)

The first fix attempted was the obvious one: give capture and replay a shared
notion of *which window* a step belongs to, so replay could resolve a selector
once and reuse the concrete window for later steps meaning the same one. It was
**abandoned before implementation**, because the identity signal it depends on
does not exist reliably. Recorded here so it is not attempted again blind.

### Design flaw found before any code

The proposal was a counter incrementing on each switch to a different window.
That cannot express the case it exists for. Walking the actual scenario through
it:

| Event | Counter |
|---|---|
| switch → Notepad | 1 |
| type | 1 |
| switch → Paradigm | 2 |
| switch **back to Notepad** | **3** |

The two visits to one window get different ids, so nothing is shared and the
cache never helps. It would have to be an identity → id *map*, incrementing only
for newly-seen windows. That in turn requires a stable window identity — which
is where it failed.

### What identity signals actually exist

`ApplicationSwitchEvent` carries no window handle at all; `grep -rni "hwnd"`
across `terminator-workflow-recorder` returns nothing. Its identity fields are
the window title, `to_process_name`, and `to_process_id`. A handle is reachable
indirectly through `metadata.ui_element` via
`UIElement::get_native_window_handle()`.

The title is disqualified by construction — it changing mid-recording is the
defect. That leaves the handle and the pid, both measured with
`examples/text_capture_probe -- windowid`, which types into a page that renames
its own window (reproducing Notepad's behaviour without touching Notepad),
switches to Calculator, and switches back:

| Switch | pid | hwnd | title |
|---|---|---|---|
| probe window | 2848 | **0** | `Untitled - Probe …` |
| Calculator | 15636 | 0x2903c8 | `Calculator` |
| probe window | 2848 | **0** | `draft note - Probe …` |
| Calculator | **4248** | **0** | `Calculator` |

**HWND is unusable.** It resolved to **0 for every browser window** — 1 usable
handle in 4 switches. Worse, the failure is `Ok(0)`, not `Err`, so code trusting
it would accept 0 as a legitimate key and merge every unresolvable window into
one identity. Browsers being the failing case matters especially: the other open
findings in this session (Sheets, Gmail) all live there.

**pid is stable for this bug's scenario but not in general.** It held at 2848
across the title change, which is exactly the condition that broke replay. But
Calculator reported **two different pids for the same visible application**
within one run — 15636 then 4248, likely a UWP hosting artifact
(`ApplicationFrameHost` versus the app process), though that root cause is
unconfirmed. Keying on pid would split such a window into two sessions.

Neither signal is reliable across target types, so "prove two selectors mean the
same window" has nothing dependable to stand on.

### A false positive this probe produced, and the correction

Worth recording because it nearly settled the design on bad evidence. The
probe's first verdict printed **"HWND IS A VALID KEY: stable across the title
change"** — reached by comparing the two browser handles and finding them equal.
Both were **0**. Two zeroes are trivially equal and carry no information.

Taken at face value, the fix would have been built on a key that is null for
every browser window. The verdict now excludes zero handles and reports `n/a`
rather than agreement. The general lesson is the one this session keeps
relearning: a diagnostic that cannot distinguish "no signal" from "matching
signal" will eventually assert the wrong thing confidently.

## Blocked approach: fail loud on ambiguity (2026-08-08)

After window-identity caching was abandoned, the simpler design was to stop
trying to prove two selectors mean the same window, and instead detect — at the
moment a single step resolves — whether that resolution was unambiguous *right
now*. More than one candidate would be a hard failure naming the selector and
the count, rather than a silent pick.

This design is still believed sound. It is **blocked on a mechanism**, not on
the idea.

### `.all()` refuses the selectors capture produces

The obvious counting path is `Locator::all()` before `Locator::first()`.
Measured with `examples/text_capture_probe -- ambiguity`, which opens one page in
two browser windows so a single title genuinely matches two:

```
selector "role:Window|name:Ambiguity Probe Window"
  .first() -> Ok   name=Some("Ambiguity Probe Window - Personal - Microsoft Edge")
  .all()   -> Err  Invalid selector: Desktop-wide search not allowed.
                   Selector must include 'process:' prefix to scope search to a
                   specific application.
```

The two methods have **opposite policies on the same selector**: `first()`
permits a desktop-wide search, `all()` rejects one. The same result held for
`role:Edit|name:AmbigField` and for a bare `role:Window`.

Every selector capture stores for a navigate step is desktop-wide
(`role:Window|name:…`), so `.all()` cannot count them as they stand.

### Why a `process:` prefix is not already available

The error suggests scoping by process. Capture does not record one. For a
navigate step the payload's `app` field holds the **window title**, because
`source_app` is the first non-empty identifier and `to_candidate` puts
`to_window_and_application_name` first. Confirmed in real probe output:
`source_app="Calculator"`, and
`source_app="Paradigm Text Capture Probe and 78 more pages - Personal - Microsoft Edge"`.
Both titles.

`ApplicationSwitchEvent::to_process_name` exists and is discarded today.

### The two remaining routes, scoped

**Route 1 — carry a process name through the pipeline.** Record
`to_process_name` at capture, thread it through `ActionCandidate` →
`CapturedAction` → the compiled payload, and count with
`process:<name>|role:Window|name:<title>`.

*Risks:* touches four layers and the stored payload shape, so old playbooks
have no process name and need a defined fallback — probably "cannot verify,
proceed as today", which leaves them exactly as unsafe as now. Process name is
also not unique (several browser windows share `msedge.exe`), so it narrows the
search without guaranteeing the count is meaningful. It does not help where the
process itself is ambiguous.

**Route 2 — count via `desktop.applications()` and match by hand.** Enumerate
top-level applications and apply the selector's role/name criteria manually,
avoiding any capture change.

*Risks:* re-implements selector matching outside the library that owns it. Any
divergence from what `first()` actually matches makes the check wrong in one of
two directions — missing real ambiguity, or **rejecting legitimate unambiguous
replays**, which is the worse failure because it breaks working playbooks. It
also needs verifying that `applications()` enumerates reliably, which has not
been tested and which `.all()`'s behaviour gives reason to doubt.

Neither is a small change, and both need their own evidence before being
committed to.

## Pattern: a swallowed error produces a confident false conclusion

Three times in one session, and worth naming because the shape repeats and each
instance cost real investigation time.

1. **The empty candidate list.** A probe reported `focused_element() calls: 0`
   and I inferred that `Desktop::new_default()` fails inside a spawned task. It
   does not. The count was zeroed by the probe calling `pump.abort()` before
   `await`, so an aborted `JoinHandle` returned `Cancelled` and silently
   discarded every timing collected. Recorded in
   `text-input-capture-truncation.md`.
2. **The zero handle.** The `windowid` probe compared two browser window handles,
   found them equal, and printed **"HWND IS A VALID KEY"**. Both were `0` —
   `get_native_window_handle` returns `Ok(0)` rather than an error for windows it
   cannot resolve. Recorded above.
3. **The rejected selector.** `locator("role:Window").all()` was reported during
   Notepad scoping work as "returning zero Notepad windows". It was returning
   `Err`, swallowed by `.ok()`. That wrong inference stood for hours and shaped
   two subsequent scoping attempts.

The common shape: **an API reports "nothing" and "refused" through the same
channel, or a diagnostic cannot tell "no signal" from "matching signal", and the
absence is read as data.** Every instance produced a confident, wrong conclusion
that survived until something forced a re-check.

The practical rule for anyone working in this area: when a probe reports zero,
empty, or equal, prove the call *succeeded* before drawing anything from the
value. `.ok()` and `unwrap_or_default()` in diagnostic code are where these
originate.

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

- [x] ~~**Give a replay run a notion of target identity.**~~ Attempted and
      abandoned — see "Refuted approach" above. No reliable window identity
      signal exists: HWND is 0 for browser windows, and pid is unstable for at
      least one app class.
- [ ] **Unblock the fail-loud check by picking Route 1 or Route 2** (see
      "Blocked approach" above). This is the recommended direction; only the
      candidate-counting mechanism is missing.
- [ ] **Prefer identifiers that do not change with content.** A window title that
      mutates as the user types is a poor key. Process id continuity, or a
      window handle held for the run, would be stable across exactly the change
      that broke this. Note that process id has its own limits — Windows 11
      Notepad hands new launches to an existing instance, measured while
      investigating the multi-line duplication.
- [ ] **Consider refusing to act on an ambiguous selector.** Still the right
      idea, and now blocked only on how to count candidates — see "Blocked
      approach". Failing loudly would have turned this run into an honest
      failure instead of a false success.
- [ ] **Reproduce it in a probe.** This is recorded from a live session and read
      out of the code; it has not been driven deterministically the way the
      capture defects were. A probe would confirm the mechanism and give any fix
      something to verify against.
