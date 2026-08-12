# terminator-rs: clicks are refused on secondary monitors

**Status: MITIGATED, and re-verified on real hardware 2026-08-11.** The defect is
unchanged in `terminator-rs` 0.23.35 and is **not fixed** — the workaround in
`replay::click` is what makes multi-monitor replay work, and it was measured
doing so on a genuine two-monitor setup rather than assumed. The decision on
upstream / vendoring / workaround is recorded under "The decision". Still **not
reported upstream** — the report is drafted and ready to paste under "Upstream
report, ready to file"; only submission remains, which needs a personal GitHub
account.
**Affected version:** `terminator-rs` 0.23.35 (crate lib name `terminator`).
**Platform:** Windows, multi-monitor only.
**Found:** 2026-08-03, during the Phase 1 Step 2 Terminator capability probe.
**Severity: HIGH.** Raised from "annoying in test tooling" on 2026-08-04, when
Step 6's live replay run confirmed it affects the real product path. See
"Confirmed impact on replay" below.

## Summary

`UIElement::click()` fails with `AutomationError::ElementNotVisible("Element not
visible")` for **every element on a non-primary monitor**, even though the
element is on screen, enabled, has a valid non-zero rect, and UI Automation
itself reports it as not offscreen.

Reads are unaffected — the accessibility tree walks correctly. `type_text()` is
also unaffected. Only the click path is gated.

For Paradigm this is a real product constraint, not a test-harness quirk: as it
stands we cannot drive any application a user has parked on a second display.

## Root cause

`click()` runs `validate_clickable()`, whose second check calls `is_visible()`.
In `src/platforms/windows/element.rs`:

```rust
fn is_visible(&self) -> Result<bool, AutomationError> {
    let is_offscreen = self.element.0.is_offscreen()?;
    if is_offscreen { return Ok(false); }          // passes: UIA says on-screen

    if let Ok((x, y, width, height)) = self.bounds() {
        if width <= 0.0 || height <= 0.0 { return Ok(false); }   // passes

        if let Ok(work_area) = WorkArea::get_primary() {
            if !work_area.intersects(x, y, width, height) {
                return Ok(false);                  // <-- fails here
            }
        }
        return Ok(true);
    }
    Ok(false)
}
```

And `WorkArea::get_primary()` is:

```rust
SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut rect, ...)
```

`SPI_GETWORKAREA` is defined by Win32 to return the work area of the **primary
monitor only**. It has no notion of a virtual desktop. `intersects()` then
requires `elem_left < work_right`, so any element whose x lies beyond the
primary monitor's width can never satisfy it.

The failure is silent in the sense that the resulting error message —
"Element not visible" — describes a symptom that is not true, sending you
looking at the element rather than at the check.

## Reproduction

1. A Windows machine with two monitors, where the secondary is positioned to the
   **right** of the primary (secondary origin x >= primary width).
2. Open any application window on the secondary monitor.
3. Run:

```rust
let desktop = Desktop::new_default()?;
let app = desktop.application("notepad")?;
let button = /* any element in that window */;

println!("{:?}", button.bounds());      // Ok((2888.0, 48.0, 33.0, 25.0)) - valid
println!("{:?}", button.is_visible());  // Ok(false)   <-- wrong
button.click()?;                        // Err(ElementNotVisible("Element not visible"))
```

Move the same window to the primary monitor and every call succeeds unchanged.

## Evidence from our run

```
monitors: 2
  primary   bounds={0,0,1920,1080}   work area={0,0,1920,1020}
  secondary bounds={1920,0,1920,1080}

notepad window rect : Left=1952 Top=32 Right=3104 Bottom=624   (entirely secondary)

target element "Add New Tab"
  bounds       : x=2888 y=48 w=33 h=25      (non-zero, valid)
  is_offscreen : false                       (UIA agrees it is on screen)
  is_visible() : false                       (2888 >= 1920, so intersects() fails)
  click()      : Err(ElementNotVisible("Element not visible"))
```

Note these figures are in physical pixels, taken with the process running as
per-monitor DPI aware (see the separate note below). The defect does **not**
depend on DPI: with fully correct physical metrics, `2888 >= 1920` still holds.

