//! Local model labeling: turn a cleaned pattern description into a short
//! human-facing label.
//!
//! This is the logic proven by `examples/inference_probe.rs` in Step 4a, moved
//! here so the example and the pipeline share one implementation. Same crate
//! (`llama-cpp-2`), same locked execution parameters, same validate-then-repair
//! fallback.
//!
//! ## Grammar constraining is off
//!
//! GBNF grammar constraining aborts the process in llama-cpp-2 0.1.153 for any
//! grammar needing more than one rule -- including the crate's own shipped
//! `json_arr.gbnf`. See `docs/known-issues/llama-cpp-2-grammar-empty-stacks.md`.
//! The grammar and its code path are retained so re-enabling after a version
//! bump is one env var. Until then JSON validity is VERIFIED, not ENFORCED.

pub mod calibration;
pub mod clean;
pub mod redact;

use std::path::Path;
use std::time::{Duration, Instant};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

pub use calibration::CalibrationSample;
pub use clean::{clean, CleanedPattern};
pub use redact::{RedactionPolicy, RedactionReason, RedactionRecord, REDACTED};

/// Locked execution parameters. Changing these invalidates the Step 4a proof.
pub const TEMPERATURE: f32 = 0.0;
pub const MAX_TOKENS: usize = 15;
pub const STOP_SEQUENCES: &[&str] = &["\n", "}"];
pub const N_CTX: u32 = 2048;

/// Retained but unused by default. See the module docs.
pub const LABEL_GRAMMAR: &str = r#"
root   ::= ws "{" ws "\"label\"" ws ":" ws string ws "}"
string ::= "\"" ( [^"\\] | "\\" ["\\/bfnrt] )* "\""
ws     ::= ([ \t] ws)?
"#;

#[derive(Debug, thiserror::Error)]
pub enum LabelingError {
    #[error("model file not found: {0}")]
    ModelNotFound(String),

    #[error("llama backend: {0}")]
    Backend(String),

    #[error("model load failed: {0}")]
    Load(String),

    #[error("context creation failed: {0}")]
    Context(String),

    #[error("inference failed: {0}")]
    Inference(String),

    #[error("model output was not usable: {0}")]
    Output(String),
}

/// One labeling call's result, including the signals calibration needs.
#[derive(Debug, Clone)]
pub struct LabelOutcome {
    pub label: String,
    pub raw_output: String,
    /// True if the raw output only validated after brace-repair.
    pub repaired: bool,
    pub tokens_generated: usize,
    pub stopped_by: Option<String>,
    pub inference_time: Duration,
    /// Time to build a fresh context. Reported separately so the cost of NOT
    /// keeping a context warm is visible rather than hidden in inference time.
    pub context_time: Duration,
    pub model_source: String,
    /// Mean probability of the generated tokens under the model's own
    /// distribution, in 0.0..=1.0. This is the raw confidence score that
    /// `confidence_calibration` exists to normalise in Phase 2.
    pub mean_token_probability: f64,
}

/// The process-wide engine.
///
/// This is a singleton by necessity, not just by preference:
/// `LlamaBackend::init()` flips a global `AtomicBool` and returns
/// `BackendAlreadyInitialized` on any second call, so a second
/// `LabelingEngine` cannot be constructed in the same process. The "load once,
/// keep the model warm" requirement and that constraint are the same thing.
///
/// A failed load is cached too, so every caller gets the same answer rather
/// than retrying a 468 MiB load that will fail again.
static SHARED: std::sync::OnceLock<Result<LabelingEngine, String>> = std::sync::OnceLock::new();

/// Borrow the process-wide engine, loading it on first call.
pub fn shared(model_path: &Path) -> Result<&'static LabelingEngine, String> {
    SHARED
        .get_or_init(|| LabelingEngine::load(model_path).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| e.clone())
}

/// A resident model. Loaded once; every call reuses it.
pub struct LabelingEngine {
    backend: LlamaBackend,
    model: LlamaModel,
    n_threads: i32,
    model_source: String,
    load_time: Duration,
}

impl LabelingEngine {
    /// Load the model and keep it resident.
    pub fn load(model_path: &Path) -> Result<Self, LabelingError> {
        if !model_path.exists() {
            return Err(LabelingError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }

        let backend = LlamaBackend::init().map_err(|e| LabelingError::Backend(e.to_string()))?;

        // n_gpu_layers(0) makes CPU-only explicit rather than incidental.
        let params = LlamaModelParams::default().with_n_gpu_layers(0);

        let start = Instant::now();
        let model = LlamaModel::load_from_file(&backend, model_path, &params)
            .map_err(|e| LabelingError::Load(e.to_string()))?;
        let load_time = start.elapsed();

        let model_source = model_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);

        Ok(Self {
            backend,
            model,
            n_threads,
            model_source,
            load_time,
        })
    }

