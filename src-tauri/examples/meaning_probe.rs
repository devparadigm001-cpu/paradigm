//! Can the local model tell a MEANINGFUL click from an INCIDENTAL one?
//!
//!     cargo run --example meaning_probe
//!
//! Uses the SAME few-shot completion format `labeling::build_prompt` uses. A
//! first attempt with free-form prose made the model echo a value from the
//! prompt for every question -- a property of driving a completion-style 0.5B
//! model with prose, not an answer to anything.
//!
//! Two questions, in order of how much they decide:
//!
//!  1. On DECIDABLE cases -- exactly one field is a UI affordance, the rest are
//!     data -- can the model do the task at all? Four of them, so a single miss
//!     is not mistaken for a pattern.
//!  2. On UNDECIDABLE cases -- every field is legitimate data, and which one
//!     matters depends on the user's private intent -- what does it answer, and
//!     does its CONFIDENCE drop? Confidence dropping is the entire premise of
//!     "local first, escalate on low confidence". If it does not drop, the
//!     router cannot tell which answers to escalate.
//!
//! U1 appears three times in three field orders. If the answer follows position
//! rather than meaning, the model is matching the prompt's shape, not reasoning.
//!
//! The few-shot examples put the odd-one-out in first, second and third
//! position, so the model is not taught "the answer is the last one".

use std::path::PathBuf;
use std::process::ExitCode;

use llama_cpp_2::LogOptions;
use paradigm_lib::labeling::LabelingEngine;

const MODEL_FILE: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";

/// Same shape as `labeling::build_prompt`: worked examples, then the real one.
fn prompt(fields: &str) -> String {
    format!(
        "Input: \"ACTION=Print this page; PRODUCT=Ceramic Mug Set; QUANTITY=12\"\n\
         Output: {{\"incidental\": \"ACTION\"}}\n\
         Input: \"INVOICE=INV-4471; HELP=Contact support; TOTAL=92.10\"\n\
         Output: {{\"incidental\": \"HELP\"}}\n\
         Input: \"NAME=Ada Lovelace; EMAIL=ada@example.com; NAV=Back to list\"\n\
         Output: {{\"incidental\": \"NAV\"}}\n\
         Input: \"{fields}\"\n\
         Output:"
    )
}

struct Case {
    id: &'static str,
    decidable: bool,
    /// The correct answer for a decidable case; the reason there isn't one
    /// otherwise. Never shown to the model.
    expected: &'static str,
    fields: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        // ---- DECIDABLE: exactly one field is a UI affordance, not data ----
        Case {
            id: "D1-button",
            decidable: true,
            expected: "BUTTON",
            fields: "SKU=MG-100; BUTTON=Add to cart; PRICE=14.99",
        },
        Case {
            id: "D2-link",
            decidable: true,
            expected: "LINK",
            fields: "ORDER=RS-1001; STATUS=Pending; LINK=View details",
        },
        Case {
            id: "D3-export",
            decidable: true,
            expected: "EXPORT",
            fields: "DATE=2026-08-19; EXPORT=Download CSV; AMOUNT=92.10",
        },
        Case {
            id: "D4-menu",
            decidable: true,
            expected: "MENU",
            fields: "EMAIL=ada@example.com; PHONE=555-0142; MENU=Open settings",
        },
        // ---- UNDECIDABLE: every field is legitimate data ----
        Case {
            id: "U1-order-a",
            decidable: false,
            expected: "none -- an order sheet makes CUSTOMER incidental, a customer list makes PRODUCT incidental",
            fields: "PRODUCT=Ceramic Mug Set; QUANTITY=12; CUSTOMER=Harbor Point Traders",
        },
        Case {
            id: "U1-order-b",
            decidable: false,
            expected: "same fields, reordered",
            fields: "CUSTOMER=Harbor Point Traders; PRODUCT=Ceramic Mug Set; QUANTITY=12",
        },
        Case {
            id: "U1-order-c",
            decidable: false,
            expected: "same fields, reordered again",
            fields: "QUANTITY=12; CUSTOMER=Harbor Point Traders; PRODUCT=Ceramic Mug Set",
        },
        Case {
            id: "U2-clinical",
            decidable: false,
            expected: "none -- all three are real clinical data",
            fields: "PATIENT=Jane Doe; MEDICATION=Amoxicillin; DOSE=500mg",
        },
        Case {
            id: "U3-invoice",
            decidable: false,
            expected: "none -- all three are real invoice data",
            fields: "VENDOR=Ashgrove Ltd; INVOICE=INV-4471; TOTAL=92.10",
        },
    ]
}

fn answered_field(raw: &str) -> String {
    // Output looks like {"incidental": "PRICE"} -- pull the last quoted token.
    raw.rsplit('"')
        .nth(1)
        .unwrap_or(raw)
        .trim()
        .to_ascii_uppercase()
}

fn main() -> ExitCode {
    if !std::env::args().any(|a| a == "--verbose") {
        llama_cpp_2::send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));
    }

    let model_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL_FILE);
    let engine = match LabelingEngine::load(&model_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("model load failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("model: {}\n", engine.model_source());

    let mut decidable: Vec<f64> = Vec::new();
    let mut undecidable: Vec<f64> = Vec::new();
    let (mut correct, mut attempted) = (0usize, 0usize);
    let mut rows: Vec<(String, String, String, f64)> = Vec::new();

    for case in cases() {
        match engine.complete(&prompt(case.fields)) {
            Ok(c) => {
                let raw = c.output.trim().replace('\n', " ");
                let got = answered_field(&raw);
                let verdict = if case.decidable {
                    attempted += 1;
                    if got == case.expected.to_ascii_uppercase() {
                        correct += 1;
                        "correct".to_string()
                    } else {
                        format!("WRONG (expected {})", case.expected)
                    }
                } else {
                    "n/a -- no correct answer exists".to_string()
                };
                if case.decidable {
                    decidable.push(c.mean_token_probability);
                } else {
                    undecidable.push(c.mean_token_probability);
                }
                rows.push((
                    case.id.to_string(),
                    got,
                    verdict,
                    c.mean_token_probability,
                ));
            }
            Err(e) => println!("{}: ERROR {e}", case.id),
        }
    }

    println!("{:<14} {:<12} {:>11}  {}", "case", "answered", "confidence", "verdict");
    for (id, got, verdict, conf) in &rows {
        println!("{id:<14} {got:<12} {conf:>11.4}  {verdict}");
    }

    let mean = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };

    println!("\n== 1. can it do the DECIDABLE task at all? ==");
    println!("   {correct}/{attempted} correct");

    println!("\n== 2. the number the escalation architecture depends on ==");
    println!("   mean confidence, DECIDABLE   ({} cases): {:.4}", decidable.len(), mean(&decidable));
    println!("   mean confidence, UNDECIDABLE ({} cases): {:.4}", undecidable.len(), mean(&undecidable));
    println!("   difference: {:+.4}", mean(&undecidable) - mean(&decidable));
    println!();
    println!("   'Escalate on low confidence' requires the UNDECIDABLE cases to");
    println!("   score measurably LOWER. Anything else and the router cannot");
    println!("   separate them, whatever model sits on the far side of it.");

    ExitCode::SUCCESS
}