## Confirmed impact on replay (2026-08-04)

This is no longer confined to probes. Step 6's replay module executes stored
playbooks through the same `element.click()` path, and a live run against a
window on the secondary monitor produced:

```
step 3 [click] executed (coordinate-click fallback)
  element.click() refused (Element not visible); clicked real coordinates
  (2254, 218) instead -- known multi-monitor defect

step 4 [click] executed (coordinate-click fallback)
  element.click() refused (Element not visible); clicked real coordinates
  (2254, 292) instead -- known multi-monitor defect
```

Every click step on a non-primary display takes the workaround, and that fact is
now written permanently into `run_steps_log.target_ui_context_json` for each
affected step. Practical consequences:

* **Replay depends on the workaround, not on the library.** If the coordinate
  fallback is ever removed or the element has unusable bounds, those steps fail
  outright rather than degrading.
* **The workaround requires per-monitor DPI awareness.** `replay::ensure_dpi_aware()`
  exists solely because of this; without it the fallback clicks the wrong place
  silently. That coupling is invisible to callers and easy to break.
* **Run history is polluted.** Diagnosing a real replay problem later means
  reading past this message on most click steps.

It also affected recording indirectly: the Step 6 probe's own driven clicks used
bare `element.click()`, all of them were refused, focus never moved, and a
password-field completion event was never captured -- so the first live run
recorded a playbook missing the very step it was meant to demonstrate.

## Correct fix (upstream)

Resolve the work area of the monitor **containing the element**, rather than the
primary monitor:

1. Build a `RECT` from the element bounds.
2. `MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST)` to get the `HMONITOR`.
3. `GetMonitorInfoW(hmonitor, &mut MONITORINFO)` and use `rcWork` — the
   per-monitor work area, correctly excluding that monitor's taskbar.
4. Intersect the element rect against that `rcWork`.

`MONITOR_DEFAULTTONEAREST` also gives sane behaviour for an element straddling
two displays or sitting slightly off-screen, which the current code cannot
express at all.

A cheaper partial fix would be to intersect against the whole virtual desktop
(`SM_XVIRTUALSCREEN` / `SM_YVIRTUALSCREEN` / `SM_CXVIRTUALSCREEN` /
`SM_CYVIRTUALSCREEN`), but that loses the taskbar exclusion the check exists to
provide, so per-monitor `rcWork` is the right answer.

## Our workaround

`examples/terminator_probe.rs` catches `AutomationError::ElementNotVisible`,
prints an explicit note naming this defect, and then clicks the element's centre
via `Desktop::click_at_coordinates()`, which does not go through
`validate_clickable()`. That is still a real synthetic mouse click at real
screen coordinates.

The workaround deliberately prints rather than silently substituting, so the
defect cannot reach later phases disguised as working code.

## Separate finding: DPI awareness is on us, not upstream

While investigating we hit a second problem that looked similar but was ours.

Rust binaries ship no DPI manifest and are therefore **DPI-unaware** by default.
UI Automation always reports element bounds in physical pixels, but
`GetSystemMetrics` is virtualized for a DPI-unaware process. On a primary
display at 125% scaling ours reported `1536x864` instead of `1920x1080`.

`Desktop::click_at_coordinates()` normalises with
`x / GetSystemMetrics(SM_CXSCREEN) * 65535`, and Windows maps that back through
the real primary width. Those two cancel out **only if the denominator is in the
same physical units as the input**. With a virtualized denominator the click
overshot by the scale factor — for us to roughly x=3611 instead of x=2888,
past the target window entirely. It failed silently: no error, no effect.

Fix, called once at process start before anything reads screen metrics:

```rust
SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
```

Verified by the probe printing metrics either side of the call:

```
[ok] per-monitor DPI awareness: primary metrics 1536x864 -> 1920x1080
```

**Any Paradigm process that mixes UIA bounds with screen coordinates must do
this**, including the Tauri app itself once it starts driving the desktop.
Tauri's own binary may already be manifested; that needs checking separately
rather than assuming.

## Explicitly NOT a defect

