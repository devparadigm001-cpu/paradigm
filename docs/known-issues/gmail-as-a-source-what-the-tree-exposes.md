# Gmail as a source: what the accessibility tree actually exposes

**Status:** discovery, 2026-08-16. **No workflow logic was built.** This answers
"is Gmail realistically readable this way, and how hard is it compared to
Sheets", and nothing more.
**Probes:** `text_capture_probe gmailtree [here]`, `gmailopened`, `gmailcapture`.
**Measured against:** a real signed-in mailbox, in the Edge profile this project
has used throughout.

## The short answer

**Gmail is dramatically easier to read than Sheets, and harder to *resume*.**

Sheets' problem was that nothing existed: a canvas grid, 56 nodes, no per-cell
element, identity reachable only through the Name Box. Gmail has the opposite
shape — 937 nodes for an inbox, every message a real named element, every field
a separate child. The read problem is close to solved on arrival.

What Gmail does **not** have is the thing Sheets gave away for free: a stable,
content-free identity per record. A spreadsheet row is "row 47" — structural,
durable, and safe to store under §3. A Gmail message in the list view has **no
identifier of any kind**. That, not reading, is the hard problem here.

## 1. The inbox list view

937 accessible nodes under the Gmail window (Sheets, for comparison: 56).

```
  450  DataItem        21  TabItem
  112  Image            4  Tab
   88  Button           3  Edit
   82  Group            1  DataGrid
   66  Hyperlink        1  Document
   51  CheckBox         1  ToolBar
   31  Pane             1  Window
   23  Text
```

### (a) Is a message a real, named element? YES — twice over

A message row is a `DataItem` carrying a composed name, **and** its fields are
separate child `DataItem`s. One real row, in full:

```
DataItem  "unread, Edikted , SAVE 60-80%: CLEARANCE Best Sellers 💕 , 10:30 AM , The Most…"
                                                        bounds=(2299,487,552,29)
  DataItem  ""                                          bounds=(2299,491,4,21)
  DataItem  "unread, Edikted , SAVE 60-80%: … , 10:30 AM , The Most…"
    CheckBox  "unread, Edikted , SAVE 60-80%: … , 10:30 AM , The Most…"
  DataItem  "Not starred"
    Button    "Not starred"
  DataItem  "Edikted"                            <- SENDER
  DataItem  "SAVE 60-80%: CLEARANCE Best Sellers 💕 \u{a0}-\u{a0} The Most Popular…"
    Hyperlink "SAVE 60-80%: … \u{a0}-\u{a0} The Most Popular…"   <- SUBJECT + SNIPPET
  DataItem  "\u{a0}"                             <- attachment column, empty
  DataItem  "Sun, Aug 16, 2026, 10:30 AM"        <- FULL DATE
  DataItem  ""                                   bounds=(0,0,1,1)
```

Sender, subject and date are individually addressable, with real bounds. This is
nothing like the Sheets grid.

Two caveats that matter for any mapping:

* **Subject and snippet share one element**, joined by `\u{a0}-\u{a0}`. Splitting
  them is string work on a findable delimiter, not a structural read.
* **The list's date is abbreviated in the composed name** (`10:30 AM`) but the
  dedicated date child carries the full `Sun, Aug 16, 2026, 10:30 AM`. Read the
  child, not the composed string.

### (c) The Gmail equivalent of the Name Box trap — it exists, and it bit this probe

The Sheets failure was reading a cell *reference* where a *value* was meant.
Gmail's version is structural: **the row, its selection checkbox, and its real
fields all carry overlapping names**, and the wrong pick still produces
plausible-looking data.

This is not hypothetical. The first run of `gmailcapture` used
"take the first `DataItem` under the row" and wrote this into the spreadsheet as
a **sender**:

```
D1 = "Edikted , SAVE 60-80%: CLEARANCE Best Sellers 💕 , 10:30 AM , The Most
      Popular Pieces Of This SALE Are Selling Fast 🛍️ Get Them Now, Or Regret
      It Later... RUN 🤍 💨 ͏ ͏ ͏ ͏ ͏ …"
```

The cause: the enumeration yields the row **itself** at depth 0, and the
selection `CheckBox` repeats the row's whole composed name. So three different
elements answer to something that looks like "the row", and only one decomposes
into fields. It landed in the sheet, it was confirmed by CSV export, and it was
wrong — the exact silent-plausible-corruption shape this project keeps hitting.

Duplicate names are pervasive, which makes name-based selectors dangerous:

```
  176 distinct name(s) appear more than once
     x100  "Not starred"
     x18   "Edikted"
     x16   "AutoForward"
     x6    "We found a match for you! Is this "The One?" …"
```

`"Not starred"` matching 100 elements is the headline: a selector on it is
meaningless. Sender names repeat legitimately, because senders send more than
once.

