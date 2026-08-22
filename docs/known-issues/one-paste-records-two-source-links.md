# One paste records two source links

**Status:** open, observed 2026-08-22 in session record-1a2123c0.
**Severity: LOW today, and that is contingent.** Detection asks whether source
positions are DISTINCT, and duplicates collapse under that question — the same
session reported "14 pair(s), 7 distinct source position(s)" and the 7 was
correct. Anything that ever counts links rather than distinct positions would
read every transfer as two.
**Where:** unresolved. `pair_clipboard` appends one `SourceLink` per `Ctrl+V`
key-down, so two links means two key-down events reached it.

## The evidence

```
[paradigm] source links: 14 pair(s), 7 distinct source position(s)
[paradigm]   link 1787425643427: el/0/Pending    -> A2
[paradigm]   link 1787425643427: el/0/Pending    -> A2
[paradigm]   link 1787425646247: B2              -> B2
[paradigm]   link 1787425646247: B2              -> B2
...
[paradigm]   link 1787425697757: el/2/Pending    -> C4
[paradigm]   link 1787425697759: el/2/Pending    -> C4
```

Every pair is duplicated. Six of the seven carry an **identical** timestamp; the
seventh differs by 2ms.

That 2ms matters. Identical timestamps would suggest one event delivered twice.
A 2ms gap suggests two genuinely separate key-down events — key auto-repeat from
holding `Ctrl+V` a fraction too long is the obvious candidate, and it would
produce exactly this shape.

## What it is NOT

**Not double-clicking.** The same recording shows every mouse click in pairs
too, and that has a mundane explanation: the user double-clicked to select text
before copying. A controlled probe issuing single clicks produced exactly one
action per click, so the recorder does not duplicate clicks and the pairs there
are real user gestures. The keyboard duplication is a separate question with no
such explanation.

## Why this is filed rather than fixed

The cause is not established. Two key-downs 2ms apart is a different defect from
one event delivered twice: the first wants debouncing on the paste path, the
second wants finding out why the stream repeats. Guessing between them would
mean writing a fix whose correctness nobody could check.

**The cheap next step** is a controlled probe: `capture_probe`, one deliberate
short `Ctrl+V`, and a count. One link means the recording's long presses were
auto-repeat; two means the event doubles regardless.

## What a fix must not do

**It must not deduplicate by source position.** One copy legitimately feeds
several pastes — that is the ordinary fan-out this project already supports —
and collapsing on the source would erase real transfers into different cells.
Any debounce belongs on the keystroke, bounded by time, and must be measured
against the fastest a person can genuinely paste twice.

## Related

* `docs/known-issues/sheets-cell-edits-are-captured-by-both-watchers.md` — a
  different double-capture, on the grid path, with a known cause.
