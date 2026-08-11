# Replay resolves each window selector independently, so steps drift between windows

**Status: FIXED, 2026-08-10.** Replay now refuses any step whose selector matches
more than one element carrying the recorded name, as
`StepResult::FailedAmbiguous`, at both resolution sites. Verified end to end
through the real compile → store → replay pipeline — see **"The fix"**. Two
earlier attempts were built and refuted before this one worked; their
post-mortems are kept below because they explain why the shipped design looks
the way it does.
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

## A compounding defect, filed separately

Selector names match by **substring**, not exactly — `contains_name` in
`terminator-rs`. So a selector can resolve onto an element that was never the
target, even when only one thing matches. That is distinct from this bug, which
is about several windows matching and replay picking one silently, but the two
compound: substring matching enlarges the candidate pool that makes ambiguity
likely in the first place. See `selector-matching-precision.md`.

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

## Route 1 built, and refuted at the last layer (2026-08-09)

Route 1 was implemented in full. **Layers 1–3 work and are kept. Layer 4 — the
ambiguity detection the whole route existed for — does not, and was removed.**

### Layers 1–3: verified, kept

Each layer was verified before the next was built, because the assumption that
started this ("`identifiers` already carries the process name") turned out to be
false: `admit` keeps only the first non-empty identifier as `source_app` and
drops the rest, and `compile` never sees `identifiers` at all. A new field was
required on both `ActionCandidate` and `CapturedAction`.

**Layer 1 — capture records it.** A real driven session
(`text_capture_probe -- procname`):

```
  kind      process_name                     source_app (for contrast)
  navigate  Some("msedge.exe")               "Paradigm Text Capture Probe and 63 more pages … Edge"
  click     Some("msedge.exe")               "msedge.exe"
  type      Some("msedge.exe")               "msedge.exe"
  navigate  Some("ApplicationFrameHost.exe") "Calculator"

  actions carrying a process_name : 4/4
  navigate actions with one       : 2/2
```

The contrast is the point: the navigate action's `source_app` is the long,
mutable window title while `process_name` is the stable `msedge.exe`.

Note Calculator resolves to `ApplicationFrameHost.exe` — the UWP frame host,
shared by *every* UWP app. Scoping to it would scope to all of them. That is a
real limit on how much a process name can ever narrow things, and it matches the
pid instability measured for Calculator in the refuted identity work above.

**Layer 2 — it survives compile → store → load.** Two tests in
`compile/store.rs` against a real encrypted database:
`the_process_name_survives_compile_store_and_load` asserts `"process"` reads
back as `"notepad.exe"` while `"app"` is still the title, and
`a_missing_process_name_stores_as_null` asserts an action without one stores
`null` rather than failing or inventing a value.

**Layer 3 — `StepPayload::scoped_selector()` builds the prefix.** Four unit
tests, including `an_old_playbook_without_a_process_name_cannot_be_scoped`,
which pins the strictly-additive requirement: a playbook recorded before this
field existed returns `None`, keeps its original unscoped selector, and replays
exactly as it always did.

### Layer 4: the count answers a different question

Supplying a `process:` prefix *does* stop `Locator::all()` rejecting the
selector — that specific blocker is gone. But the number it returns is not a
count of the selector's matches.

Measured with two windows sharing a title, in a browser holding three windows
total:

```
selector "process:msedge.exe|role:Window|name:Ambiguity Probe Window"
  .all() -> 3 candidate(s)
      [0] Window "Ambiguity Probe Window - Personal - Microsoft Edge"
      [1] Window "Ambiguity Probe Window - Personal - Microsoft Edge"
      [2] Window "Paradigm Text Capture Probe and 63 more pages …"   <-- name does not match

selector "process:msedge.exe|role:Edit|name:AmbigField"
  .all() -> 3 candidate(s)
      [0] role="Window" …                                            <-- asked for Edit, got Window
```

**Two entirely different selectors returned the same three elements.** With a
`process:` prefix, `all()` returns every top-level window of that process and
ignores the role and name criteria.