`Desktop::click_at_coordinates()` normalising against `SM_CXSCREEN` instead of
passing `MOUSEEVENTF_VIRTUALDESK` initially looked like a second bug. It is not:
the mapping is self-inverting and addresses secondary monitors correctly once
the caller is DPI-aware. Verified working across monitors in our run.

One untested caveat: a monitor positioned to the **left of** or **above** the
primary has negative coordinates, which would produce a negative normalised
value. We have not tested that layout and it may genuinely break. Worth checking
before assuming multi-monitor support is complete.

## The decision (2026-08-11)

### Re-measured first, on a real two-monitor machine

This machine has the exact layout the defect needs — a secondary display to the
right of the primary:

```
\\.\DISPLAY1 primary=True  bounds={X=0,Y=0,Width=1536,Height=864}
\\.\DISPLAY4 primary=False bounds={X=1920,Y=0,Width=1920,Height=1080}
```

(The primary reporting 1536×864 is this document's DPI finding showing up live:
that reading came from a DPI-unaware shell.)

`text_capture_probe -- multimon` opens the probe page positioned at x=2000 and
exercises both halves. Nothing in it clicks arbitrary UI:

```
  FieldA bounds: Some((2180.0, 407.0, 498.0, 41.0))
  on the secondary display (x >= 1920): true

  is_visible() : Ok(false)   (UIA says on-screen; the check disagrees)
  click()      : Err Element is not visible: Element not visible

  falling back to a real click at (2429, 428)...
  focused element afterwards: "FieldA"
```

Both claims hold: the library **still refuses** the click, and the coordinate
fallback **still lands it** — proven by what holds focus afterwards, not by the
call returning `Ok`.

### Why not vendor or patch the dependency

Same reasoning as `press-key-enter-injects-end-keystroke.md`, and it applies more
strongly here. `Cargo.toml` has no `[patch]` section and no `path`/`git`
dependencies; patching would be a new pattern to re-apply and re-verify on every
version bump. And unlike a latent defect, this one is already covered: the
workaround is in the product path and now has evidence behind it.

### Why upstream is right but cannot be the answer here

`MonitorFromRect` + `GetMonitorInfoW`'s `rcWork` is the correct fix and belongs
in the library — the "Correct fix (upstream)" section above still stands
unchanged. But it lands on someone else's timeline against a version this project
pins, so it cannot be what protects replay today. Filing it stays open.

### So: the workaround, made honest rather than invisible

The workaround already existed in `replay::click`. What this session changed is
the two things the doc called out as fragile about depending on it.

**The DPI coupling is now checkable.** `replay::is_per_monitor_dpi_aware()`
reports the process's actual awareness. Note why the obvious version of this
would have been wrong: `SetProcessDpiAwarenessContext` *fails* when awareness was
already set, which is exactly what happens under a host that ships a DPI
manifest — so checking the setter's return value says nothing. Asking for the
resulting state does.

**The failure is no longer silent.** If the coordinate fallback runs in a process
that is not per-monitor aware, the step detail now says so. That failure
otherwise looks like "the click did nothing" rather than like a DPI problem —
measured previously as a click meant for x=2888 landing at ~3611.

### Item 3 answered: the Tauri app is already per-monitor DPI aware

The app now states it at startup, and it reads:

```
[paradigm] per-monitor DPI aware: true
```

That is **before** `replay()` calls `ensure_dpi_aware()` — Tauri's own binary is
manifested per-monitor aware, so the app was always correct here and
`ensure_dpi_aware` is a no-op belt-and-braces for it. Reported rather than
enforced at startup: forcing an awareness there would change how the app's own
window scales, which is a rendering decision, not an automation one.

One caveat on the probe's own output, so it is not over-read: its `main()` calls
`ensure_dpi_aware()` before any mode runs, so the "BEFORE: true" line it prints
says nothing about a fresh process. The app's startup line is the one that
answers the question.

## Upstream report, ready to file

Everything needed is below; it could not be submitted from this session (filing
needs a personal GitHub account). Paste as-is.

