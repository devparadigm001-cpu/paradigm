# Two documents in one browser window cannot both be resolved

**Status:** open, not urgent. Observed 2026-08-14 while verifying a real
workflow.
**Where:** `run::surfaces::window_for`.

## What happens

A templated workflow names a source and a destination document. To run, both
must be found:

```rust
let source_window = window_for(desktop, source_doc)?;
let destination_window = window_for(desktop, destination_doc)?;
```

`window_for` walks the top-level windows and matches on the **address bar
text**. An address bar reports the **active tab only**. So if both documents are
tabs of the same browser window, at most one of them is findable at any moment,
and opening the workflow's two surfaces fails with:

> no open window is showing the destination document `<id>`

Observed while re-opening two spreadsheets to verify a workflow: launching both
with `--new-window` still left one window, and the run could not resolve the
pair until they were genuinely separate windows.

## Why it is matched that way, and why that part is right

Matching on the document id in the URL rather than the window title is
deliberate, and documented in
[replay-window-selector-ambiguity.md](replay-window-selector-ambiguity.md):
titles are user-editable and duplicate freely — two "Untitled spreadsheet"
windows are the normal case, not the exception — whereas the id identifies
exactly one document.

The limitation is not the matching. It is that the **address bar is the only
place the id is exposed**, and a tabbed window has one address bar.

## Why it matters more than it looks

Nothing tells the user this. The workflow simply reports that a document is not
open, while the document is plainly visible on screen — as a background tab. The
message is accurate and unhelpful at the same time.

It also interacts with §4.5's correction flow: the panel asks the user to click
the correct column in the live spreadsheet, which assumes the spreadsheet is
reachable.

## What would change it

1. **Read tabs, not just windows.** Chromium exposes tabs in the accessibility
   tree; a document could be located by its tab rather than by the window's
   address bar. Whether the *inactive* tab's content is reachable is unmeasured
   and is the question that decides whether this works at all — an inactive tab
   is often not rendered.
2. **Activate a tab before resolving.** If tabs can be found and selected, the
   pair could be resolved one at a time. This makes the run steal focus, which
   §4.10's "does not lock the window" is not about but is adjacent to.
3. **Say so plainly.** Cheapest and probably first: when a document is not
   found, check whether it appears in any window's tab strip and say "that
   document is open in a background tab — it needs its own window", rather than
   "no open window is showing it".

None attempted. (3) is the one worth doing before real users meet this.
