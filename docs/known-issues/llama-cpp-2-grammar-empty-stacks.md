# llama-cpp-2: grammar-constrained decoding aborts the process

**Status: investigated further 2026-08-11; still open, deliberately.** Root cause
is **narrowed, not isolated**: one of the three candidates is refuted, the call
path to the assert is identified, and the failing-grammar characterisation below
turned out to be **wrong** and is corrected. Still reproduces on the newest
release (0.1.154). The workaround is sufficient for how the product actually uses
this, so the remaining work is an upstream report — drafted under "Upstream
report, ready to file", not filed. See "Second investigation".
**Affected:** `llama-cpp-2` 0.1.153 / `llama-cpp-sys-2` 0.1.153 (vendored llama.cpp).
**Platform:** Windows 11, MSVC, CPU-only build. Not tested elsewhere.
**Found:** 2026-08-04, during the Phase 1 Step 4a inference probe.

## Summary

> **Corrected 2026-08-11:** the "more than one rule" framing in this section is
> wrong — `root ::= "a"` aborts too. What is certain is that grammar-constrained
> decoding aborts the process for almost every useful grammar. See "Second
> investigation", point 3.

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

> **Partly superseded.** The table's grouping is right about *what* fails but the
> conclusion drawn from it — "more than one rule" — does not survive
> re-measurement: a one-rule one-literal grammar also aborts. See "Second
> investigation", point 3.

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
- ~~It fires on the **first** sample call, before any token is generated — so the
  stack set is empty at *initialisation*, not exhausted part-way through.~~
  **Superseded:** the assert is reached from `llama_grammar_apply_impl` on the
  sampling path, which runs on every sample rather than only the first, so this
  does not establish where the emptiness comes from. See "Second investigation",
  point 4.
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

## Second investigation (2026-08-11)

Four things established, one of them a correction to this document.

### 1. It still reproduces on the newest release

Bumped to `llama-cpp-2` 0.1.154 (a 16-minute vendored rebuild) and re-ran the
same two-rule grammar:

```
...llama-cpp-sys-2-0.1.154\llama.cpp\src\llama-grammar.cpp:940:
GGML_ASSERT(!stacks.empty()) failed
```

Identical assert, identical line, in the 0.1.154 vendored tree. **The bump was
then reverted**: it fixes nothing and would cost every developer a 16-minute
rebuild for no benefit. The retest is recorded here instead.

### 2. Candidate 2 — shim/vendor drift — is REFUTED

`common` is a **default feature** of `llama-cpp-2`, so the build takes the
`#[cfg(feature = "common")]` branch and calls the crate's own C shim
`llama_rs_sampler_init_grammar` rather than upstream's function directly. That
made shim drift the leading suspect. It is not: the shim is a pure passthrough.

```cpp
extern "C" struct llama_sampler * llama_rs_sampler_init_grammar(
    const struct llama_vocab * vocab, const char * grammar_str, const char * grammar_root) {
    try {
        return llama_sampler_init_grammar(vocab, grammar_str, grammar_root);
    } catch (...) { return nullptr; }
}
```

Same signature as the vendored `llama.h` declares, same arguments, no
transformation. Nothing drifts. That leaves candidates 1 and 3.

### 3. "More than one rule" is the WRONG characterisation

The table above says a single rule of literal terminals works. Re-measured on
0.1.154, that is not what separates the working case from the failing ones:

| Grammar | Rules | Result |
|---|---|---|
| `root ::= "{" "}"` | 1 | **works** |
| `root ::= "a"` | 1 | **abort** |
| `root ::= [a-z]` | 1 | **abort** |
| `root ::= "a" \| "b"` | 1 | **abort** |

A single rule containing a single literal aborts, while a single rule containing
*two* literals works. Rule count is not the variable. Anyone reasoning from the
original table — including anyone writing an upstream report from it — would be
describing a pattern that does not exist.

### 4. The assert is reached from the sampling path, not from initialisation

This document says the stack set is empty "at *initialisation*, not exhausted
part-way through". The call path says otherwise.
`llama_grammar_reject_candidates` has exactly two callers, and the one on the
sampling path is `llama_grammar_apply_impl`:

```cpp
const auto rejects = llama_grammar_reject_candidates(grammar.rules, grammar.stacks, candidates_grammar);
```

It is called **unconditionally**, with no guard for `grammar.stacks.empty()` —
even though the same function has just computed `allow_eog`, which is precisely
the state that matters when a grammar has been satisfied. So the assert is
reachable from ordinary sampling whenever `grammar.stacks` is empty at that
moment, whatever emptied it.