**Where:** `mediar-ai/terminator` — confirmed from the crate manifest
(`repository = "https://github.com/mediar-ai/terminator"`, `terminator-rs`
0.23.35). The faulty code is terminator's own, in
`src/platforms/windows/element.rs`.

**Title:** `is_visible()` returns false for every element on a secondary
monitor, so `click()` refuses valid targets (`SPI_GETWORKAREA` is primary-only)

**Version:** `terminator-rs` 0.23.35 (crate lib name `terminator`). Verified
present in the published source, not only observed at runtime.

**Platform:** Windows 11, multi-monitor. Reproduced with a secondary display
positioned to the **right** of the primary (secondary origin x ≥ primary width).
Single-monitor setups are unaffected.

### Expected vs actual

**Expected:** an element that is on screen, enabled, has a valid non-zero
bounding rect, and that UI Automation reports as *not offscreen*, should be
considered visible, and `click()` should click it.

**Actual:** `is_visible()` returns `Ok(false)` and `click()` fails with
`AutomationError::ElementNotVisible("Element not visible")`, for **every**
element on a non-primary monitor. Reads and `type_text()` are unaffected — only
the click path is gated.

The error message describes a condition that is not true, which sends the caller
looking at the element rather than at the check.

### Root cause

`click()` → `validate_clickable()` (`element.rs:310`) → step 2 calls
`is_visible()` (`element.rs:321`). `is_visible()` (`element.rs:1284`) ends with:

```rust
// Check if within work area (not behind taskbar)
if let Ok(work_area) = WorkArea::get_primary() {
    if !work_area.intersects(x, y, width, height) {
        tracing::debug!("Element outside work area");
        return Ok(false);          // <-- every secondary-monitor element
    }
}
```

`WorkArea::get_primary()` (`element.rs:109`) is a single
`SystemParametersInfoW(SPI_GETWORKAREA, ...)` call. **`SPI_GETWORKAREA` is
defined by Win32 to return the work area of the primary monitor only** — it has
no notion of a virtual desktop. `intersects()` (`element.rs:138`) then requires:

```rust
elem_left < work_right && elem_right > self.x && ...
```

so any element whose `x` lies beyond the primary monitor's width can never
satisfy it, regardless of where it actually is.

### Reproduction

1. Windows machine with two monitors, secondary to the right of the primary
   (secondary origin x ≥ primary width).
2. Open any application window on the **secondary** monitor.
3. Run:

```rust
let desktop = Desktop::new_default()?;
let app = desktop.application("notepad")?;
let element = /* any element in that window */;

println!("{:?}", element.bounds());       // Ok((2888.0, 48.0, 33.0, 25.0)) — valid
println!("{:?}", element.is_offscreen()); // Ok(false)  — UIA says it is on screen
println!("{:?}", element.is_visible());   // Ok(false)  — wrong
element.click()?;                         // Err(ElementNotVisible("Element not visible"))
```

Move the same window to the primary monitor and every call succeeds, unchanged.

### Measured evidence

Two independent runs on real hardware, seven days apart.

**Run 1** — Notepad on the secondary display:

```
monitors: 2
  primary   bounds={0,0,1920,1080}   work area={0,0,1920,1020}
  secondary bounds={1920,0,1920,1080}

notepad window rect : Left=1952 Top=32 Right=3104 Bottom=624   (entirely secondary)

target element "Add New Tab"
  bounds       : x=2888 y=48 w=33 h=25      (non-zero, valid)
  is_offscreen : false                       (UIA agrees it is on screen)
  is_visible() : false                       (2888 >= 1920, so intersects() fails)
  click()      : Err(ElementNotVisible("Element not visible"))
```

**Run 2** — re-verified 2026-08-11 on a two-monitor machine, a browser input at
x=2180:

```
\\.\DISPLAY1 primary=True  bounds={X=0,Y=0,Width=1536,Height=864}
\\.\DISPLAY4 primary=False bounds={X=1920,Y=0,Width=1920,Height=1080}

  FieldA bounds: Some((2180.0, 407.0, 498.0, 41.0))
  on the secondary display (x >= 1920): true

  is_visible() : Ok(false)   (UIA says on-screen; the check disagrees)
  click()      : Err Element is not visible: Element not visible

  falling back to a real click at (2429, 428)...
  focused element afterwards: "FieldA"
```

