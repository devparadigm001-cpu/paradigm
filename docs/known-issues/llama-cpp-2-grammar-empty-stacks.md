# llama-cpp-2: grammar-constrained decoding aborts the process

**Status:** reproduced locally, **root cause not isolated**, not reported upstream.
**Affected:** `llama-cpp-2` 0.1.153 / `llama-cpp-sys-2` 0.1.153 (vendored llama.cpp).
**Platform:** Windows 11, MSVC, CPU-only build. Not tested elsewhere.
**Found:** 2026-08-04, during the Phase 1 Step 4a inference probe.

## Summary

Any GBNF grammar that needs **more than one rule** causes llama.cpp to abort the
whole process:

```
llama.cpp/src/llama-grammar.cpp:940: GGML_ASSERT(!stacks.empty()) failed
```

This is `abort()`, not an error return. It cannot be caught from Rust — the
process dies with `STATUS_STACK_BUFFER_OVERRUN` (`0xC0000409`).

"More than one rule" includes rules llama.cpp generates *implicitly*: a `*`, `?`
or a parenthesised group each create an anonymous rule. So in practice this
disables grammar constraining for anything except a single rule of literal
terminals.

## What was tested

All six run through the identical binary, model, and decode loop. Only the
grammar text differs.

| Grammar | Result |
|---|---|
| `root ::= "{" "}"` | **works** — generated `{}`, stopped correctly |
| `root ::= "{" [a-z]* "}"` | abort |
| `root ::= "{" inner "}"` + `inner ::= "abc"` | abort |
| `root ::= "{\"label\": " string "}"` + `string ::= "\"" [^"]* "\""` | abort |
| Our `{"label": "<string>"}` grammar, with and without leading `ws` | abort |
| **The crate's own shipped `src/grammar/json_arr.gbnf`** | abort |

The last row matters most: that grammar ships inside `llama-cpp-2` itself and is
canonical llama.cpp JSON GBNF. Its failure rules out our GBNF being at fault.

## What is established about the mechanism

- The abort is in `llama_grammar_reject_candidates`, which asserts its `stacks`
  argument is non-empty.
- It fires on the **first** sample call, before any token is generated — so the
  stack set is empty at *initialisation*, not exhausted part-way through.
- Neither `"failed to parse grammar"` nor `"grammar does not contain a 'root'
  symbol"` is logged. Those are the only two paths that make
  `llama_grammar_init_impl` return `nullptr`, so the grammar object was built
  and the text parsed.
- `llama-cpp-2` does not mangle the grammar string: `sanitize_grammar_strings`
  only checks that the root symbol appears and that there are no NUL bytes,
  then wraps in `CString`.

Note for future debugging: llama.cpp reports grammar errors through its logger.
Calling `send_logs_to_tracing` without registering a tracing subscriber makes
those messages vanish, which is indistinguishable from "no error occurred". Use
`--verbose` on the probe, which deliberately leaves llama.cpp's own stderr
logging in place.

## Root cause: NOT determined

Three live candidates, not yet distinguished:

1. A bug in the vendored llama.cpp revision's grammar stack construction.
2. A drift between `llama-cpp-2`'s C shim and the llama.cpp it vendors. The shim
   (`wrapper_common.cpp`) calls `llama_sampler_init_grammar(vocab, str, root)`
   with a `llama_vocab*`; if that signature or its semantics changed upstream,
   the result could be a valid-but-degenerate grammar with no parse error.
3. Something specific to this Windows/MSVC/CPU build.

A breakage this fundamental would be highly visible in upstream llama.cpp, which
weakly favours (2) or (3) — but that is a hunch, not evidence.

**To settle it:** build llama.cpp's own `test-grammar-integration` from the
vendored source in `~/.cargo/registry/src/.../llama-cpp-sys-2-0.1.153/llama.cpp`
and feed it the same GBNF. If it passes there but fails through the crate, the
fault is in the binding.

## Consequence and workaround

Grammar constraining was the mechanism for guaranteeing model output is valid
JSON. Without it, the guarantee weakens from **enforced** to **verified**:

- *Enforced* — llama.cpp masks every token that would violate the grammar, so
  malformed output is unrepresentable.
- *Verified* — the model emits freely and we check afterwards; malformed output
  is possible, we just detect it.

The probe now does strict parse-and-validate (`serde_json`, must be an object,
exactly one key, key `label`, value a string), plus a single repair step that
extracts the first balanced `{...}` substring and validates that.

**There is deliberately no retry.** At the locked temperature of 0.0 decoding is
deterministic, so re-running the same prompt returns byte-identical output. A
retry loop would spend latency to fail identically. Any real recovery must
change the input — a corrective re-prompt — or accept non-determinism by raising
temperature. Neither is implemented.

Observed behaviour so far: 2 of 2 inferences produced valid JSON unaided. That
is not evidence of reliability at any scale, and should not be treated as such.

## Retesting

The grammar path is intact behind an env var, so retesting after a version bump
costs nothing:

```
PARADIGM_TRY_GRAMMAR=1 cargo run --release --example inference_probe
PARADIGM_GRAMMAR='root ::= "{" "}"' cargo run --release --example inference_probe
```

`PARADIGM_GRAMMAR` overrides the grammar text, which is how the table above was
produced.

## Next steps

- [ ] Decide whether to isolate the root cause (build `test-grammar-integration`).
- [ ] Retest against a newer `llama-cpp-2` before Step 4b relies on validation alone.
- [ ] If grammar stays unavailable, decide whether Step 4b needs corrective
      re-prompting or should reject unparseable labels outright.
