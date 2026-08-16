# Capturing the CSV export in memory, without a file

**Status:** investigated 2026-08-16, **not pursued**. Not because it is
impossible, but because the measurement that motivates it does not hold.
**Where:** `source::csv_snapshot::fetch_export_blocking`.

## The short answer

The file is not the cost. A fetch takes ~1s and **~0.88s of that is the browser
round-trip before the file exists at all**. Capturing the response in memory
would remove a write, a read and a delete — a small slice of that second, not
the second itself.

Both routes to doing it are also real work with real footprints, so the payoff
would be small and the price high.

## 1. Does the automation library expose network responses? No

`terminator-rs` is a UI-Automation library. There is no CDP client, no network
interception, no response-body access — searching its source for `network`,
`cdp`, `devtools`, `response_body`, `intercept` and `har` returns nothing but
unrelated test names.

It does expose `execute_script`, and its first line of documentation is the
catch:

```rust
/// Execute JavaScript in browser using extension bridge ONLY
```

It routes through `extension_bridge::try_eval_via_extension`, which talks to a
**browser extension** over a local WebSocket at `127.0.0.1:17373`. The crate
ships one (`browser-extension/`) whose manifest requests a content script on
`"matches": ["<all_urls>"]`.

That route would genuinely work — in-page `fetch()` of the export URL uses the
page's own session and returns the body to the caller, no file involved. What it
costs is asking the user to install a browser extension with all-URLs access,
and running a local WebSocket server alongside the app. That is a separate piece
of work with a privacy footprint of its own, not a refactor of the fetch.

## 2. Raw HTTP from Rust with the browser's cookies? Blocked, and unwise

Three separate obstacles, any one of which is enough:

* **No HTTP client exists here.** The dependency tree has no `reqwest`, `ureq`
  or `hyper` — the only network access in this codebase is the browser. Adding
  one is a decision in itself.
* **The cookies are not readable.** Edge's `Local State` contains an
  `app_bound_encrypted_key`, Chromium's App-Bound Encryption, which exists
  specifically to stop another process decrypting the cookie store the way the
  older DPAPI approach did. *(The key's presence was confirmed by reading the
  file; the key itself was not decoded, so its exact scheme version is
  unverified.)*
* **It would be the wrong thing to do anyway.** Reading a user's entire cookie
  jar to fetch one spreadsheet is a serious escalation for an app whose §3
  posture is that durable data stays structural. A session-less request already
  returns **401**, so nothing short of real credentials would work.

## 3. The measurement that settles it

`text_capture_probe polltest` starts the export and polls every 50ms to find
when the file truly appears:

```
trial 1: 1.21s      trial 2: 0.88s
trial 3: 0.89s      trial 4: 1.03s
true latency: min 0.88s, average 1.00s
```

The production fetch polls once a second, so its floor is 1.00s — meaning the
polling granularity costs about **0.12s**, and the remaining ~0.88s is the
browser round-trip: launching or reaching the browser, the request, the
response, the save.

In-memory capture removes the file write, read and delete. Those are a small
part of ~1s, and the round-trip they sit inside would remain.

## What is actually available, and it is marginal

Tightening the poll from 1s to ~100ms would recover roughly 0.12s per record —
about a second across a nine-record run. Real, measurable, and small. Worth
doing if the fetch loop is touched for another reason; not worth a change on its
own.

## Reproducing

```
text_capture_probe polltest <doc> 4     # true download latency vs poll floor
```
