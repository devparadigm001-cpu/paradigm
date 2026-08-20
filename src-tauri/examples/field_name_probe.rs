//! Can the local model name a field that `identity::tree` could not label?
//!
//!     cargo run --example field_name_probe
//!
//! The gap this targets is real and documented: a marked value on a page with
//! no structural element before it comes back as `el/<ordinal>/` with an EMPTY
//! field name, so any pattern built from it reads `"" -> A`. See
//! `docs/known-issues/an-element-identity-mark-records-no-field-label.md`.
//!
//! This is NOT the intent question that was just disproven. Whether a click was
//! meaningful depends on what the user privately wanted; a field NAME is
//! plausibly a property of the text and its surroundings. Different problem,
//! and it deserves its own test rather than the previous conclusion.
//!
//! Four things are measured, in descending order of how much they matter:
//!
//!  1. STABILITY. `detect` groups records by `FieldMapping::source_field`. If
//!     three records' values for one field get three different names, the
//!     signature differs per record and detection returns InconsistentMapping.
//!     A consistently WRONG name is more useful here than an inconsistently
//!     right one. This is the criterion the approach lives or dies on.
//!  2. ACCURACY where content settles it -- an email address, a phone number.
//!  3. BEHAVIOUR where it genuinely does not -- "12" could be a quantity, an
//!     age, a line number, a count. Several names are defensible and the page's
//!     own schema is exactly what is missing.
//!  4. CONTEXT SENSITIVITY. The same value under two different page contexts.
//!     If context steers the answer, feeding more of the tree is a real fix. If
//!     it does not, the surrounding text is not being used.
//!
//! Same few-shot completion format as `labeling::build_prompt`, because prose
//! prompting makes this model echo its input.

use std::path::PathBuf;
use std::process::ExitCode;

use llama_cpp_2::LogOptions;
use paradigm_lib::labeling::LabelingEngine;

const MODEL_FILE: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";

fn prompt(value: &str, nearby: &str) -> String {
    format!(
        "Input: value=\"ada@example.com\" nearby=\"Contact; Send message; Profile\"\n\
         Output: {{\"field\": \"EMAIL\"}}\n\
         Input: value=\"2026-03-04\" nearby=\"Invoice; Due; Paid\"\n\
         Output: {{\"field\": \"DATE\"}}\n\
         Input: value=\"92.10\" nearby=\"Subtotal; Tax; Total\"\n\
         Output: {{\"field\": \"TOTAL\"}}\n\
         Input: value=\"{value}\" nearby=\"{nearby}\"\n\
         Output:"
    )
}

struct Case {
    group: &'static str,
    note: &'static str,
    value: &'static str,
    nearby: &'static str,
}

fn cases() -> Vec<Case> {
    let order_ctx = "Order RS-1001; Pending; Ceramic Mug Set; 12";
    vec![
        // 1. STABILITY -- one field, three records, identical context shape.
        Case { group: "STABILITY", note: "customer, record 1", value: "Harbor Point Traders", nearby: order_ctx },
        Case { group: "STABILITY", note: "customer, record 2", value: "Ashgrove Manufacturing", nearby: "Order RS-1002; Pending; Steel Bracket; 40" },
        Case { group: "STABILITY", note: "customer, record 3", value: "Windmere Consulting", nearby: "Order RS-1003; Pending; Ergonomic Chair; 2" },

        // 2. ACCURACY -- content settles it.
        Case { group: "DECIDABLE", note: "expect PHONE", value: "555-0142", nearby: "Contact; Email; Address" },
        Case { group: "DECIDABLE", note: "expect INVOICE-ish", value: "INV-4471", nearby: "Billing; Date; Amount" },

        // 3. GENUINELY AMBIGUOUS -- several names are defensible.
        Case { group: "AMBIGUOUS", note: "quantity? age? count? line no?", value: "12", nearby: order_ctx },
        Case { group: "AMBIGUOUS", note: "which date? ordered/shipped/delivered", value: "2026-08-19", nearby: "Order RS-1001; Shipped; Delivered" },
        Case { group: "AMBIGUOUS", note: "no context at all", value: "Harbor Point Traders", nearby: "" },

        // 5. ECHO TEST -- same value, same nearby tokens, DIFFERENT ORDER.
        // If the answer tracks position, the model is copying a neighbouring
        // token rather than classifying the value.
        Case { group: "ECHO", note: "phone, nearby order A", value: "555-0142", nearby: "Contact; Email; Address" },
        Case { group: "ECHO", note: "phone, nearby order B", value: "555-0142", nearby: "Address; Contact; Email" },
        Case { group: "ECHO", note: "phone, nearby order C", value: "555-0142", nearby: "Email; Address; Contact" },
        Case { group: "ECHO2", note: "customer, nearby order A", value: "Harbor Point Traders", nearby: "Order RS-1001; Pending; Ceramic Mug Set; 12" },
        Case { group: "ECHO2", note: "customer, nearby order B", value: "Harbor Point Traders", nearby: "Pending; Ceramic Mug Set; 12; Order RS-1001" },
        Case { group: "ECHO2", note: "customer, nearby order C", value: "Harbor Point Traders", nearby: "Ceramic Mug Set; 12; Order RS-1001; Pending" },
        // 4. CONTEXT SENSITIVITY -- same value, two page contexts.
        Case { group: "CONTEXT", note: "company in an ORDER page", value: "Harbor Point Traders", nearby: order_ctx },
        Case { group: "CONTEXT", note: "same company in a SUPPLIER directory", value: "Harbor Point Traders", nearby: "Supplier directory; Active; Payment terms; Net 30" },
    ]
}

fn field_of(raw: &str) -> String {
    raw.rsplit('"').nth(1).unwrap_or(raw).trim().to_ascii_uppercase()
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

    println!("{:<11} {:<24} {:<16} {:>6}  {}", "group", "value", "-> field", "conf", "note");
    let mut stability: Vec<String> = Vec::new();
    let mut context: Vec<String> = Vec::new();

    for case in cases() {
        match engine.complete(&prompt(case.value, case.nearby)) {
            Ok(c) => {
                let field = field_of(c.output.trim());
                println!(
                    "{:<11} {:<24} {:<16} {:>6.3}  {}",
                    case.group,
                    case.value.chars().take(23).collect::<String>(),
                    field,
                    c.mean_token_probability,
                    case.note
                );
                if case.group == "STABILITY" {
                    stability.push(field.clone());
                }
                if case.group == "CONTEXT" {
                    context.push(field);
                }
            }
            Err(e) => println!("{:<11} ERROR {e}", case.group),
        }
    }

    println!("\n== 1. STABILITY: does one field get one name across records? ==");
    println!("   names produced: {stability:?}");
    let all_same = stability.windows(2).all(|w| w[0] == w[1]);
    if all_same {
        println!("   SAME across all three -- detect would group these as one field.");
    } else {
        println!("   DIFFERENT -- detect would see {} distinct source fields for what is",
            stability.iter().collect::<std::collections::BTreeSet<_>>().len());
        println!("   one column, and return InconsistentMapping instead of a Pattern.");
    }

    println!("\n== 2. CONTEXT: does the same value get a different name in a different page? ==");
    println!("   names produced: {context:?}");
    if context.windows(2).all(|w| w[0] == w[1]) {
        println!("   UNCHANGED -- the surrounding text is not steering the answer.");
    } else {
        println!("   CHANGED -- context is being used, so feeding more tree may help.");
    }

    ExitCode::SUCCESS
}