    pub fn model_source(&self) -> &str {
        &self.model_source
    }

    pub fn load_time(&self) -> Duration {
        self.load_time
    }

    /// Label a cleaned pattern description.
    ///
    /// Takes `&self`, so the caller holds one engine and calls this repeatedly;
    /// the model is never reloaded. A fresh context per call keeps the borrow
    /// checker honest (a `LlamaContext` borrows the model, so storing both in
    /// one struct would be self-referential) and costs `context_time`, which is
    /// reported so the tradeoff is measurable rather than assumed.
    pub fn label(&self, description: &str) -> Result<LabelOutcome, LabelingError> {
        let ctx_start = Instant::now();
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(std::num::NonZeroU32::new(N_CTX))
            .with_n_threads(self.n_threads)
            .with_n_threads_batch(self.n_threads);
        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| LabelingError::Context(e.to_string()))?;
        let context_time = ctx_start.elapsed();

        let prompt = build_prompt(description);
        let raw = infer(&self.model, &mut ctx, &prompt)?;

        let (parsed, repaired) = validate_or_repair(&raw.output);
        let label = parsed.map_err(LabelingError::Output)?;

        Ok(LabelOutcome {
            label,
            raw_output: raw.output,
            repaired,
            tokens_generated: raw.tokens_generated,
            stopped_by: raw.stopped_by,
            inference_time: raw.duration,
            context_time,
            model_source: self.model_source.clone(),
            mean_token_probability: raw.mean_token_probability,
        })
    }
}

/// The locked prompt shape: three worked examples, then the pattern to label.
pub fn build_prompt(input_pattern: &str) -> String {
    format!(
        "Input: \"User copies spreadsheet row then pastes into SAP window, repeated 4 times.\"\n\
         Output: {{\"label\": \"Copy + Paste to SAP?\"}}\n\
         Input: \"User opens patient invoice PDF then forwards to accounting email, repeated 3 times.\"\n\
         Output: {{\"label\": \"Forward Invoice?\"}}\n\
         Input: \"User clicks Save Form then clicks Close Window, repeated 5 times.\"\n\
         Output: {{\"label\": \"Save + Close?\"}}\n\
         Input: \"{input_pattern}\"\n\
         Output:"
    )
}

struct RawInference {
    output: String,
    duration: Duration,
    tokens_generated: usize,
    stopped_by: Option<String>,
    mean_token_probability: f64,
}

fn infer(
    model: &LlamaModel,
    ctx: &mut llama_cpp_2::context::LlamaContext,
    prompt: &str,
) -> Result<RawInference, LabelingError> {
    let start = Instant::now();
    let map_err = |e: &dyn std::fmt::Display| LabelingError::Inference(e.to_string());

    let tokens = model
        .str_to_token(prompt, AddBos::Always)
        .map_err(|e| map_err(&e))?;
    let mut batch = LlamaBatch::new(tokens.len().max(64), 1);
    let last = tokens.len() as i32 - 1;
    for (i, token) in (0_i32..).zip(tokens.iter().copied()) {
        // Only the final prompt token needs logits.
        batch.add(token, i, &[0], i == last)
            .map_err(|e| map_err(&e))?;
    }
    ctx.decode(&mut batch).map_err(|e| map_err(&e))?;

    // See module docs: grammar is disabled by default because it aborts.
    let use_grammar = std::env::var("PARADIGM_TRY_GRAMMAR").is_ok();
    let grammar_text =
        std::env::var("PARADIGM_GRAMMAR").unwrap_or_else(|_| LABEL_GRAMMAR.to_string());
    let mut samplers = Vec::new();
    if use_grammar {
        samplers.push(
            LlamaSampler::grammar(model, &grammar_text, "root").map_err(|e| map_err(&e))?,
        );
    }
    // temp(0.0) is safe, not a divide-by-zero: llama.cpp keeps the maximum
    // logit and sets the rest to -inf for t <= 0. With greedy() that is exactly
    // deterministic argmax, so the locked temperature is stated, not implied.
    samplers.push(LlamaSampler::temp(TEMPERATURE));
    samplers.push(LlamaSampler::greedy());
    let mut sampler = LlamaSampler::chain_simple(samplers);

    // One decoder for the whole generation: a multi-byte character can be split
    // across two tokens, and a per-token decoder would corrupt it.
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut output = String::new();
    let mut n_cur = batch.n_tokens();
    let mut generated = 0usize;
    let mut stopped_by = None;
    let mut prob_sum = 0.0f64;

    while generated < MAX_TOKENS {
        let logits_idx = batch.n_tokens() - 1;
        let token = sampler.sample(ctx, logits_idx);
        sampler.accept(token);

        // Confidence signal: the model's own probability for the token it just
        // chose, read from the same logits it was chosen from.
        prob_sum += f64::from(token_probability(ctx.get_logits_ith(logits_idx), token.0));

        if model.is_eog_token(token) {
            stopped_by = Some("<eog>".to_string());
            break;
        }

        output.push_str(
            &model
                .token_to_piece(token, &mut decoder, false, None)
                .map_err(|e| map_err(&e))?,
        );
        generated += 1;

        // Stop sequences are terminators that are KEPT, not stripped: "}" is
        // the closing brace of the JSON object.
        //
        // Match by EARLIEST POSITION IN THE OUTPUT, not by order in
        // STOP_SEQUENCES -- output containing "\n" before "}" would otherwise
        // be truncated past its closing brace.
        if let Some((idx, stop)) = STOP_SEQUENCES
            .iter()
            .filter_map(|s| output.find(*s).map(|i| (i, *s)))
            .min_by_key(|(i, _)| *i)
        {
            output.truncate(idx + stop.len());
            stopped_by = Some(stop.to_string());
            break;
        }

        batch.clear();
        batch.add(token, n_cur, &[0], true).map_err(|e| map_err(&e))?;
        n_cur += 1;
        ctx.decode(&mut batch).map_err(|e| map_err(&e))?;
    }

    if stopped_by.is_none() {
        stopped_by = Some(format!("max_tokens={MAX_TOKENS}"));
    }

    // Count the token that triggered the stop, so the mean reflects every
    // sampled token rather than only the ones that reached the output.
    let sampled = generated.max(1);
    Ok(RawInference {
        output,
        duration: start.elapsed(),
        tokens_generated: generated,
        stopped_by,
        mean_token_probability: (prob_sum / sampled as f64).clamp(0.0, 1.0),
    })
}

