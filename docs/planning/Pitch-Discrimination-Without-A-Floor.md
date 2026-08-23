# Pitch discrimination without a floor

**Status:** designed and prototyped 2026-08-22, scored against real layouts.
Not built into the product.
**Prototype:** `src-tauri/examples/pitch_discrimination_probe.rs`.
**Replaces:** `detect::candidates::RECORD_PITCH_FLOOR_PX`, proven defective in
`docs/known-issues/the-pitch-floor-returns-a-harmonic-instead-of-declining.md`.

## Why a floor cannot work, restated precisely

`record_pitch` votes on the most common pairwise y-difference above a floor `F`.
Two failures pull the floor in opposite directions:

* **Too high.** For a list of `n` rows at pitch `p`, every difference is `k*p`,
  the floor admits only `k >= ceil(F/p)`, and support for `k*p` is `n-k`, which
  strictly decreases with `k`. So the winner is always `ceil(F/p)*p` — a
  harmonic, deterministically. Measured at 4x, 5x, 5x and 4x on File Explorer,
  ftp.gnu.org, and the Wikipedia table.
* **Too low.** OrderFlow's within-record spread is **135px** against a
  between-record pitch of **209px**. Field offsets enter the vote and can win.

There is no `F` between those, because *the numbers overlap*: OrderFlow needs
`F > 135` and File Explorer needs `F < 32`. **The floor is a length, and page
densities differ by more than an order of magnitude.** No length is correct for
all of them, which is a structural statement rather than a tuning complaint.

## The proposal: periodicity, not magnitude

A repeating list's y values are a union of arithmetic progressions sharing one
common difference: field `i` of record `n` sits at `base_i + n*p`. The **set** is
therefore periodic with period `p` — and a within-record offset is not a period
of the set. Shifting every y by 90px does not land the set on itself; shifting by
209px does.

**coverage(p)** = of the points that could map (`y + p <= max`), the fraction
whose image is also a point.

Measured separation on real data, and it is not marginal:

```
OrderFlow, all elements          p=  15px  coverage 0.30   <- within-record offset
                                 p=  21px  coverage 0.32   <- within-record offset
                                 p=  24px  coverage 0.32   <- within-record offset
                                 p=  30px  coverage 0.32   <- within-record offset
                                 p= 209px  coverage 1.00   <- the true pitch
                                 p= 418px  coverage 1.00   <- harmonic

OrderFlow, clicked fields only   p=  90px  coverage 0.60   <- within-record offset
                                 p= 119px  coverage 0.50   <- within-record offset
                                 p= 209px  coverage 1.00   <- the true pitch
```

**0.30–0.60 against 1.00.** The threshold sits in a wide empty gap rather than
being fitted to a boundary.

## Three filters, each earned by a measured failure

The first prototype scored 4/6 and its two failures are the reason two of these
exist. Neither was anticipated; both were found by running it.

1. **coverage >= 0.80.** Rejects within-record offsets.
2. **at least three blocks** (`p <= span/2`). Rejects a large period over a few
   scattered points, which "covers" trivially because almost nothing is eligible
   to map — the non-list fixture returned a confident 1190px without this. The
   Rule of 3 wants three records anyway, so a period that cannot produce three is
   useless even when real.
3. **fewest offset clusters, then smallest p.** Under the true pitch,
   `(y - y_min) mod p` collapses to one cluster per field — seven fields, seven
   clusters. Under a near-miss period the offsets drift by the error on every
   record and smear into many more. This is what rejected a spurious 185px on
   OrderFlow, which coverage alone admitted because 185 = 209 - 24 and the
   layout happens to contain 24px offsets.

"Smallest p" as the tie-break is what makes a harmonic unreachable: coverage is
monotone (`coverage(p) >= coverage(k*p)`), so a fundamental that passes is always
preferred over its own multiples. **The proven defect is gone by construction,
not by tuning.**

## Scored against real layouts

| layout | truth | shipped | proposed |
|---|---|---|---|
| OrderFlow, all elements | 209px | 150px | **209px** |
| OrderFlow, clicked fields only | 209px | 209px | **209px** |
| File Explorer, Projects | 32px | 128px | **32px** |
| ftp.gnu.org directory index | 26px | 130px | **26px** |
| Wikipedia table rows | 33px | 133px | **33px** |
| not a repeating list | none | None | **None** |

**2/6 to 6/6.** Five of the six are measured layouts, not invented ones.

## What it still cannot do

**A genuinely ambiguous set stays ambiguous.** Two-line rows — lines 16px apart,
rows 32px apart — produce a y set that is uniformly 16px and genuinely periodic
at both 16 and 32. Nothing in y alone can choose, and the offset-cluster rule
should actively prefer 16, because one cluster is more economical than two.

**Confirmed by execution, in a validated replication.** The Rust probe could not
be run -- Application Control blocked the rebuilt binary through twenty minutes
of retries -- so the algorithm was replicated in JavaScript and checked against
the six cases whose Rust output is already known. It reproduced all six exactly,
which is what makes its seventh answer worth anything:

```
  OrderFlow, all elements      truth  209px   got   209px  ok
  OrderFlow, clicked only      truth  209px   got   209px  ok
  File Explorer, Projects      truth   32px   got    32px  ok
  gnu.org directory index      truth   26px   got    26px  ok
  Wikipedia table rows         truth   33px   got    33px  ok
  not a repeating list         truth   none   got    None  ok
  two-line rows (AMBIGUOUS)    truth   32px   got    16px  XX
```

**16px against a truth of 32px.** The predicted failure, executed rather than
argued. Note what it is NOT: not a harmonic, and not a confident answer over
nothing -- it is the smaller of two periods that are both genuinely present. The
fix is x, and until x is used this case is wrong.

The information needed is **x**: under the true pitch every block shows the same
field layout across x, and under the half-pitch alternate blocks differ. That
extension is sketched, not built, and it is the obvious next piece.

**The threshold is still a constant.** But it is a *dimensionless* one, in [0,1],
and unlike a length it does not change when a layout is rendered at a different
scale. That is the structural improvement; it is not a claim that 0.80 is proven.
Five real layouts is evidence, not coverage.

**Cost is O(n^2) candidates x O(n) coverage.** Fine at the scale that matters —
the input is clicked positions, tens rather than thousands — but it should be
measured before shipping, not assumed, given how the last cost assumption went.

## Why this is not built yet

The rule is testable in isolation and correct on everything measured, but it
changes which records `assign_records` produces on **every** page, including the
ones already working. Landing it wants the same treatment the last two fixes got:
a real recording before and after, on a surface where the answer is known
independently.
