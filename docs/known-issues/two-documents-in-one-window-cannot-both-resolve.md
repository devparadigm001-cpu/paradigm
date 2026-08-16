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

## Confirmed with direct evidence 2026-08-15, and it is worse than written above

The section above frames this as "two documents in the same window". Measured,
the condition is narrower and far easier to hit: **any document that is not the
ACTIVE tab is invisible**, even when it is plainly open and even when the user
is looking at it.

A real report — "no open window is showing the destination document
`1g3lvtsY…`" while both documents were open in tabs — was checked with
`text_capture_probe tabcheck`, which asks exactly what `window_for` asks:

```
window : "Untitled spreadsheet - Google Sheets and 8 more pages - Personal - Microsoft Edge"
address: "https://docs.google.com/spreadsheets/d/1ko7z65TnzI5suwvu3LGv8yoemmhOs5siSQe9KBB8NZs/edit?gid=0#gid=0"

  1ko7z65…  (source)       window_for would FIND it
  1g3lvtsY… (destination)  window_for would NOT find it
```

**Nine tabs in one window, one address bar, and it reports the active tab
only.** The source was frontmost so it resolved; the destination was one of the
eight behind it and did not. Nothing was closed, nothing was wrong with the
document, and the error message — "no open window is showing" — was false in
the plain reading a user gives it.

Also visible in that dump: only **3 of 11** top-level windows expose an address
bar at all. `window_for` is matching against a property most windows do not
have, and the ones that do have exactly one of, no matter how many documents
they contain.

### Why this is more urgent than the original entry suggests

The original said the pair "could not be resolved until they were genuinely
separate windows" — which reads as an unusual setup. It is not. Working with
two spreadsheets in one browser window is the *ordinary* way to work, and the
run needs both. So this fires in the common case, and the message actively
misleads: it says the document is not open when the true statement is "it is
open but not in front".

### What the message should say today, before any fix

The tab strip is in the accessibility tree even when its tabs' URLs are not. A
window whose title carries "and N more pages" is telling us it has background
tabs. That is enough to replace a false statement with a true one:

> That document is open in a background tab. Bring it to the front, or give it
> its own window, and try again.

### One thing this does NOT affect

Scanning. Since `check_for_new_records` reads the source from a CSV export it
resolves no windows at all, so **Check for new is immune**. The run still opens
both surfaces and is fully exposed.

## Part B, measured 2026-08-16: a real fix is a redesign, not a patch

The message is now honest (see below), but the underlying matching still cannot
see a background tab. Before recommending a deeper fix, the question was
whether the accessibility tree exposes any way to tell what a NON-frontmost tab
holds. Measured with `text_capture_probe tabprobe` against a window of 12 tabs:

```
== window: "Example Domain and 11 more pages - Personal - Microsoft Edge"
   role:TabItem name="Untitled spreadsheet - Google Sheets"
   role:TabItem name="Untitled spreadsheet - Google Sheets"
```

**Tabs are named after the page, and carry no URL.** No `value`, no
`description`, nothing holding a document id. And every Google Sheets tab has
the *same* name — which is precisely the ambiguity that made matching use the
URL rather than the title in the first place (see
`replay-window-selector-ambiguity.md`).

So identifying a background tab requires **activating** it and reading the
address bar. That means, per candidate tab:

* stealing focus and changing what the user is looking at, mid-run;
* one activation plus a settle each — the same per-step cost that made scanning
  slow, now paid per tab rather than per cell;
* leaving the browser on a different tab than the user left it on, or restoring
  it and hoping that lands;
* and doing all of it *while a run is in progress*, which §4.10's "does not lock
  the window" is directly about.

**Recommendation: keep the honest message, defer the deeper fix.** Not because
it is hard, but because every available mechanism is worse than the problem: a
run that rearranges the user's browser to find a document is a bigger
intrusion than a run that asks them to bring it forward.

If it is ever built, the shape that avoids the intrusion is not tab-walking at
all — it is removing the need to resolve a window, the way the scan already
did. `check_for_new_records` reads its source from a CSV export and resolves no
windows, so **Check for new is already immune**. A destination writer cannot
work that way (writing needs the live grid), but a *reader* on the run path
could, which would halve the exposure.

## Part A, shipped 2026-08-16: the message no longer lies

`open_for` now inspects window titles when matching fails. A window titled
"… and N more pages" is announcing hidden tabs, and that is enough to stop
asserting something false:

> could not reach the source document `1ko7z65…`. It MAY be open in a
> background tab — a window here reports having more pages behind the one it is
> showing, and only the front tab of a window can be found. Bring that document
> to the front, or give it its own window, and try again

Verified by reproducing the exact scenario — the document open in a background
tab among eleven — and running the real `open_for` against the stored template:

```
mentions a background tab : true
asserts it is not open    : false
```

Deliberately hedged with "MAY". The title says a window has hidden pages; it
does not say which document is in them. Claiming the document is definitely
there would replace one confident wrong answer with another. When no window
reports extra pages, the original blunt message still appears — it is correct
in that case, and hedging every failure would make the hedge meaningless.