So the count cannot answer "does this selector match more than one window". It
answers "how many windows does this process have", which for a browser is
routinely three or more. Wiring the check to it would have failed legitimate,
unambiguous replays — the exact over-cautious-rejection risk flagged against
Route 2, which turns out to afflict Route 1 too.

The check and its `count_candidates` helper were removed rather than shipped.

### What is kept, and why

`process_name` now flows capture → compile → storage correctly, and
`scoped_selector()` builds the prefix correctly including the empty-string and
missing-selector edge cases. Both are marked `#[allow(dead_code)]` and remain
tested.

They are kept because **any** future approach needs a stable process identity —
including one that never calls `Locator::all()`. Rebuilding that plumbing is the
expensive part; the three-line counting call that failed is not.

## Route 3: count from a root element instead of a `process:` prefix (2026-08-10)

**Status: survives the decisive test. The first counting mechanism of the three
that works.** Root-scoped counting *does* inflate on containment, exactly as
feared — but filtering candidates through the already-shipped
`resolved_is_recorded_target` removes the containment-only match while still
reporting genuine ambiguity. Measured three separate runs, nine rounds, no
variation. **Still not implemented in `replay/mod.rs`** — see "What is still not
established" below for what an implementation would have to settle first.

### What the library source actually says

`.all()` rejecting desktop-wide selectors is **not a UI Automation limit**. It
is a deliberate guard in `terminator-rs`, `platforms/windows/engine.rs:1006`:

```rust
// Enforce scoping: desktop-wide search requires process selector when root is None
if root.is_none() && !selector_has_process_scope(selector) {
    return Err(AutomationError::InvalidSelector(format!(
        "Desktop-wide search not allowed. Selector must include 'process:' prefix …
         Or use element.locator() to search within a specific element's tree.
```

The condition is `root.is_none() && !has_process_scope`, so there are **two**
ways to satisfy it, and the error message names both. Route 1 took the
`process:` prefix, which parses to a `Selector::Chain` and is handled by the
chain arm — that arm applies each link as a *descendant* search, which is why
its count came back as "every top-level window of the process" and ignored role
and name. Supplying a **root element** instead (`Locator::within()`, and
`Desktop::root()` is public) skips the chain and reaches `Selector::Role`, whose
matcher does apply `control_type` and `contains_name`.

That is a different code path, not a re-run of Route 1.

### The measurement

`text_capture_probe -- rootcount` opens two browser windows sharing a title and
one with a unique title, then counts via `within(desktop.root())`. Verbatim:

```
selector "role:Window|name:RootCount Duplicated"
  expected: 2  (genuine ambiguity)
  depth 1        -> Err  Element not found: … Err: find element time out   [5044 ms]
  depth 3        -> 2 candidate(s)   [288 ms]
        [0] role="Window" name=Some("RootCount Duplicated - Personal - Microsoft Edge")
        [1] role="Window" name=Some("RootCount Duplicated - Personal - Microsoft Edge")
  depth default  -> 2 candidate(s)   [1733 ms]

selector "role:Window|name:RootCount Unique"
  expected: 1  (must NOT over-reject)
  depth 1        -> Err  Element not found: … Err: find element time out   [5026 ms]
  depth 3        -> 1 candidate(s)   [284 ms]
        [0] role="Window" name=Some("RootCount Unique - Personal - Microsoft Edge")
  depth default  -> 1 candidate(s)   [1849 ms]

selector "role:Window|name:RootCount Absent Window"
  depth 1/3/default -> Err  Element not found …   [~5 s each]

selector "role:Edit|name:RootCountField"
  depth 1        -> Err   [5027 ms]
  depth 3        -> Err   [5194 ms]
  depth default  -> 3 candidate(s)   [1756 ms]

--- desktop.windows_for_application("msedge.exe") ---
  -> 0 window(s)   [34 ms]
```

**2 for the duplicated title and 1 for the unique one** — the discrimination
Route 1 could not produce. Role is respected too: the `Edit` selector returned
three edits, not three windows.

### Incidental findings worth keeping