**The last two lines are the important ones.** Clicking the element's centre via
`Desktop::click_at_coordinates()` — which does not go through
`validate_clickable()` — **lands the click**, confirmed by which element holds
focus afterwards rather than by the call returning `Ok`. So the element is
genuinely clickable at those exact coordinates; only the visibility gate refuses
it. That isolates the defect to the check, not to the element, the window, or
the click mechanism.

### Ruled out, so it need not be re-checked

* **Not offscreen.** `is_offscreen()` returns `false` — UI Automation itself
  reports the element as on screen.
* **Not zero-size bounds.** The rect is valid and non-zero; the earlier
  `width <= 0.0 || height <= 0.0` branch passes.
* **Not a DPI artifact.** Figures are physical pixels, taken with the process
  running per-monitor DPI aware
  (`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`).
  With fully correct physical metrics `2888 >= 1920` still holds. (In Run 2 the
  `1536×864` primary figure comes from a DPI-*unaware* PowerShell enumeration
  and is shown only to identify the display; the probe's own bounds are
  physical.)
* **Not the click path.** The coordinate click at the same location succeeds and
  moves focus, as shown above.

### Suggested fix

Resolve the work area of the monitor **containing the element**, rather than the
primary monitor:

1. Build a `RECT` from the element bounds.
2. `MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST)` → `HMONITOR`.
3. `GetMonitorInfoW(hmonitor, &mut MONITORINFO)` and use **`rcWork`** — the
   per-monitor work area, which correctly excludes that monitor's taskbar.
4. Intersect the element rect against that `rcWork`.

`MONITOR_DEFAULTTONEAREST` also gives sane behaviour for an element straddling
two displays or sitting slightly off-screen, which the current code cannot
express at all.

A cheaper partial fix is to intersect against the whole virtual desktop
(`SM_XVIRTUALSCREEN` / `SM_YVIRTUALSCREEN` / `SM_CXVIRTUALSCREEN` /
`SM_CYVIRTUALSCREEN`), but that loses the taskbar exclusion the check exists to
provide — so per-monitor `rcWork` is the better answer.

### Untested, and worth stating

A monitor positioned **left of** or **above** the primary produces negative
coordinates. That layout has not been tested here; `intersects()` may behave
differently again, and the reporter's coordinate-click workaround normalises
against `SM_CXSCREEN` in a way that would produce a negative normalised value.

## Next steps

- [x] ~~**Raised priority:** decide whether to report upstream, patch a vendored
      copy, or carry the workaround.~~ **Decided: carry the workaround**, which
      already existed, and make its two fragile parts visible instead of
      implicit. Vendoring rejected on precedent and maintenance cost; upstream is
      the correct fix but cannot protect a pinned version on our timeline. See
      "The decision".
- [x] ~~Confirm the Tauri app binary's DPI awareness before Phase 2 automation.~~
      **Confirmed per-monitor aware**, from Tauri's own manifest, before
      `ensure_dpi_aware()` is reached. The app states it at startup.
- [ ] **Report it upstream** (`mediar-ai/terminator`): `is_visible()` should
      intersect against the work area of the monitor containing the element
      (`MonitorFromRect` + `GetMonitorInfoW`'s `rcWork`), not `SPI_GETWORKAREA`.
      **The report is now written and ready to paste** — see "Upstream report,
      ready to file". Still not filable from this session: submitting needs a
      personal GitHub account. The only remaining work is submission.
- [ ] Test a monitor positioned left of / above the primary. **Not tested** — it
      needs a display arrangement this machine does not have, and rearranging a
      user's monitors is not something to do for a test. The concern is real:
      negative coordinates would produce a negative normalised value in
      `click_at_coordinates`, and nothing has exercised that.
- [ ] **Re-check on upgrade.** Pinned to 0.23.35 and re-measured there. If the
      version moves, re-run `text_capture_probe -- multimon`: if the refusal
      stops reproducing, the workaround becomes dead weight and should go.
