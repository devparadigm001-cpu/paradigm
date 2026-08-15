# A WebView2 checkbox's toggle state is not readable, but the checkbox works

**Status:** open (external), worked around. Measured 2026-08-14 while proving
§4.5's supervision checkbox.
**Affects:** probes only. No product code reads `is_toggled`.

## What happens

`terminator::UIElement::is_toggled()` returns `false` for the app's own
supervision checkbox **in every state** — before clicking, after
`set_toggled(true)`, after `click()`, and after focus-and-Space.

The checkbox itself is fine. The node is exactly what it should be:

```
role=CheckBox  bounds=560,934 18x18  enabled=Ok(true)  visible=Ok(true)
name="Pause and ask me about incomplete records Off by default: …"
```

and the clicks land: the run that followed **paused on the incomplete record**,
which is behaviour only a supervised run produces. So the control was switched
on while the property still read off.

## Why this is worth a document rather than a shrug

It produced two silently-wrong probes before it was noticed, both of the same
family as the `csv_at` defect — a verifier that cannot fail.

**1. A retry cascade that passed by arithmetic.** The first helper tried
`set_toggled` → `invoke` → `click` → Space, stopping when `is_toggled` agreed.
It never agreed, so all four ran. Three of them actually worked, so the box was
toggled an odd number of times and happened to land ON:

```
set_toggled(true) -> on ;  invoke -> errored ;  click -> off ;  Space -> on
```

The probe passed. The pass meant nothing — one more working mechanism and it
would have landed OFF and "disproved" a feature that works.

**2. A default-state assertion that could not fail.** The unsupervised probe
asserted "the checkbox reads false without touching it" to prove §4.5's default.
Since `is_toggled` reads `false` unconditionally, that assertion passes whether
the default is off, on, or the checkbox has no state at all.

## The workaround

Do not read toggle state. Click **once**, deterministically, and let the
*behaviour* decide — which is the rule `click_and_wait` already applies to
buttons: check the effect, not the mechanism.

For supervision the two runs are decisive as a pair, because only one thing
differs between them:

| | checkbox | run does |
|---|---|---|
| default | not touched | writes the blank, logs it, carries on to the end |
| opt-in | clicked once | pauses on the incomplete record, offers correction |

Neither outcome can be produced by a misreported property. Presence of the
checkbox is still asserted — one that stopped rendering would otherwise look
identical to a working default.

## What is not known

Whether this is specific to `<input type="checkbox">` under React's controlled
pattern, to this WebView2 build, or to how `terminator` maps TogglePattern.
`invoke` failing with `Failed to get InvokePattern: The operation completed
successfully.` suggests the pattern lookup itself is what is unavailable, not
that the answer is false — but that is inference from an error string, not a
measurement.

`examples/text_capture_probe.rs cbdump` dumps every checkbox node with bounds,
enabled/visible/toggled, and tries a keyboard route. It is the fastest way to
ask the same question of the next webview control, and it needs the relevant
card to be on screen when it runs.