* **Depth matters and is not uniform.** `depth 1` never works — top-level
  windows sit deeper than one level below the desktop root. `depth 3` suffices
  for windows and is **6× faster** than the default (288 ms vs 1733 ms), but is
  *not* enough for the `Edit` case, which needs the default depth. Any use of
  this must pick a depth per role, and that is a tuning parameter with no
  principled value yet.
* **"No match" is an `Err`, not `Ok(0)`,** and it costs the full timeout (~5 s).
  A counter placed before `first()` would add ~5 s to every genuinely-missing
  element, so it would have to run *after* a successful `first()`.
* **`desktop.windows_for_application()` is a dead end.** It takes an application
  *name*, not a process name, and returned 0 windows for `"msedge.exe"` in 34 ms.
  Internally it resolves one app element via `application(name)` and filters its
  children, so it could not enumerate across processes even if named correctly.
  Candidate 1 of this attempt's brief is closed on this evidence.

### The over-rejection test (2026-08-10, later the same day)

The gap above — "the decisive test was not run" — is now closed.
`text_capture_probe -- decoycount` builds a genuine containment collision and
counts with and without the filter.

**The decoy is a natural shape, not a contrived one.** Browser windows are
titled `<page> - <profile> - <browser>`, so a page titled `Draft X` produces a
window title that *ends with*, and therefore contains, the entire window title
of a page titled `X`. No engineering of the string was needed:

| Window | Full title | Contains the recorded name? |
|---|---|---|
| target | `DecoyCount Invoice - Personal - Microsoft​ Edge` | — (it *is* the recorded name) |
| decoy | `Draft DecoyCount Invoice - Personal - Microsoft​ Edge` | **yes** |
| near-miss | `DecoyCount Invoice Notes - Personal - Microsoft​ Edge` | no |
| twin ×2 | `DecoyCount Twin - Personal - Microsoft​ Edge` | yes, and identical to each other |

The near-miss is the informative control: extending the title on the *trailing*
side does not collide, because the browser's own suffix falls between. Only a
*leading* extension collides. That bounds the real-world exposure — the decoy
has to be a window whose title ends with the recorded title — but does not
remove it.

The filter is the shipped `replay::resolved_is_recorded_target`, called
directly rather than reimplemented, so the test exercises the real predicate.
Its visibility was widened from private to `pub` for this; no behaviour changed.

#### Result, three runs × three rounds

```
  target   raw=[2, 2, 2]  filtered=[1, 1, 1]      run 1
  twin     raw=[2, 2, 2]  filtered=[2, 2, 2]
  target   raw=[2, 2, 2]  filtered=[1, 1, 1]      run 2
  twin     raw=[2, 2, 2]  filtered=[2, 2, 2]
  target   raw=[2, 2, 2]  filtered=[1, 1, 1]      run 3
  twin     raw=[2, 2, 2]  filtered=[2, 2, 2]
```

with the per-candidate decisions printed verbatim:

```
TARGET (must not over-reject)
  raw=2  filtered=1   [164 ms]
      [0] drop "Draft DecoyCount Invoice - Personal - Microsoft​ Edge"
      [1] KEEP "DecoyCount Invoice - Personal - Microsoft​ Edge"
TWIN   (must still catch ambiguity)
  raw=2  filtered=2   [161 ms]
      [0] KEEP "DecoyCount Twin - Personal - Microsoft​ Edge"
      [1] KEEP "DecoyCount Twin - Personal - Microsoft​ Edge"
```

Three things follow, and the first is as important as the other two:

1. **The feared over-rejection is real.** Raw root-scoped counting reports **2
   for a completely unambiguous replay**. Had the check been wired to the raw
   count — which is what "Route 3 looks promising" meant before this test — it
   would have refused a legitimate playbook. Route 3 would have died exactly
   like Routes 1 and 2, and the earlier `rootcount` evidence could not have
   shown it, because those titles were chosen not to collide.
2. **The filter rescues it.** The decoy is dropped, the count falls to 1, and
   the replay proceeds.
3. **The filter does not paper over real ambiguity.** Two genuinely identical
   windows still count 2 after filtering, so the check keeps the ability it
   exists for.

Timing at depth 3 was 153–265 ms across all nine rounds; the default depth cost
775–956 ms for the same answer, reconfirming the earlier depth finding on
independent runs.

