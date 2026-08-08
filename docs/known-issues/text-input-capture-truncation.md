# Capture: typed text is silently truncated in recorded actions

**Status: root cause identified; value corruption FIXED; a narrower miss
remains.** Investigated 2026-08-06 — see "Investigation" below for the
measurements. Typed text is no longer taken from the recorder's
`TextInputCompleted`; `src/capture/text.rs` reads the field directly.
**Affected:** `terminator-workflow-recorder` 0.23.35, text-input event assembly.
**Platform:** Windows. Observed in Microsoft Edge; other apps still untested.
**Found:** 2026-08-04, during Phase 1 Step 6 replay testing. Reproduced again
the same day during Step 7's end-to-end IPC test.
**Severity: MEDIUM.** The dangerous form of this defect — silently *wrong* text
that every later stage trusts — was not reproduced after the change: captured
payloads were exact in 40/40 trials and never partial at any typing speed.
What remains is a narrower race in which a typed action can be missed
**entirely** (never wrong), and it still bites `tests/ipc_pipeline.rs`.
A missing action is visible; a wrong one was not. See "Remaining limitation".

**Scope of that claim.** Every measurement here used synthetic `type_text()`
into web `<input>` elements in Microsoft Edge — the same narrow environment the
original observations came from. Native Win32 fields, WinUI (Notepad), a second
browser, and genuine human typing are all untested. Human typing matters most
and is least like what was measured: it is slower, interleaved with mouse
movement, and hits a different timing profile than any of these runs. Read
"FIXED" as "not reproducible in the measured environment", not as "cannot
happen".

## Summary

Typed text is recorded with only a leading prefix of what was actually entered.
Observed twice, with different lengths: `"probe-user"` (10 chars) captured as
`"p"`, and `"ipc-pipeline-test"` (17 chars) captured as `"ip"`.

The truncation is silent: nothing in the pipeline errors, warns, or marks the
value as partial. Every downstream stage treats the fragment as the faithful
record of what the user typed. Two independent reproductions make this look
systematic rather than a one-off glitch, which is what moved this from an
observation to a tracked defect.

## What was observed

### Reproduction 1 — Step 6 replay probe

The probe drove `type_text("probe-user", …)` into the page's Username field and
reported success. The captured action, read back from `run_steps_log`, was:

```
[step_order 4] event_type="execute" action_type="type"
    is_sensitive : false
    data_payload : "p"
    target_ctx   : {"app":"msedge.exe","detail":"typed 1 character(s)",
                    "name":"Username","result":"executed",
                    "selector":"role:Edit|name:Username"}
```

Replay then faithfully reproduced the recording — `typed 1 character(s)` — so
the defect propagated end to end without any stage detecting it. Replay was
correct here: it typed exactly what was stored. The recording was wrong.

### Reproduction 2 — Step 7 end-to-end IPC test

Independent of the first: a different test binary (`tests/ipc_pipeline.rs`), a
different string, a different session, and observed incidentally rather than
looked for. The test drove `type_text("ipc-pipeline-test", …)` — 17 characters —
into the same kind of field. The capture returned to the frontend was:

```json
{"action_type":"type","element_name":"Username","element_role":"Edit",
 "payload_preview":"ip","source_app":"msedge.exe","step_order":2}
```

Replay again reproduced it faithfully: `"detail":"typed 2 character(s)"`.

### What the two together rule out

| Run | Typed | Captured | Kept |
|-----|-------|----------|------|
| Step 6 | `"probe-user"` (10 chars) | `"p"` | 1 char |
| Step 7 | `"ipc-pipeline-test"` (17 chars) | `"ip"` | 2 chars |

**A fixed truncation length is ruled out** — 1 and 2 differ. A fixed column or
buffer-size limit would have produced the same length both times.

Both retained a **leading prefix**, not a suffix or an arbitrary slice. That is
consistent with the accumulator being read while it is still filling, rather
than with a corrupted or bounded buffer.

One hypothesis fitted both data points and was tested first: the completion
event fires on a short debounce measured from the *first* keystroke, capturing
only the keystrokes that landed inside that window. It predicts a variable
prefix length that scales with typing speed.

**REFUTED, 2026-08-06.** `examples/text_capture_probe` swept the
inter-keystroke delay across 0 / 50 / 150 / 300 ms. Kept-prefix length did not
track the delay at any setting. The refutation is stronger than "no
correlation": across a five-run baseline sweep the event was delivered for
**1 of 20** typed actions, and a timer from the first keystroke cannot produce
*no event at all*. The mechanism was also wrong — there is no accumulator to
read early. See "Investigation".

