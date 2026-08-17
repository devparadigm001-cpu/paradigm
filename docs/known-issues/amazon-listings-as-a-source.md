# Amazon listings as a source: easy to read, ambiguous to interpret

**Status:** discovery, 2026-08-16. **No workflow logic was built.**
**Site chosen by the user**, from a set of no-login product-listing options.
**Probes:** `text_capture_probe amzntree [here]`, `amzncapture`.
**Measured against:** `https://www.amazon.com/s?k=wireless+mouse`, real page, in
the Edge profile this project has used throughout.

> **This investigation destroyed data in a scratch spreadsheet.** See
> "The incident" at the bottom. It is recorded here rather than quietly fixed,
> because the mechanism is only half understood.

## The short answer

Third source shape investigated, after a canvas grid (Sheets) and a message list
(Gmail). It inverts the Sheets problem completely.

**Sheets was hard to read and unambiguous once read.** A canvas grid with no
per-cell element — but `B2` means exactly one thing.

**Amazon is trivial to read and genuinely ambiguous.** Every field is a named
element on first inspection. But "the price" has no single answer, and the wrong
answer looks exactly like the right one.

Identity, meanwhile, is the strongest of the three sources: a real product id,
readable from the list, without opening anything.

## 1. The results page

1614 accessible nodes (Sheets: 56; Gmail inbox: 937).

```
  479  Text        118  Group        4  Document
  435  Hyperlink    54  List         3  Edit
  298  ListItem     30  Pane         2  ComboBox
  169  Button       12  Image        2  Slider
```

The probe guards on content before measuring anything: it aborts on a bot-check
page, and aborts if fewer than three price-shaped elements exist. Measuring a
CAPTCHA page as though it were results would be worse than failing.

### (a) A listing is a real element — but not a consistent one

Listings were located structurally rather than by guessing a role: walk **up**
from a price-shaped element until the subtree also contains a long-named
`Hyperlink` (the product title). That definition found them — and showed they do
not share a shape:

```
  found at 1 hop(s) up: Group     id="407461" name=""
  found at 1 hop(s) up: Group     id="141041" name=""
  found at 4 hop(s) up: ListItem  id="187851" name="INPHIC Wireless Mouse for Laptop…"
```

Two unnamed `Group`s one hop up, and a `ListItem` **four** hops up whose name is
the product title. There is no single "listing" role and no single depth. Any
reader has to define a listing by structure, not by role — which is workable, and
is more than Sheets ever offered.

### (b) Fields are individually readable

Title, rating, review count, price, delivery date and badges are all separate
named elements:

```
ListItem   "INPHIC Wireless Mouse for Laptop, 2.4G Rechargeable Comput…"
  Hyperlink  "INPHIC Wireless Mouse for Laptop, 2.4G Rechargeable Comput…"
     text="https://www.amazon.com/INPHIC-…/dp/B0GV25DTDV/ref=sr_1_…"
  Button     "4.4 out of 5 stars, rating details"
  Hyperlink  "10 ratings"
  Text       "Price, product page"
  Hyperlink  "$15.99"
  Text       "You pay $11.99"
  Text       "with coupon"
```

### (c) The trap, and it is about meaning rather than structure

Sheets' trap was reading a cell *reference* where a *value* was meant. Gmail's
was overlapping duplicate names. Amazon's is worse than both, because the wrong
value is indistinguishable from the right one.

**"The price" is three different shapes across three adjacent listings:**

| listing | price elements |
|---|---|
| sponsored | `Hyperlink "$9.99"` + `Text "List:"` + `Text "$12.99"` |
| organic, deal | `Hyperlink "$13.79 List: $17.99"` — **two prices, one name** |
| organic, coupon | `Hyperlink "$15.99"` + `Text "You pay $11.99"` + `Text "with coupon"` |

A reader taking "the first price-shaped element in the listing" returns the sale
price for one, a concatenation of two prices for another, and the pre-coupon
price for a third — while the *actual* price the customer pays is `$11.99`, in a
separate element, only in the third case.

Every one of those outputs is a well-formed price. Nothing about the result
signals that three different questions were answered. This is the
plausible-wrong-value class, with no tell.

Shared and duplicated names are pervasive as well, exactly as in Gmail:

```
  235 distinct name(s) appear more than once
     x81  "Popular Shopping Ideas"
     x36  "Price, product page"
     x35  "Add to cart"
     x34  "Join Prime" / "Tue, Aug 18" / "Fri, Aug 21"
```