#### The run that decided nothing, and why it was allowed to

The first execution of this probe reported a clean pass on every line that
mattered — target filtered=1, twin filtered=2, "filter rescues the unambiguous
case: **true**" — and was **invalid**. Its explicit precondition check said so:

```
  decoy    present=true  contains recorded name=false
  !! PRECONDITION FAILED: no containment collision was built.
```

Edge had opened the target page as a **tab in an existing 40-tab window** rather
than a new one, so the recorded name came back as
`"DecoyCount Invoice and 39 more pages - Personal - Microsoft​ Edge"`. The decoy
title could not contain *that*, so no collision existed and the filter was never
exercised. The "pass" was the trivial one: nothing to reject, so nothing
rejected.

This is the fourth instance of the pattern named at the bottom of this document,
in its "cannot tell no-signal from matching-signal" form — and the first one
caught at the moment it happened rather than hours later, because the probe was
built to prove its own premise before reporting a verdict. **A probe that tests a
mitigation must first prove the thing being mitigated is present.** The fix was a
warm-up window that absorbs Edge's merge-into-an-existing-window behaviour.

That run also caused real collateral damage worth recording: cleanup closed the
matched window, and with it 39 unrelated tabs belonging to the user. The probe
now refuses to close any window whose title says `and N more pages`, since such a
window is shared and only one tab in it is the probe's.

### Candidate 2, untouched

Deferring the ambiguity question to the user — surfacing "this matched two
windows, which did you mean?" instead of deciding silently — has not been
investigated. It depends on detection working first, so it is downstream of the
fix below rather than an alternative to it.

## The fix (2026-08-10)

Route 3 is implemented in `replay/mod.rs` as `resolve_recorded`, called at both
resolution sites: `navigate` and the shared click/type element path. A step whose
selector matches more than one element carrying the recorded name fails as
`StepResult::FailedAmbiguous` instead of acting on a silent pick.

### Search depth: why the default is required, not a shortcut

This was the one open question blocking implementation, recorded above as
"enumeration completeness … a tuning parameter, not a guarantee". It turned out
to be answerable from the library source, and the answer is *derived* rather than
tuned. Reading `terminator-rs` 0.23.35 `platforms/windows/engine.rs`:

**The requirement is not "enumerate every window on the desktop".** It is "cover
everything `first()` could have returned", because the question the check asks is
whether `first()` had more than one candidate to choose from. That reframing is
what makes a guarantee possible at all — absolute completeness is unverifiable,
relative coverage is not.

Three facts establish the coverage:

1. **Both calls start from the same node.** `find_element` (behind `first()`)
   with `root: None` resolves its root via `get_root_element_with_retry()`.
   `Desktop::root()`, which the counter passes to `within()`, returns
   `get_root_element()`. The same desktop node.
2. **Both bottom out in the same matcher.** `Selector::Role` in both
   `find_element` and `find_elements`, with the same `control_type` and
   `contains_name` filters.
3. **The resolver's depth is bounded by 50.** `find_element` computes it with
   `calculate_search_depth(role, name, None, None)`, which returns **5** for a
   *named container* role (`pane`/`window`/`application`) and
   `default_depth.unwrap_or(50)` = **50** otherwise. Because it always passes
   `None`, 50 is the deepest search `find_element` can ever perform.

Meanwhile `should_use_shallow_search` returns false whenever a root is supplied,
so the counter's depth is exactly what we pass: `depth.unwrap_or(50)`. Passing
`None` therefore gives the counter a traversal that is a superset of the
resolver's, for every role. **50 is not a guess — it is the library's own maximum
resolver depth.**

**Why the fast option is unsafe.** Depth 3 measured 6× faster for windows
(153–265 ms vs 775–956 ms) and is what an optimisation pass would reach for. It
is below the resolver's 5 for exactly the named-window selectors this bug is
about. A counter at depth 3 audits a search by examining *less* of the tree than
the search covered, so it could report "one candidate, unambiguous" for a
selector `first()` had two to choose from — the original silent-wrong-window bug,
reintroduced by the check built to prevent it.