Both observations came from synthetic `type_text()` input into web `<input>`
elements in Microsoft Edge, so neither the input method nor the field type has
been varied yet.

### A related inconsistency in the same event

In a Step 4b run, the same event type reported a keystroke count that did not
match the text it carried:

```
payload: "paradigm-probe-1785818826423"      (28 characters)
detail : "Typed, 1 keystroke(s) over 2418ms"
```

A later run of the identical action reported `26 keystroke(s) over 5289ms` for
the same 28-character string. So `text_value` and `keystroke_count` are
assembled inconsistently with each other, and neither is reliably correct.
Whether this shares a cause with the truncation is unknown.

### Possibly related: events going missing entirely

Across this session's probes, a `TextInputCompleted` event for a password field
was captured in **1 of 4** live runs, despite the driven typing reporting
success every time. This may be the same underlying assembly or timing problem,
or something separate about masked fields. Not established either way; recorded
here because it was seen in the same subsystem during the same session.

## Where it likely lives

Upstream, in `terminator-workflow-recorder`'s text-input tracking
(`src/recorder/windows/mod.rs`), which accumulates keystrokes and emits a
`WorkflowEvent::TextInputCompleted` carrying `text_value`, `keystroke_count`
and `typing_duration_ms`.

Paradigm's own code takes that value verbatim. `src/capture/mod.rs`'s
`to_candidate()` maps `WorkflowEvent::TextInputCompleted` straight through:

```rust
payload: Some(e.text_value.clone()),
```

There is no truncation, length cap, or transformation on our side of that
boundary. Steps 4b, 5 and 6 all processed correctly whatever text they were
handed:

* **Step 4b (clean/tag)** summarises payloads to 40 characters for the prompt,
  but `"p"` is far below that cap and was passed through whole.
* **Step 5 (compile)** stored the payload verbatim in `action_payload_json`.
* **Step 6 (replay)** typed exactly the stored string.

None of them is at fault. The value was already wrong when it arrived.

## Investigation (2026-08-06)

### Root cause: there is no accumulator, and keystrokes are dropped on a lock

`TextInputTracker` (`recorder/windows/structs.rs:29`) stores the element, a
start time, a keystroke *count* and some flags. **It holds no character
buffer.** `text_value` is produced at emit time by
`TextInputTracker::get_completion_event`:

```rust
let text_value = match self.element.text(0) { ... }
```

That is a live UI Automation read of the field. So a truncated `text_value` is
not a partially-filled buffer — it is a read that landed while the field still
held a prefix.

Whether the event is emitted at all is gated by `keystroke_count`, and the
keystroke path drops keystrokes silently (`recorder/windows/mod.rs:1079`):

```rust
if let Ok(mut tracker) = current_text_input.try_lock() {
    text_input.add_keystroke(key_code);
}
```

A failed `try_lock` discards the keystroke — no retry, no queue. The UIA thread
holds that same mutex across slow element resolution (150 ms sleeps, 100 ms and
200 ms `recv_timeout`s). Lose every keystroke and `keystroke_count` stays 0, so
`should_emit_completion` refuses and **no event is produced**. Lose some and the
count is wrong, which is exactly the `"1 keystroke(s)"` reported for a
28-character string above. The recorder's own trace shows the lock failing:

```
❌ Could not lock text input tracker for transition
```

Corroboration: enabling the recorder's `tracing` output made the defect vanish
(4/4 events, all exact) — added log I/O shifts the timing. A Heisenbug of that
shape is a race, not a data-assembly bug.

Also observed in the same trace: `is_text_input_element` is very permissive,
starting trackers on a `Document` and on a `Button` named "Done".

### The fix

`src/capture/text.rs` follows focus across text fields and reads the field
directly — the same `element.text(0)` call, on our own trigger, with no
`try_lock` anywhere. `WorkflowEvent::TextInputCompleted` is no longer mapped to
an action at all. An action is emitted when the field's value changed since we
began watching **or** when typing keystrokes were observed into it; requiring
only the former loses text that arrived before the focus event was processed,
and requiring neither invents actions for fields merely clicked through.

### Measurements

`examples/text_capture_probe`, 20-character strings, ground truth verified by an
independent read; trials where the text never reached the field are excluded as
setup failures rather than scored.