## 2. An opened message

Opening drops the tree to 251–362 nodes. **Every field is distinct.**

```
  Text      "SAVE 60-80%: CLEARANCE Best Sellers 💕"     <- SUBJECT
  ListItem  "Edikted hello@edikted.comUnsubscribe10:30 AM (6 hours ago)to me…"
    Group   "Edikted hello@edikted.com Unsubscribe"
      Text  "Edikted hello@edikted.com Unsubscribe"
        DataItem  "Edikted hello@edikted.com"           <- SENDER name + address
        Hyperlink "Unsubscribe"
    Group   "10:30 AM (6 hours ago) Not starred"
      DataItem  "10:30 AM (6 hours ago)"                <- DATE
    Group   "to me Show details"
      Text  "to "
      Text  "me"                                        <- RECIPIENT
```

### (b) Is the body one blob? NO — measured on two different email shapes

| | nodes | body shape |
|---|---|---|
| HTML marketing email | 251 | many `Hyperlink` + `Image` elements, no prose blob |
| forwarded plain-text email | 362 | 95 `Text` elements, roughly one per paragraph |

The plain-text case, which is the one that could plausibly have been a blob:

```
  Text  "W2W Fwd: Shift Today"                        <- subject
  Text  "AutoForward@m1.whentowork.com"               <- sender
  Text  "to " / Text "me"
  Text  "The City of Plano - Tom Muehlenbeck Recreation Center"
  Text  "WhenToWork.com message from … forwarded at your request."
  Text  "Hi,"
  Text  "Could someone please pick up my 1-6pm shift, …"   [content redacted]
  Text  "WhenToWork, LLC"
  Text  "Note: You received this message because …"
```

Paragraph-level granularity. Reassembling a body means concatenating in tree
order — real work, but ordinary work.

**One trap worth stating:** `text(0)` is **never** body text. On a `Hyperlink` it
returns the href (one measured at 454 chars); on the `Document` it returns the
page URL. Body text lives in `name`. A reader built on `text()` would return
URLs and look like it was working.

**The sender element's format varies.** `"Edikted hello@edikted.com"` (display
name + address) versus `"AutoForward@m1.whentowork.com"` (address only). Any
sender/address split has to tolerate both.

## 3. Identity — the actual hard problem

### In the list view there is no message id. At all.

Every descendant of every message row was dumped with every accessor. The result
is unambiguous:

```
  DataItem   name="Edikted"                          text="" id="954379"
  Hyperlink  name="SAVE 60-80%: … "                  text="" id="332614"
  DataItem   name="Sun, Aug 16, 2026, 10:30 AM"      text="" id="900112"
```

**`text` is empty on every element in the list**, including the subject
`Hyperlink` — there is no href, no URL, no id. Nothing in the list view names a
message.

### The message id exists only once the message is OPENED

```
  Edit      "Address and search bar"
     text="https://mail.google.com/mail/u/0/#inbox/FMfcgzQhVrJTtlfrzjbcLrScFbsJcRxM"
  Document  "SAVE 60-80%: CLEARANCE Best Sellers💕 - …"
     text="https://mail.google.com/mail/u/0/#inbox/FMfcgzQhVrJTtlfrzjbcLrScFbsJcRxM"
```

Better than the Sheets `gid` case in one respect: the URL is on the in-page
`Document`, not only in browser chrome, so reading it is not browser-specific.

But the URL embeds the **current view** — opened from a search it reads
`#search/AutoForward/FMfcgzQ…`. Only the trailing token is the message.

And obtaining it **requires opening the message, which marks it read.** Reading
the identity mutates the mailbox.

### Runtime ids: partly message-bound, partly shared — a genuine trap

UIA runtime ids were tested rather than assumed, because they looked promising.

| | inbox view | search view (same message, different position) |
|---|---|---|
| sender `DataItem` (AutoForward) | `736162` | **`736162`** |
| row wrapper | (row 2) | `673013` |
| `"Not starred"` `DataItem` | `601850` | **`601850`** |
| spacer `DataItem` | `124770` | **`124770`** |

Two opposite behaviours in one table:

* The **sender** element's id followed the message across a position change and
  across a page reload.
* The **structural** cells — star, spacer, attachment column — carry ids that are
  *identical across different messages*. `601850` is the star cell of every row.

So "use the element id" is right for some cells and catastrophically wrong for
others, and both look equally stable if you only check that the number doesn't
change. **Honest limit:** this is a handful of observations inside one browser
session. UIA runtime ids are not contractually durable across processes, and a
browser restart was not tested. Nothing should be built on them.

### What "next unread" can actually be built on

**The `"unread, "` name prefix is real and reliable.** An unread row's composed
name begins with it; a read row's does not. Confirmed by consequence rather than
by inspection: the Edikted row read `"unread, Edikted , …"` before this probe
opened it, and `"Edikted , …"` afterwards. 81 rows carried it in the rendered
list.