**Why per-role depth was rejected too.** Mirroring the library's own rule (5 for
containers, 50 otherwise) would be fast and correct *today*. It requires
replicating `should_use_shallow_search`'s undocumented predicate, and if that
drifts in a future version our depth silently drops below the resolver's.

**The asymmetry settles it.** Under-counting is silent and writes to the wrong
window. Over-counting is loud and refuses a step the user can see. Only one of
those is recoverable, so the check errs toward over-enumeration.

**No truncation signal exists.** For the record, since the alternative would have
been to detect truncation rather than reason about depth: `UIMatcher::search` in
`uiautomation` recurses while `depth < self.depth` and returns `Ok(())` either
way. No flag, no counter, no error. A chosen depth cannot be checked for
completeness at runtime, which is why the coverage argument above had to be made
from the source instead.

### The constructive-resolution change to the target check

The end-to-end test exposed a defect in the *existing* target check, and fixing
it was necessary to make the ambiguity check reachable at all.

On the first end-to-end run, the containment-decoy trial failed with
`FAILED (resolved the wrong element)` at the navigate step:

```
  recorded: "AmbigReplay Invoice - Personal - Microsoft​ Edge"
  resolved: "Draft AmbigReplay Invoice - Personal - Microsoft​ Edge"
```

`first()` returns whichever containment match it reaches first in traversal
order, and that is **not necessarily the recorded one**. The target check then
vetoed the whole step. So a legitimate, unambiguous replay was refused — the
correct window was open, uniquely identifiable by exact name, and sitting in the
candidate list — because the resolver guessed and the checker could only veto.

This was pre-existing behaviour, not a regression from the ambiguity work: the
new code was never reached, because the target check returns first.

The fix is that resolution is now **constructive rather than defensive**. Since
the enumeration is already paid for, the exact match is right there:

* exactly one candidate carries the recorded name → **act on that element**,
  which may not be the one `first()` returned
* several do → `FailedAmbiguous`
* none do, or the enumeration failed → fall back to checking `first()`'s own
  pick, which distinguishes "the target is genuinely gone" (`FailedWrongTarget`,
  as before) from "the enumeration did not see it" (proceed, with the reason
  recorded in the step detail — never silently)

That last branch matters: "could not check" is carried into the step detail
rather than dropped, so it stays distinguishable from "checked, and it was fine".

**Evidence level: confirmed in both directions** (2026-08-10, later the same
day). This section previously carried a caveat that the change had one
validation run, all of it in the direction where `first()` picks wrong and the
change rescues the step — the direction that makes it look good. That gap is now
closed; see "Both directions of constructive resolution" below.

### `FailedAmbiguous` is a separate result from `FailedWrongTarget`

They are easy to conflate and were deliberately kept apart. `FailedWrongTarget`
is a *proven mismatch* — the element found is demonstrably not the recorded one.
`FailedAmbiguous` is *unproven identity* — the element found may well be correct,
but an equally good candidate exists. The remedies differ (a drifted selector
versus a duplicate window), and collapsing them would make a log read "resolved
the wrong element" for an element that is probably right. For a bug whose whole
nature is plausible-but-wrong reporting, that is the wrong trade.

### End-to-end evidence: five trials

`text_capture_probe -- ambigreplay` builds a playbook from live-observed values,
pushes it through the real `CapturedStream::admit` → `compile` → `store` →
`replay` path, and replays it against a desktop that gains one window per trial.
Every trial prints its own premise before the verdict, because the previous probe
in this investigation once reported a clean pass against a decoy that had never
been created.

```
  trial                                  status           ms  failure
  1. clean, checks ON                    completed      2185  (none)
  1b. clean, old playbook (checks OFF)   completed       455  (none)
  2. containment decoy present           completed      2447  (none)
  3. element-level ambiguity             failed         1614  FAILED (selector is ambiguous) @ click
  4. window-level ambiguity              failed          834  FAILED (selector is ambiguous) @ navigate
```

