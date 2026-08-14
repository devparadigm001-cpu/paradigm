# Auto-labelling fails with "EOF while parsing a string"

**Status:** open, low priority. Observed during a real recording 2026-08-14.
**Impact:** naming only. The user types a name instead, and the recording saves
normally — `compile_and_store_playbook` takes a `name_hint` and only calls the
model when one is absent.

## What was seen

```
model output was not usable: not valid JSON: EOF while parsing a string
```

## The likely cause, from reading the code — not yet confirmed against a real failure

The generation budget is **15 tokens**:

```rust
pub const MAX_TOKENS: usize = 15;   // labeling/mod.rs
```

and the model must produce a complete JSON object inside it:

```json
{"label": "Log into portal and submit form"}
```

The braces, quotes, the `label` key and the colon cost several tokens before any
of the name is emitted. A label a few words long can therefore run out of budget
**mid-string**, and a JSON document cut off inside a string literal produces
exactly `EOF while parsing a string`. Every other malformed-output case
(`top level is not an object`, `missing "label" key`, `expected exactly one key`)
has its own distinct message, and this was not one of them — which is what
points at truncation rather than the model returning something structurally
wrong.

**This is inference from the error text and the constant, not a measurement.**
It has not been reproduced.

## Why the output is not guaranteed valid in the first place

Grammar-constrained decoding — which would make invalid JSON unrepresentable
rather than merely unlikely — is **deliberately off**. GBNF grammars abort the
process in llama-cpp-2 0.1.153 for any grammar needing more than one rule,
including the crate's own shipped `json_arr.gbnf`. See
[llama-cpp-2-grammar-empty-stacks.md](llama-cpp-2-grammar-empty-stacks.md). The
grammar and its code path are retained for when that is fixed.

So today the model is *asked* for JSON by prompt alone, and validated
afterwards. Truncation is one of the ways that can fail.

## What would settle it in about five minutes

`Completion` already records why generation stopped:

```rust
stopped_by = Some(format!("max_tokens={MAX_TOKENS}"));   // vs Some("<eog>")
```

So the next failure only needs that field printed alongside the error. If it
reads `max_tokens=15`, the diagnosis is confirmed and the fix is a larger
budget; if it reads `<eog>`, the model genuinely emitted malformed JSON and the
prompt is the thing to look at.

`examples/inference_probe.rs` already drives the model directly and is the
cheapest place to reproduce: run it against a description long enough to force
a multi-word label and see whether the output is cut off.

## Why the fix is not just "raise MAX_TOKENS"

It probably is, but the number is not arbitrary. It bounds how long the user
waits on a local model at the end of every unnamed recording, and a larger
budget also lets the model ramble past a good label. Raising it should come
with a check that the label is still a short name rather than a sentence —
`validate` enforces the shape but not the length.

## Related

The same model and the same `complete()` path back `detect::verify`'s
mapping-sensibility check (§4.1). That path asks for `{"sensible": "yes"}`,
which is far shorter and much less likely to truncate — but it shares this
budget, so anything done here should be checked against both callers rather
than tuned for labelling alone.
