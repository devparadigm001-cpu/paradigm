# Capture: typed text is silently truncated in recorded actions

**Status:** reproduced **twice, independently**, **root cause not investigated**.
**Affected:** `terminator-workflow-recorder` 0.23.35, text-input event assembly.
**Platform:** Windows. Observed in Microsoft Edge; other apps untested.
**Found:** 2026-08-04, during Phase 1 Step 6 replay testing. Reproduced again
the same day during Step 7's end-to-end IPC test.
**Severity: MEDIUM, rising.** Step 7 is complete and unaffected — its wiring
faithfully carried what capture handed it. Must be resolved before Step 12 (the
end-to-end human test) — see "Suggested priority" below. The second
reproduction raises confidence that this is systematic, not incidental.

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

One hypothesis fits both data points, and is worth testing first: the
completion event fires on a short debounce measured from the *first* keystroke,
capturing only the keystrokes that landed inside that window. It predicts a
variable prefix length that scales with typing speed and system load, which is
what was seen. It is a hypothesis consistent with two samples, not an
established cause — n=2 cannot distinguish it from several alternatives.

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

- [ ] **Reproduce reliably.** Seen twice, in 2 of the small number of typed
      actions across this session's probes — frequent enough that it is not a
      rare edge case, but the rate is still not quantified. Write a loop that
      types known strings of varying length and compares captured `text_value`
      against ground truth, recording the kept-prefix length each time.
- [ ] **Test the debounce hypothesis first.** Both reproductions kept a leading
      prefix of differing length, which fits "the completion event fires on a
      timer from the first keystroke". Vary the inter-keystroke delay: if the
      kept prefix grows as typing slows, that confirms it and points at a timing
      fix rather than a data-assembly one.
- [ ] **Determine whether it is timing or assembly.** The alternative is that
      the buffer is assembled incorrectly. The `1 keystroke(s)` versus
      `26 keystroke(s)` discrepancy for identical input is evidence worth
      chasing, since it is reproducible in the existing probe output.
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