| # | Desktop state | Required | Result |
|---|---|---|---|
| 1 | target only | complete | completed, 3/3 |
| 1b | target only, `target.name` stripped | complete, checks skipped | completed, 3/3 |
| 2 | + window *and* field whose names CONTAIN the recorded ones | still complete | completed, 3/3 |
| 3 | + different window title, EXACT recorded field name | click refuses | `FailedAmbiguous` @ click, navigate still executed |
| 4 | + EXACT recorded window title | navigate refuses | `FailedAmbiguous` @ navigate |

Four things this establishes that the isolated `decoycount` measurement could
not:

* **Both sites work, independently.** Trial 3's field twin has a *different*
  window title on purpose — a window-level refusal would stop the run before any
  element step is reached, so the element site would never be exercised.
* **No over-rejection through the real path.** Trial 2 has containment decoys at
  *both* levels (`"Draft AmbigReplay Invoice…"` and `"Draft AmbigField"`) and
  completes. Raw candidate counts there were 2 and 2; exact counts were 1 and 1.
* **Old playbooks are untouched.** Trial 1b is the same playbook with
  `target.name` removed, which is exactly what a playbook recorded before these
  fields existed looks like. Both checks skip and it completes — the same
  strictly-additive property `scoped_selector()` was given.
* **The refusals name the problem.** Verbatim from trial 4:

```
window selector "role:Window|name:AmbigReplay Invoice - Personal - Microsoft​ Edge"
matches 2 windows that all carry the recorded name
"AmbigReplay Invoice - Personal - Microsoft​ Edge", so which one the recording
meant cannot be determined.
Refusing to activate any of them: activating the wrong one sends every later
step to the wrong window while the run still reports success.
```

### Both directions of constructive resolution

The five trials above all exercised one direction: `first()` picks a containment
decoy, and constructive resolution rescues the step. The reverse — `first()`
already returning the correct element — was never tested, so the change had only
been observed where it *changes* the outcome, never where it should leave one
alone.

`text_capture_probe -- resolveorder` closes that. Window z-order drives traversal
order, so each trial activates three windows (target plus two containment decoys,
`"Draft …"` and `"Copy of …"`) in a chosen sequence, records what `first()`
actually returns under that ordering, then replays.

**What counts as correct here is not "the run completed".** A run can complete
having typed into the wrong window — that is the entire bug. So every trial reads
all three fields before and after, and resolution is judged by *the decoy fields
staying empty*.

Six orderings, three runs, 18 trials:

```
  ordering                 first():W first():F  status     resolution   text
  target front, A then B     correct   correct  completed          ok     ok
  target front, B then A     correct   correct  completed          ok     ok
  decoy A frontmost            WRONG     WRONG  completed          ok     ok
  decoy B frontmost            WRONG     WRONG  completed          ok     ok
  target middle, A front       WRONG     WRONG  completed          ok     ok
  target middle, B front       WRONG     WRONG  completed          ok     ok
```

Identical across all three runs. Two orderings per run put `first()` on the
correct element unaided (6 samples of the previously untested direction) and four
put it on a decoy (12 samples of the previously tested one). In every trial the
text landed in the target's field and neither decoy was touched.

So the change is not merely harmless when `first()` is already right — it is
inert. Where `first()` was correct, the uniquely-matching candidate *is* what
`first()` returned, and the outcome is unchanged.

#### One anomaly, recorded rather than smoothed over

An earlier run of this probe (a five-ordering version) produced one trial where
the target field received `"can cordra"` instead of `"ordr"`. It did not
reproduce in five subsequent runs — 28 trials in total, one occurrence.

It is worth being precise about what it was and was not. The text went into the
**target's** field; both decoy fields stayed empty. So resolution was correct and
the disturbance was in the keystrokes themselves — real typing into a real
browser, where anything that steals focus mid-step can interleave characters.
That is orthogonal to the resolution logic and is not evidence about it.

It did, however, expose a fault in the probe: the original verdict collapsed
"text arrived intact" and "text arrived in the right place" into one flag, and so
announced *"constructive resolution does not behave identically across traversal
orders"* — a resolution failure that had not happened. The probe now reports the
two separately, and only calls a resolution failure when a decoy field changes.
This is the same lesson as the swallowed-error pattern below, in a new costume: a
diagnostic that cannot separate two causes will eventually name the wrong one.