| Configuration | Typed actions captured | Payload exact |
|---|---|---|
| `TextInputCompleted` (before) | **1/20 (5%)** | 1/1 |
| Direct read, settled typing (A–D) | **40/40 (100%)** | 40/40 |
| Direct read, no-settle fast typing (E) | **4/10** | 4/4 |

Independent element reads were correct in **every** trial of every run,
including at 0 ms inter-keystroke delay — that is the finding the fix rests on.
**No truncation was reproduced at any speed after the change**: captured
payloads were either exact or absent, never partial. That is an absence of
evidence across 40 trials in one environment (Edge, web `<input>`, synthetic
input), not proof that partial capture cannot occur elsewhere.

Clicking through a pre-filled field without typing recorded nothing in 5/5 runs,
so the emit condition does not fabricate actions.

### Remaining limitation

Trial E types the whole string in one `type_text` with no pause after the click.
`initial` is read when the *click event is processed*, and the recorder resolves
that element through UIA first, so with fast input the text can land before we
know which field to watch — the baseline then already contains the text and the
keystroke counter is also still zero, because both depend on watching having
started.

`tests/ipc_pipeline.rs` is shaped exactly this way and **still captures no
`type` action.** Real human typing is A–D shaped (a pause after clicking, then
per-keystroke input), where capture measured 40/40.

An attempt to fix this by resolving the focused element on the first keystroke
(`Desktop::focused_element`) was **measured and reverted**: it left E at 0/5 and
regressed A from 5/5 to 0/5. Recorded so the next attempt does not repeat it.

### Investigating that reverted attempt (2026-08-08)

A second session went back to ask *why* it regressed the settled case, which was
never established at the time. The answer is still not known — but the space has
been narrowed considerably, and one previously-recorded explanation turned out
not to survive testing.

**The code was never committed.** `git log -S "begin_from_keystroke"` returns
nothing on any branch; it existed only in a working tree and was reverted before
the surrounding commit. This investigation had to reconstruct it from a commit
message. A failed attempt is worth committing to a scratch branch — the source
would have made this much cheaper.

#### Correction: the "focus resolved to the browser's address bar" finding does not replicate

An earlier single-sample observation held that `focused_element()` on the first
keystroke resolved to the browser's own address bar rather than the target
field, and that this explained both halves of the failure. **Controlled testing
refutes it.**

`examples/text_capture_probe -- pumpcost` now drives two phases and records what
focus resolved to on *every* keystroke:

| Phase | Shape | First keystroke resolved to | Distinct targets across phase |
|---|---|---|---|
| 1 | settled (click, 400 ms, then type) | `role="Edit" name="FieldA"` | only `FieldA` |
| 2 | no settle (click, type immediately) | `role="Edit" name="FieldB"` | only `FieldB` |

**40 of 40 keystrokes resolved to the correct target field**, in both phases, at
4–7 ms per call (max 21 ms). Never an address bar, never a stale element. The
no-settle phase — the one where OS focus plausibly would not have settled —
resolved just as cleanly as the settled one.

Two explanations fit the earlier observation and this evidence cannot separate
them: it may have been an artifact of a data-loss bug in the diagnostic itself
(the probe called `pump.abort()` before `await`, so an aborted `JoinHandle`
returned `Cancelled` and silently discarded everything collected — fixed this
session), or a genuine one-off anomaly. Either way it is **not a reproducible
mechanism** and should not be treated as the root cause.

#### Correction: querying focus state on keystroke is NOT ruled out

An earlier instruction, recorded here so the correction is explicit, was that
this investigation should conclude that "query live focus state on keystroke" is
a dead direction. **The evidence points the other way and that conclusion is
withdrawn.** Focus resolution on keystroke is correct 40/40 and costs 4–7 ms,
which is mechanically sound and fast enough to sit in the event path. Whatever
broke the reverted fix, it was not this.

Closing off a direction the measurements support would have been the more
expensive mistake, so the direction stays open.

#### What is ruled out, and what is not

Ruled out, each by measurement rather than argument:

* **Unreliable focus resolution** — 40/40 correct, both shapes (above).
* **A different element handle causing a spurious flush.** The handle from
  `focused_element()` and the one on the recorder's Click event are the same:
  identical `id` (`Some("391359")`), `role`, and `name`, and both read text
  identically. `same_element()` returns `true`, so the later click would *not*
  have been mistaken for a move to another field.
* **COM apartment / cross-thread issues.** `Desktop::new_default()` succeeds
  inside the spawned pump task, and `focused_element()` returns correct results
  from it.
