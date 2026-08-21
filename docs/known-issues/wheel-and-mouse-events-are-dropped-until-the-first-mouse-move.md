# Wheel and mouse events are dropped until the recorder has seen a mouse move

**Status:** open, measured 2026-08-21. Upstream, in
`terminator-workflow-recorder` 0.23.35.
**Severity: MEDIUM**, and it is the reason a whole class of signal looked absent
when it was merely gated.
**Where:** `recorder/windows/mod.rs`. `last_mouse_pos` is written in exactly one
place — the `EventType::MouseMove` arm at `:1310`. Three other arms read it and
return early when it is `None`: `MouseDown` (`:1211`), `MouseUp` (`:1269`) and
`Wheel` (`:1350`).

```rust
EventType::Wheel { delta_x, delta_y } => {
    if let Some((x, y)) = *last_mouse_pos.lock().unwrap() {
```

There is no `else`. A wheel event that arrives before any mouse move is
discarded without a log line.

## The measurement

Four wheel notches were injected at the centre of an Edge window with the
cursor placed by `SetCursorPos`, while a probe subscribed to the raw recorder
with paradigm's own config:

```
  wheel events     : 0
  scroll_delta sum : dx=0  dy=0
  other mouse      : 0
  key-downs        : 3        <- "abc", same run, same hook
```

The keystrokes prove the input hook was alive and does see injected input. The
wheel notches still produced nothing.

Repeating the run with six **relative** moves (`mouse_event` with
`MOUSEEVENTF_MOVE`) before the notches:

```
  wheel #1   delta=(    0,   -1)  at ( 3383,  530)  under: Document "WHEELTEST"
  wheel #2   delta=(    0,   -1)  at ( 3383,  530)  under: Document "WHEELTEST"
  wheel #3   delta=(    0,   -1)  at ( 3383,  530)  under: Document "WHEELTEST"
  wheel #4   delta=(    0,   -1)  at ( 3383,  530)  under: Document "WHEELTEST"
  other mouse      : 6
```

Same process, same config, same injection method for the notches. The only
change is the six moves, and they are what `SetCursorPos` alone does not
provide: it repositions the cursor without putting an event through the
low-level hook, so `last_mouse_pos` stays `None`.

## Why it matters beyond scrolling

`MouseDown` and `MouseUp` read the same variable. Whether that reaches paradigm
is **not tested here** — paradigm takes clicks from the high-level
`WorkflowEvent::Click`, which may well be produced by the UI Automation path
rather than this one. It is a question this doc raises and does not answer.

The measured consequence is narrower and certain: a recording whose first
scroll happens before the user's first mouse movement loses that scroll
silently.

## What this cost, and the lesson that generalises

The first two probe runs reported zero wheel events and were nearly written up
as "the recorder does not surface scrolls". That would have been wrong, and it
would have justified building an inference layer to recover something the
stream was already carrying. What separated the two readings was sending a
keystroke in the same run: a positive control on the hook itself.

**A silent zero is not evidence of absence until something else in the same run
is proven present.**

## Reproducing

1. `cargo run --example scroll_event_probe -- 16`
2. Within the window, inject relative mouse moves and then wheel notches
   through `mouse_event` — `SetCursorPos` will not do, for the reason above.
3. Repeat without the moves. The notches vanish.

## Related

* `docs/known-issues/element-bounds-are-viewport-relative-so-scrolling-moves-them.md`
  — the defect whose direction 3 this signal serves.
* `docs/known-issues/text-input-capture-truncation.md` — the earlier case of an
  upstream decision reported through `tracing` and therefore invisible here.