### Cost: 576 ms per step, measured against a trivial page

Trials 1 and 1b are a true A/B — the same playbook, the same three steps, the
same UI, differing only in whether `target.name` is present to trigger the
checks:

```
  checks ON : 2185 ms
  checks OFF:  455 ms
  added     : 1730 ms over 3 steps (576 ms/step)
```

Per-enumeration cost is ~700–800 ms, consistent with `decoycount`'s standalone
775–956 ms at depth 50.

**The honest caveat: the baseline is a trivial local HTML page, and that is the
only workload this has been measured against.** The 4.8× multiplier is an
artifact of an unusually fast best case and will not transfer. The number that
does transfer is the absolute ~0.7 s/step — roughly 9% of the 8 s window budget
and 5% of the 15 s element budget, and small against latencies already measured
in this project, where a *single* Gmail element took 6,743 ms to appear (the
reason `ELEMENT_LOCATE_TIMEOUT` is 15 s). On a 12-step playbook it adds ~7 s.

Shipped on that basis: acceptable on the evidence available, **not settled**. It
has not been measured on a real workload.

**If it does need addressing, restricting the check to navigate steps is the
wrong answer.** Trial 3 is the direct evidence: element resolution is
desktop-wide, so a correctly-resolved navigate does not make the following click
safe — it can still land in another window. Checking only navigate closes the
window-level hole and leaves the element-level one open, which is the same silent
wrong-write at finer granularity. The two real options, in order of preference:

1. **Scope element resolution to the window the preceding navigate resolved.**
   Cuts the enumeration to a small subtree *and* shrinks the ambiguity surface
   itself. It changes which elements are findable, so it needs its own evidence.
2. **Reuse the verdict across consecutive steps sharing an identical selector.**
   Trial 1's click and type both used `role:Edit|name:AmbigField` back to back;
   caching removes about a third of the added cost. It assumes the UI did not
   change between two adjacent steps — an assumption worth testing rather than
   presuming.

There is no cheaper condition that preserves the guarantee: you need the
enumeration to know whether there is ambiguity.

### Regression evidence

The fourth change to land at these same two resolution sites, so this was checked
deliberately rather than assumed.

| Check | Result |
|---|---|
| `cargo test` (89 unit + 23 integration) | pass |
| `cargo clippy --all-targets` | 0 errors |
| `replaycheck` — a real recorded playbook still replays | **completed 4/4, 0 wrong-target rejections** |
| `multiline` — multi-line flush fix | pass, replay reproduces text exactly |
| `windowswitch` — window-switch attribution fix | pass, type ordered before the switch |
| `verify` — A–E coverage trials | pass; section 4 updated, ambiguity now covered |
| `ipc_pipeline::full_pipeline_over_ipc` | **fails — pre-existing** |

The `ipc_pipeline` failure was verified as pre-existing by stashing this work and
re-running against HEAD: identical failure, no `type` action captured. It is the
deliberately-visible test tracking `text-input-capture-truncation.md`, a capture
defect unrelated to replay.

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

4. **The collision that was never built.** The `decoycount` probe's first run
   reported the mitigation working on every line — and the collision it was
   meant to mitigate did not exist, because Edge had opened the target as a tab
   in an existing window. Recorded above.

The practical rule for anyone working in this area: when a probe reports zero,
empty, or equal, prove the call *succeeded* before drawing anything from the
value. `.ok()` and `unwrap_or_default()` in diagnostic code are where these
originate.

Instance 4 adds a second rule, and it is the one that saved this session's
result: **a probe testing a mitigation must assert that the condition being
mitigated is actually present**, and report *invalid* rather than *pass* when it
is not. Every earlier instance in this list cost hours because nothing forced
that check; this one cost one run, because the check was written before the
verdict was believed.

A related but distinct variant, worth one line rather than a section: during the
Route 1 work an edit that *deleted* the ambiguity check read as contradictory,
because the tool's diff shows the removed code in full next to the replacement
comment. Nothing was wrong with the change; the presentation made a deletion
look like a retention. Different mechanism from the swallowed errors above —
this is display, not data — but the same cost, a round trip spent establishing
what the actual state was. Stating "this removes X" in prose alongside a
deletion avoids it.