Note the contrast a few lines away: `llama_grammar_accept_str` *does* check
`stacks.empty()` and raises a catchable `std::runtime_error`. The apply path
aborts the process instead.

### What was NOT established, and why the investigation stopped there

**Why `stacks` is empty for those particular grammars.** Distinguishing candidate
1 (the vendored llama.cpp's stack construction) from candidate 3 (this
Windows/MSVC/CPU build) still needs what the original doc proposed: building
llama.cpp's own `test-grammar-integration` from the vendored source, or attaching
a debugger.

That was judged not worth doing now, for reasons that are about this project
rather than about the bug:

* **Nothing in the product depends on grammar.** It is gated behind
  `PARADIGM_TRY_GRAMMAR`, and the shipped path is validate-and-repair
  (`labeling::validate_or_repair`), which is sufficient for the single small
  JSON object the labeller actually asks for.
* **Both remaining candidates are upstream's to fix.** Neither changes anything
  we would do locally; they change what an upstream maintainer looks at first.
* **The cost is real and the payoff is theirs.** A CMake/MSVC build of
  llama.cpp's test suite is speculative work on a third-party defect, on top of
  a 16-minute rebuild already spent, when the report can be filed without it.

## Upstream report, ready to file

Everything needed is here; it could not be submitted from this session (filing
needs a personal GitHub account).

**Where:** `utilityai/llama-cpp-rs` (the `llama-cpp-2` crate). Possibly
`ggml-org/llama.cpp` if the maintainers confirm the vendored source is at fault —
the evidence below does not settle which.

**Title:** Grammar-constrained sampling aborts the process:
`GGML_ASSERT(!stacks.empty())` in `llama_grammar_apply_impl`

**Versions:** `llama-cpp-2` 0.1.153 **and** 0.1.154 (both reproduce);
`llama-cpp-sys-2` same. Default features (`common` enabled).

**Platform:** Windows 11, MSVC, CPU-only build. Not tested elsewhere — worth
saying plainly, since candidate 3 is exactly "this build".

**Model:** `qwen2.5-0.5b-instruct-q4_k_m.gguf`, temperature 0, greedy.

**Symptom:** `abort()`, not an error return — unrecoverable from Rust, process
dies with `STATUS_STACK_BUFFER_OVERRUN` (`0xC0000409`).

```
llama.cpp/src/llama-grammar.cpp:940: GGML_ASSERT(!stacks.empty()) failed
```

**Minimal reproduction:** construct `LlamaSampler::grammar(&model, g, "root")`,
chain with temp+greedy, and sample. With `g = "root ::= \"a\""` it aborts. With
`g = "root ::= \"{\" \"}\""` it does not. Both are single-rule grammars.

**The strongest single data point:** the crate's own shipped
`src/grammar/json_arr.gbnf` aborts. That rules out the reporter's GBNF being at
fault.

**What has been ruled out, so the maintainer need not re-check it:**

* Not a GBNF authoring error — the crate's own shipped grammar fails.
* Not a parse failure — neither `"failed to parse grammar"` nor `"grammar does
  not contain a 'root' symbol"` is logged, and those are the only paths returning
  `nullptr` from `llama_grammar_init_impl`.
* Not string mangling — `sanitize_grammar_strings` only checks for the root
  symbol and NUL bytes.
* Not shim drift — `llama_rs_sampler_init_grammar` is a pure passthrough to
  `llama_sampler_init_grammar` with a matching signature.
* Not rule count — a one-rule one-literal grammar aborts while a one-rule
  two-literal grammar works.

**Suggested direction:** `llama_grammar_apply_impl` calls
`llama_grammar_reject_candidates` without checking `grammar.stacks.empty()`,
while `llama_grammar_accept_str` checks the same condition and throws a catchable
error. Whatever empties the stacks, an assert on the sampling path turns a
recoverable state into a process abort.

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

- [x] ~~Decide whether to isolate the root cause (build `test-grammar-integration`).~~
      **Decided: not now.** Narrowed instead -- candidate 2 refuted, the assert's
      call path identified, the failing-grammar characterisation corrected. The
      remaining step distinguishes two upstream-owned causes and changes nothing
      locally. See "Second investigation".
- [x] ~~Retest against a newer `llama-cpp-2` before Step 4b relies on validation alone.~~
      **Done: 0.1.154 still aborts**, identical assert. Bump reverted -- no fix,
      and a 16-minute rebuild for everyone otherwise.
- [ ] If grammar stays unavailable, decide whether Step 4b needs corrective
      re-prompting or should reject unparseable labels outright.
