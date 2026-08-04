# terminator-rs: clicks are refused on secondary monitors

**Status:** confirmed locally, **not yet reported upstream** (mediar-ai/terminator).
**Affected version:** `terminator-rs` 0.23.35 (crate lib name `terminator`).
**Platform:** Windows, multi-monitor only.
**Found:** 2026-08-03, during the Phase 1 Step 2 Terminator capability probe.

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

## Next steps

- [ ] Decide whether to report upstream, patch a vendored copy, or carry the
      workaround.
- [ ] Test a monitor positioned left of / above the primary.
- [ ] Confirm the Tauri app binary's DPI awareness before Phase 2 automation.