* **Broadcast-channel lag dropping events.** Zero `LAGGED` lines with a
  subscriber installed; 96 events against a channel capacity of 1000. (The
  channel *does* drop silently on lag — `Lagged(skipped) => continue`, reported
  through `tracing`, which goes nowhere without a subscriber — so this was worth
  checking rather than assuming.)
* **The call being slow enough to starve the pump.** Mean 7.1 ms, max 21.4 ms,
  283 ms total across 40 calls.

**Still unknown: why the settled case regressed from 5/5 to 0/5 while that fix
was active.** Every component tests sound in isolation, yet the composition
measurably failed across five consecutive runs — too consistent to be noise. No
supported mechanism has been found.

#### A new risk for anyone revisiting this

Before any click, `focused_element()` resolves to `role="Document"`. When the
reverted fix was written, `is_text_role("Document")` was **false**. It is now
**true**, added so Notepad's editing surface would be recognised. So a stray
keystroke before any click would now start watching the *page document*, and a
subsequent click would flush it — emitting the entire page text as a `type`
action.

Re-attempting this approach today is therefore **more dangerous than when it was
tried**, and any new attempt must gate on something narrower than
`is_text_role` alone.

#### The next experiment, if this is picked up again

Reconstruct the reverted fix *with instrumentation* — logging when watching
starts, via which route, and what each flush observes — and watch it fail. That
is the only remaining way to see the composition break, since none of its parts
break alone. It is a session's work and should not be rushed into a fix.

## Why it matters

Capture is the root of the pipeline, and every later stage inherits its errors
without being able to detect them:

* **Labeling (Step 4b)** describes the pattern using the captured text. A
  truncated value produces a description of something the user did not do, and
  the model labels that instead.
* **Compiled playbooks (Step 5)** persist the truncated value as the step's
  payload. The playbook is now permanently wrong, and nothing in validation can
  catch it — `"p"` is a perfectly valid string.
* **Replay (Step 6)** reproduces the truncation faithfully, so a playbook that
  looked correct when recorded types the wrong thing into a real form.

The failure mode is the dangerous kind: silent, plausible, and self-consistent.
A truncated payload looks exactly like a short one the user genuinely typed.

## Suggested priority

**Not blocking Step 7.** Steps 7 onward can be built and tested against this
behaviour, since the defect is in the data rather than in the interfaces.

**Must be investigated and fixed before Step 12**, the end-to-end human test.
That step's whole purpose is to confirm a real person can record a real workflow
and have it replay correctly. If captured text cannot be trusted, a Step 12
failure is uninterpretable — it would be impossible to tell a genuine
end-to-end problem from this known capture defect, and a Step 12 *pass* would be
equally uninformative, since it would only mean the truncation happened not to
bite that run.

## Next steps

- [x] **Reproduce reliably.** Done — `examples/text_capture_probe` quantified
      the baseline at 1/20 and the fix at 40/40 on settled typing.
- [x] **Test the debounce hypothesis first.** Done — refuted, see above.
- [x] **Determine whether it is timing or assembly.** Timing: a race on a
      `try_lock`, plus a live UIA read at emit. Not assembly — there is no
      buffer to assemble.
- [ ] **Close the no-settle race (trial E, 4/10).** This is the one that still
      bites `tests/ipc_pipeline.rs`. Note that `Desktop::focused_element` on the
      first keystroke has already been tried and made things worse.
- [ ] **Make `ipc_pipeline` assert on captured text.** It currently checks only
      `action_count > 0` and `step_count == action_count`, so it passed green
      through every variant of this defect, including capturing no typing at
      all. A test that cannot fail on the bug it covers is worse than no test.
- [ ] **Check field-type and browser sensitivity.** Only Edge, and only web
      `<input>` elements, have been observed. Test native Win32 fields, WinUI
      fields (Notepad's editor), and a second browser.
- [ ] **Check whether synthetic input is a factor.** Every observation so far
      came from `type_text()` driving the field rather than a human typing.
      Synthetic input arrives far faster than human keystrokes and may hit a
      timing path real usage would not — or may mask one that real usage hits
      harder. Test with genuine human typing before concluding.
- [ ] **Decide on a detection mechanism.** Even after a fix, capture could
      record the element's value after typing and compare it against the
      assembled `text_value`, flagging a mismatch rather than trusting the
      event. That turns a silent corruption into a visible one.
