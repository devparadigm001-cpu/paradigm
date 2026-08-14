# Paradigm's own UI was not exposed to the accessibility tree

**Status:** FIXED 2026-08-13. Cause confirmed by measurement, not inferred.
**Probe:** `src-tauri/examples/app_ui_accessibility_probe.rs`
**Fix:** `--force-renderer-accessibility` in `tauri.conf.json`'s
`additionalBrowserArgs`.

## The fix, and the measurement that confirmed it

The hypothesis below — Chromium builds the renderer accessibility tree lazily
— was correct. Two runs of the same probe against the same app:

| | before | after |
|---|---|---|
| nodes in a full `children()` walk | 24 | **47** |
| `Button` | 3 (window chrome only) | **10** |
| `Document` | 0 | 1 |
| `Text` | 0 | 13 |
| named page controls | none | `"Start recording"`, `"Refresh"`, `"Demo: start recording"` |

Confirmed twice, in the right order:

1. Launched with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--force-renderer-accessibility`
   as an environment variable — 47 nodes, 10 buttons. That establishes the flag
   is what does it.
2. Removed the environment variable, put the flag in `tauri.conf.json`
   instead, relaunched — **47 nodes, 10 buttons again**. That establishes the
   committed configuration is what does it, rather than a leftover env var in
   one shell.

The second run is the one that matters. A fix proven only under the
environment variable that was used to discover it would not be a fix.

### Note on the argument string

`additionalBrowserArgs` **replaces** Tauri's default rather than appending to
it, so the default is written out explicitly alongside the new flag:

```
--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --force-renderer-accessibility
```

Dropping the first half would silently turn Microsoft's out-of-process UI
features back on as a side effect of an accessibility fix.

---

*Everything below is the original report, kept because the measurement is the
point and a fixed issue with its evidence deleted is just an assertion.*

## What was measured

With `npm run tauri dev` running and the main window open, against the live
`Paradigm` window.

A full `children()` walk of the window, depth 25, node budget 4000:

```
nodes walked: 24 (budget left 3976)

  Button      3      <- Minimize, Maximize, Close
  MenuBar     1
  MenuItem    1      <- "System"
  Pane        17     <- all unnamed
  TitleBar    1
  Window      1
```

A locator sweep over candidate roles, same window:

```
role:Button      3    (the same window chrome)
role:Edit        0
role:CheckBox    0
role:Hyperlink   0
role:ListItem    0
role:Custom      0
role:Text       13    <- static text only
role:Document    1
```

The 13 `Text` nodes read correctly — "Stored playbooks", "No playbooks saved
yet — record and save one below.", and so on. So **static text is reachable and
every interactive control is not**.

The two traversals also disagree: the locator finds text and a Document that
the `children()` walk never reaches. That is itself a clue about where the tree
stops being built.

## Why this matters more than it looks

1. **Accessibility.** A screen reader would find the same thing here: readable
   prose and nothing operable. Every button in the app is currently unreachable
   by assistive technology.

2. **This product drives other apps through exactly this tree.** Paradigm's
   premise is that an application's controls are addressable through UI
   Automation. Its own are not — so it cannot record or replay against itself,
   and it cannot be driven by the kind of automated click-through every backend
   item in this build was verified with.

## What this blocked

Section 6's live verification. Items 1–4 and 7 are built against commands
proven live by the backend probes, and the exact JSON the frontend is typed
against is pinned by contract tests in `commands.rs`. But the click-through
itself could not be automated, because from UI Automation's point of view there
is nothing to click.

**It has to be done by a person, and until it is, Section 6 is not closed.**

## Likely cause — a hypothesis, not a conclusion

Chromium-based webviews build the renderer accessibility tree lazily, and only
once a client announces itself as assistive technology in a way the engine
recognises. The probe's property-level UIA queries appear not to trigger that,
so the host pane is exposed while the document inside it stays collapsed.
WebView2 accepts `--force-renderer-accessibility`; whether Tauri passes
additional browser arguments through, and whether that flag alone is enough,
is **unmeasured**.

Do not treat the cause as established. What is established is the measurement
above.

## What would close this

1. Launch the dev app with renderer accessibility forced (Tauri additional
   browser arguments) and re-run the probe. If page controls appear, the cause
   is confirmed and the fix is a launch argument.
2. If they do not, the next candidate is that the webview needs a real UIA
   client connection rather than the property queries the probe makes.

Either way the probe is the check: it should print page buttons by name.