The opened view corroborates with a `Button "Mark as unread"` — present only when
the message is currently read.

So:

| Question | Answer |
|---|---|
| "which messages are unread right now?" | **Answerable**, from the list, cheaply |
| "which messages did I already process?" | **Not answerable** from the list |

That gap is the real finding. §4.7's ledger keys on `(playbook_id, source_id,
row_key)` where `row_key` is structural — "row 47". Gmail offers no structural
key. The candidates are all bad in different ways:

1. **Message id** — genuinely stable, but requires opening every message, which
   marks it read and mutates the user's mailbox before deciding whether to
   process it.
2. **Content hash of (sender, subject, date)** — works without mutation, but
   makes the ledger a store of message content, which is exactly what §3's
   "durable data is structural" refuses. It is also not unique: 18 rows share the
   sender `"Edikted"`, and repeated newsletters share subjects too.
3. **Runtime ids** — not durable by contract; see the trap above.
4. **Read state as a proxy** — "process everything unread, and processing marks
   it read." Requires no ledger at all, and the mailbox itself becomes the
   record. Fragile in an obvious way: anything else that reads the mail, on any
   device, silently changes what the next run sees.

None of these was chosen. Option 4 is the cheapest and the most honest fit for
what Gmail actually exposes, and its failure mode is the one a user would most
easily understand — but it is a decision, not a finding.

## 4. What Record Mode actually captures

`gmailcapture` runs a real `CaptureSession` across the real task: read sender and
subject off two real emails, write them into a scratch spreadsheet, verify by CSV
export, clear up afterwards.

```
5 action(s), 272 unmapped event(s), 0 paste(s) observed

  navigate  role=Window     name="Untitled spreadsheet - Google Sheets …"
  type      role=ComboBox   name="D1"  payload="Edikted"
  type      role=ComboBox   name="E1"  payload="SAVE 60-80%: CLEARANCE Best Sellers 💕"
  type      role=ComboBox   name="D2"  payload="AutoForward"
  type      role=ComboBox   name="E2"  payload="W2W Fwd: Shift Today"

  ground truth from the export:
    row 1: D="Edikted"     E="SAVE 60-80%: CLEARANCE Best Sellers 💕"
    row 2: D="AutoForward" E="W2W Fwd: Shift Today"
```

The destination half is fully captured, correctly attributed per cell, and
CSV-confirmed. That is the existing `GridCellWatcher` path working as built.

**The Gmail half produced no captured action, and that is expected rather than
alarming** — this probe read Gmail through the accessibility tree, which is not
user input, so there was nothing for capture to observe. **What was NOT measured:
what capture records when a human actually clicks and drags through Gmail to
select and copy text.** Given `sheetstabclick` found that a click inside the
Sheets page captures as an anonymous `role:pane`, the equivalent question for
Gmail is open and worth its own probe. Do not assume Gmail's named elements
survive into capture just because they exist in the tree.

The known clipboard gap applies unchanged: a paste into a grid cell is still
lost, counted only as `pastes_observed`.

## 5. How hard is this compared to Sheets?

**Reading: much easier.** Sheets needed a transient editor overlay, a Name Box
indirection, U+FEFF stripping, a commit-edge trigger, and eventually a CSV export
path because per-cell reads cost ~1s each. Gmail hands over sender, subject, date
and body as named elements on first inspection. No canvas, no overlay, no export
needed to read.

**Two mechanical problems, both ordinary:**

* Picking the right element among overlapping duplicates — the trap in §1(c).
  Solvable with a precise structural rule, and it needs a real test, because
  the wrong rule produces plausible output.
* `click()` does not work on list rows: every attempt returned
  `Element is not visible`, including after `activate_window`, and including on a
  freshly re-resolved handle. **`invoke()` works.** Unexplained, and recorded as
  such rather than guessed at.

**Resuming: harder than Sheets, and it is a design question, not an engineering
one.** Everything above is about reading a message. Knowing which messages a
previous run already handled has no clean answer in what Gmail exposes, and the
options trade mailbox mutation against storing content the privacy model says
should not be stored. That decision should be made deliberately before any
Gmail source reader is built, because it determines the shape of the reader.

## Reproducing

```
text_capture_probe gmailtree          # list view + open one email, full dump
text_capture_probe gmailtree here     # same, but measure the view already on screen
text_capture_probe gmailopened        # read-only: enumerate whatever is on screen
text_capture_probe gmailcapture       # Gmail -> spreadsheet with capture running
```

`gmailtree` opens one message and therefore marks it read. `gmailopened` mutates
nothing. `gmailcapture` writes D1:E2 of the scratch document and clears them
again, confirming both by export.