/// Softmax probability of `token_id` under `logits`.
fn token_probability(logits: &[f32], token_id: i32) -> f32 {
    let idx = token_id as usize;
    if idx >= logits.len() {
        return 0.0;
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !max.is_finite() {
        return 0.0;
    }
    let sum: f32 = logits.iter().map(|l| (l - max).exp()).sum();
    if sum <= 0.0 {
        return 0.0;
    }
    ((logits[idx] - max).exp()) / sum
}

/// Validate, and if that fails, repair once by extracting the first balanced
/// `{...}` and validating that.
///
/// No retry loop: decoding is deterministic at temperature 0.0, so re-running
/// the same prompt returns byte-identical output. Repair is the only fallback
/// that can change the outcome without changing the input.
pub fn validate_or_repair(raw: &str) -> (Result<String, String>, bool) {
    match validate(raw) {
        Ok(label) => (Ok(label), false),
        Err(first) => match extract_balanced_object(raw) {
            Some(candidate) => match validate(&candidate) {
                Ok(label) => (Ok(label), true),
                Err(e) => (Err(format!("{first}; repair also failed: {e}")), true),
            },
            None => (
                Err(format!("{first}; no balanced {{...}} found to repair")),
                false,
            ),
        },
    }
}

/// First balanced brace-delimited substring, brace-counting so a `{` inside the
/// label string cannot terminate it early.
pub fn extract_balanced_object(raw: &str) -> Option<String> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in raw[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(raw[start..start + offset + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Confirm the output really is `{"label": "<string>"}`.
pub fn validate(raw: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw.trim()).map_err(|e| format!("not valid JSON: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "top level is not an object".to_string())?;
    if obj.len() != 1 {
        return Err(format!("expected exactly one key, found {}", obj.len()));
    }
    let label = obj
        .get("label")
        .ok_or_else(|| "missing \"label\" key".to_string())?;
    label
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("\"label\" is not a string: {label}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_well_formed_output() {
        let (r, repaired) = validate_or_repair(r#" {"label": "Save + Close?"}"#);
        assert_eq!(r.unwrap(), "Save + Close?");
        assert!(!repaired);
    }

    #[test]
    fn repairs_trailing_junk() {
        let (r, repaired) = validate_or_repair(r#"{"label": "Approve?"} trailing garbage"#);
        assert_eq!(r.unwrap(), "Approve?");
        assert!(repaired, "should have needed repair");
    }

    #[test]
    fn rejects_wrong_shape() {
        let (r, _) = validate_or_repair(r#"{"title": "nope"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn brace_inside_label_does_not_terminate_early() {
        let extracted = extract_balanced_object(r#"{"label": "a } b"} tail"#).unwrap();
        assert_eq!(extracted, r#"{"label": "a } b"}"#);
    }

    #[test]
    fn prompt_embeds_the_pattern() {
        let p = build_prompt("User clicks X, repeated 1 time.");
        assert!(p.contains("User clicks X, repeated 1 time."));
        assert!(p.ends_with("Output:"));
    }
}