## Why it matters

Kept in the present tense because it is the argument for the fix, and for why the
fix accepts a real latency cost and a real risk of refusing borderline replays.

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
- [x] ~~**Unblock the fail-loud check by picking Route 1 or Route 2.**~~
      **Route 1 built and refuted** — see "Route 1 built, and refuted at the
      last layer". Its plumbing works and is kept; its counting mechanism does
      not. Route 2 was then described as the only unexplored option; **that is
      no longer true** — a third path exists (root-scoped counting, 2026-08-10)
      and looks better than Route 2 on first evidence, because the library does
      its own matching rather than us re-implementing it.
- [x] ~~**Finish, or kill, Route 3.**~~ **Finished: it survives.** The
      over-rejection test was run with a genuine containment decoy and repeated
      three times. Raw root-scoped counting *does* inflate to 2 on an
      unambiguous replay — the failure that killed Routes 1 and 2 — but
      filtering through `resolved_is_recorded_target` drops the decoy to 1 while
      still reporting 2 for two genuinely identical windows. See "The
      over-rejection test".
- [x] ~~**Settle enumeration completeness, then implement.**~~ **Settled and
      implemented** — see "Search depth". The completeness question was the wrong
      one: absolute completeness is unverifiable (`UIMatcher::search` gives no
      truncation signal at all), but it is also not what the check needs. The
      requirement is that the counter cover *the resolver's* search space, and
      that is establishable from the library source — same root node, same
      matcher, and the resolver's depth is bounded by 50, which is what passing
      `None` gives the counter.
- [x] ~~**Reconsider whether fail-loud-on-ambiguity is the right design at
      all.**~~ It was the right design; the two earlier failures were mechanism
      failures, not design failures. Both over-counted, and over-counting refuses
      working playbooks. Filtering the count through `resolved_is_recorded_target`
      is what made the mechanism usable.
- [x] ~~**Repeat the end-to-end validation of the constructive-resolution
      change.**~~ **Done — confirmed in both directions.** The gap was that the
      change had only been tested where `first()` picks wrong and it rescues the
      step. `text_capture_probe -- resolveorder` varies window z-order across six
      orderings, three runs, 18 trials, with two containment decoys present: 6
      trials had `first()` already correct and 12 had it landing on a decoy. All
      18 completed, wrote to the target field, and left both decoy fields empty.
      Where `first()` was already right the change is inert. The two halves of
      the fix now rest on comparable evidence. See "Both directions of
      constructive resolution", including the one non-reproducing typing anomaly
      and the probe fault it exposed.
- [ ] **Measure the cost on a real workload.** ~576 ms/step is measured only
      against a trivial local page — see "Cost". Acceptable on that evidence, not
      settled. If a real playbook shows otherwise, the fix is scoping element
      resolution to the navigated window, or caching the verdict across
      consecutive steps with an identical selector — *not* checking fewer steps,
      for the reason trial 3 demonstrates.
- [ ] **Surface ambiguity to the user rather than only refusing.** Candidate 2
      from the original brief, still untouched, and now unblocked: detection
      works, so "this matched two windows, which did you mean?" is buildable.
      Today the run fails with a message naming the selector and the count, which
      is honest but leaves the user to close the duplicate window themselves.
- [ ] **Prefer identifiers that do not change with content.** A window title that
      mutates as the user types is a poor key. Process id continuity, or a
      window handle held for the run, would be stable across exactly the change
      that broke this. Note the measured limits: Windows 11 Notepad hands new
      launches to an existing instance, HWND is 0 for browser windows, and a
      UWP app's process resolves to the shared `ApplicationFrameHost.exe`.
- [x] ~~**Reproduce it in a probe.**~~ `text_capture_probe -- ambigreplay`
      reproduces it deterministically: trial 4 opens two windows sharing a title
      and drives a real replay at them. Before the fix that scenario is exactly
      the silent wrong-window write; after it, the run refuses.