And shared **ids** recur across different listings — `Text "Price, product page"`
carries id `118404` in more than one listing, the same trap as Gmail's `601850`
star cell. Ids are right for some elements and meaningless for others.

## 2. Identity — the strongest of the three sources

```
  433 element(s) expose a URL through text()
   51 element(s) carry a /dp/<id> product path

  d=15  Hyperlink  asin="B00PGB7OKM"
  d=15  Hyperlink  asin="B0GV25DTDV"
  d=15  Hyperlink  asin="B07CMS5Q6P"
```

The ASIN is a **stable, content-free product identifier, readable directly from
the list view** without opening anything. That is precisely what Gmail could not
provide — Gmail's list exposed no id at all, and its message id required opening
the message and mutating the mailbox.

Roughly three hyperlinks per listing carry the same ASIN, so it is redundantly
available.

**The gap: sponsored listings have no ASIN.** Their links point at ad trackers:

```
  Hyperlink "wegear USB Wireless Mouse…"
     text="https://aax-us-east-retail-direct.amazon.com/x/c/JACRsTnXZVWD2tz2muxpT1MAAAG…"
```

Confirmed per listing by the extractor: `asin=NONE (sponsored?)` for the
sponsored result, real ASINs for the two organic ones. So an ASIN-keyed ledger
covers organic results and silently omits sponsored ones — which is arguably
correct behaviour, but it is a decision, not a default.

## 3. What Record Mode captured

`amzncapture` ran a real `CaptureSession` across the real task. Where writes
landed, capture was correct and correctly attributed:

```
  type  role=ComboBox  name="D2"  payload="Logitech M185 Compact Ambidextrous Wireless Mouse…"
  type  role=ComboBox  name="E2"  payload="$13.79 List: $17.99"
  type  role=ComboBox  name="D3"  payload="INPHIC Wireless Mouse for Laptop, 2.4G Rechargeable…"
  type  role=ComboBox  name="E3"  payload="$15.99"
```

It also recorded `click role=group name="-"` — an anonymous in-page click, the
same shape as the Sheets `role:pane` finding in
`complex-web-grid-capture-unreliable.md`.

**Not measured:** what capture records when a human clicks and drags through
Amazon to select and copy text. Same open question as Gmail. Do not assume the
named elements survive into capture just because they exist in the tree.

## 4. Two real defects this surfaced

**Currency write verification fails.** `wrote "$9.99" but the cell reads "9.99"`
— a successful write reported as a failure. Filed separately as
`a-reformatted-value-fails-write-verification.md`; it is not an Amazon problem.

**A write intended for `D1` landed in `A1`.** Capture recorded
`type role=ComboBox name="A1"` twice, with the product title as payload. The
Name Box navigation to `D1` did not take, and the value was typed into whatever
cell was current. This is the silent-wrong-target class, observed live, and it is
**not explained**. `goto` has retry and a `position_lost_message`, and neither
prevented it here.

## The incident

Running `amzncapture` left the scratch document `1g3lvtsYyGc…` with columns A and
B **empty**. Before, from this project's own export earlier the same evening:

```
1| Customer,Amount
2| Blue Horizon Supply,1150
3| Redwood Manufacturing,675.25
4| Silverline Consulting,920.5
5| Cedar Point Logistics,340
6| Marigold Retail Group,1580.75
7| Jahnavi,1680
8| Hello,1000
```

After: `,,,` on every row, plus a leftover product title in `D3` that the
clearing loop failed to remove.

The `A1` mis-write above accounts for `A1` only. **What emptied `A2:B8` is not
established.** The clearing loop targeted `D1:E3` exclusively; a `{Delete}` sent
while a larger range was selected is a plausible mechanism and is *not*
confirmed. No mechanism is asserted here, because guessing at one would make the
next person stop looking.

Recovery was left to Google Sheets' version history rather than attempted by
retyping, since version history restores exactly — including anything this
project has no record of.

**What this says about the probes, and it is the useful part:** `amzncapture`
wrote into a document that held real data, using a writer whose navigation can
silently target the wrong cell, and a clearing step that presses `{Delete}` on
whatever is selected. `gmailcapture` has the same shape and got away with it. A
probe that mutates a shared document should seed its own throwaway document
instead, and that is the change to make before either is run again.

## Reproducing

```
text_capture_probe amzntree [term]        # full tree dump of the results page
text_capture_probe amzntree here          # measure the page already on screen
text_capture_probe amzncapture            # DO NOT RUN against a document you care about
```

`amzntree` clicks nothing and mutates nothing. `amzncapture` writes to a
spreadsheet and, on this evidence, cannot be trusted to confine itself to the
cells it intends.
