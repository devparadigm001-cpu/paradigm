//! Phase 1 Step 4a: prove local model inference works.
//!
//!     cargo run --release --example inference_probe
//!
//! Thin wrapper over `paradigm_lib::labeling` -- the logic this probe proved in
//! Step 4a now lives in that module, and this file only drives it. If this
//! example and the pipeline ever disagree, it is a bug in one caller, not two
//! divergent copies of the inference loop.
//!
//! Proves:
//!   1. Q4_K_M GGUF loads on CPU, in-process (no llama-server subprocess).
//!   2. Output is valid JSON of the expected shape. NOTE: VERIFIED, not
//!      ENFORCED -- see docs/known-issues/llama-cpp-2-grammar-empty-stacks.md.
//!   3. The locked execution parameters are honoured.
//!   4. The model stays warm: two labels, one load.

use std::path::PathBuf;
use std::process::ExitCode;

use llama_cpp_2::LogOptions;
use paradigm_lib::labeling::{LabelingEngine, MAX_TOKENS, TEMPERATURE};

const MODEL_FILE: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";

fn main() -> ExitCode {
    let verbose = std::env::args().any(|a| a == "--verbose");
    if !verbose {
        // Routing to tracing with no subscriber registered is indistinguishable
        // from silence, which once hid llama.cpp's grammar errors. --verbose
        // deliberately leaves llama.cpp's own stderr logging in place.
        llama_cpp_2::send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));
    }

    let model_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL_FILE);
    println!("== local inference probe (qwen2.5-0.5b-instruct Q4_K_M) ==\n");
    println!("model : {}", model_path.display());

    let engine = match LabelingEngine::load(&model_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "loaded: {:.3}s (CPU only)   temperature={TEMPERATURE}, max_tokens={MAX_TOKENS}\n",
        engine.load_time().as_secs_f64()
    );

    if std::env::var("PARADIGM_TRY_GRAMMAR").is_err() {
        println!(
            "GRAMMAR DEFECT: constrained decoding DISABLED (llama-cpp-2 0.1.153 aborts).\n\
             JSON validity is verified, not enforced. PARADIGM_TRY_GRAMMAR=1 retests.\n"
        );
    }

    let patterns = [
        "User clicks Add New Tab then types text into it, repeated 1 time.",
        "User selects invoice line then clicks Approve button, repeated 6 times.",
    ];

    let mut ok = true;
    for (i, pattern) in patterns.iter().enumerate() {
        println!("-- inference {} --", i + 1);
        println!("input pattern: {pattern:?}");
        match engine.label(pattern) {
            Ok(o) => {
                println!(
                    "context      : {:.3}s   inference: {:.3}s   tokens: {}",
                    o.context_time.as_secs_f64(),
                    o.inference_time.as_secs_f64(),
                    o.tokens_generated
                );
                println!("raw output   : {:?}", o.raw_output);
                println!(
                    "label        : {:?}{}",
                    o.label,
                    if o.repaired { "  (AFTER REPAIR)" } else { "" }
                );
                println!(
                    "confidence   : {:.4} mean token probability",
                    o.mean_token_probability
                );
            }
            Err(e) => {
                eprintln!("FAILED: {e}");
                ok = false;
            }
        }
        println!();
    }

    println!("== result ==");
    println!(
        "model load paid once : {:.3}s",
        engine.load_time().as_secs_f64()
    );
    if ok {
        println!("\nPASS: local CPU inference works, model held resident across calls.");
        ExitCode::SUCCESS
    } else {
        eprintln!("\nFAIL: see above.");
        ExitCode::FAILURE
    }
}
