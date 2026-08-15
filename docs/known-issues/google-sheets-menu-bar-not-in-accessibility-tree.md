# Google Sheets' menu bar is not in the accessibility tree

**Status:** open (external). Measured 2026-08-14.
**Breaks:** `examples/text_capture_probe.rs sheetstrash`, which is the repo's
only automated cleanup for throwaway spreadsheets.
**Impact:** cleanup only. No product code drives the Sheets menus.

## What happens

`sheetstrash` drives **File → Move to trash**. It looks for a `role:MenuItem`
or `role:Button` named exactly `"File"`, and on ten consecutive scratch
documents it reported:

```
could not locate the File menu; leaving this document alone.
```

It was right to refuse. `menudump` on the same window shows why:

```
role:MenuItem:   0 node(s)
role:MenuBar:    0 node(s)
role:Menu:       0 node(s)
role:PopupButton:0 node(s)
role:Button:    34 node(s)
```

Those 34 are browser chrome and toolbar controls — `Share`, `Undo (Ctrl+Z)`,
`Print (Ctrl+P)`, `Add Sheet`, `Sheet1`, `Hide the menus (Ctrl+Shift+F)`. There
is **no File, Edit, View, Insert or Format anywhere in the tree**, even though
`Hide the menus` proves the menu bar is on screen.

This is the same shape as the app's own UI before
[app-own-ui-not-in-accessibility-tree.md](app-own-ui-not-in-accessibility-tree.md):
rendered, visible, and absent from automation. There the fix was a launch flag
on our own webview. Here the page belongs to Google, so there is no equivalent
lever.

## What was tried and did not work

**Sheets' menu search (Alt+/).** It goes through the page's own input handling
rather than the accessibility tree, so it should not care what is exposed. A
`trashkbd` mode sent `%{/}`, typed `Move to trash` and pressed Enter; the trash
banner never appeared and the probe reported failure rather than success, as it
should. The mode was removed rather than left behind — a cleanup tool that
always reports failure invites someone to trust it.

**Unverified:** whether `%{/}` is even the right key string. `press_key`'s
modifier syntax is undocumented in the crate, and this repo has already
recorded one surprise in it — `press_key` prefixes Enter-like keys with
`{LEFT}{END}`, which is why `run::spreadsheet` commits with Tab. So "Alt+/ does
not work" is **not** established; "this spelling of it did not" is.

## Doing it by hand

Faster than fixing this, and the reason this is low priority: select the files
in <https://drive.google.com/drive/my-drive> and press Delete. Throwaway
documents from the probes are all titled `Untitled spreadsheet`.

Every probe that creates one prints its id at the end under `doc id for
cleanup:`, so the ids are recoverable from probe output.

## If it is worth fixing later

1. **Confirm the key spelling** against a control that is easy to observe
   before spending it on a destructive action.
2. **Drive's file list rather than the document.** `drive.google.com` is an
   ordinary web page; selecting a row and pressing Delete may expose more than
   the Sheets editor does. Unmeasured.
3. **Stop creating them.** Several probes call `https://sheets.new` per run,
   which is why ten accumulated in one evening. A probe that reuses one scratch
   document, or one that is pointed at an existing one by id (as `editmode`
   already supports), leaves nothing to clean up.

(3) is the one that actually removes the problem.
