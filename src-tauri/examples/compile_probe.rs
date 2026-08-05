//! Phase 1 Step 5: capture -> label -> compile -> validate -> store -> read back.
//!
//!     cargo run --release --example compile_probe
//!     cargo run --release --example compile_probe -- 40
//!
//! Records a real session, labels it with the local model, compiles it into
//! playbook rows, validates, writes it to a scratch encrypted database, then
//! reads it back and prints every stored field.
//!
//! Three things are proven from the READ-BACK rows, not from in-memory state:
//!   * `reversible` was assigned, and why for each step.
//!   * `control_role` mapped from the raw Terminator role.
//!   * A redacted payload is still redacted in what was STORED.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use llama_cpp_2::LogOptions;
use paradigm_lib::capture::{CaptureSession, ExclusionList};
use paradigm_lib::compile::{compile, store, validate, ReversibilityPolicy};
use paradigm_lib::db;
use paradigm_lib::labeling::{clean, LabelingEngine, RedactionPolicy};
use terminator::Desktop;

const MODEL_FILE: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";
const DEFAULT_SECONDS: u64 = 40;

const LOGIN_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Paradigm Probe Login</title></head>
<body style="font-family:sans-serif;padding:2rem">
<h2>Paradigm Probe &mdash; test login (not a real service)</h2>
<label for="u">Username</label><br>
<input id="u" name="Username" aria-label="Username" placeholder="Username"
       style="font-size:1.2rem;padding:.4rem"><br><br>
<label for="p">Password</label><br>
<input id="p" name="Password" aria-label="Password" placeholder="Password"
       type="password" style="font-size:1.2rem;padding:.4rem"><br><br>
<button aria-label="Submit Payment" style="font-size:1.1rem;padding:.4rem 1rem">Submit Payment</button>
&nbsp;
<button aria-label="Cancel" style="font-size:1.1rem;padding:.4rem 1rem">Cancel</button>
</body></html>
"#;

const FAKE_SECRET: &str = "hunter2-probe-not-real";

#[cfg(windows)]
fn make_dpi_aware() {
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    #[cfg(windows)]
    make_dpi_aware();
    llama_cpp_2::send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));

    let seconds = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECONDS);

    println!("== compile probe: capture -> label -> compile -> validate -> store ==\n");

    // ---- model ------------------------------------------------------------
    let model_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL_FILE);
    let engine = match LabelingEngine::load(&model_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: model load: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("[ok] model resident ({:.3}s)", engine.load_time().as_secs_f64());

    // ---- target page ------------------------------------------------------
    let page = std::env::temp_dir().join("paradigm-compile-probe.html");
    if let Err(e) = std::fs::write(&page, LOGIN_HTML) {
        eprintln!("FAIL: writing probe page: {e}");
        return ExitCode::FAILURE;
    }
    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: accessibility engine: {e}");
            return ExitCode::FAILURE;
        }
    };
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    println!("[..] opening {url}");
    for browser in ["msedge", "chrome", "firefox"] {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, &url])
            .spawn()
        {
            let _ = c.wait();
            break;
        }
    }
    tokio::time::sleep(Duration::from_secs(10)).await;

    // ---- record -----------------------------------------------------------
    println!("\nWHAT TO DO ({seconds}s): click the Password field and type; click");
    println!("Submit Payment and Cancel so both an irreversible and a reversible");
    println!("step get recorded. The probe also drives the password field itself.\n");

    let session = match CaptureSession::start_session("compile-probe", ExclusionList::placeholder())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: capture start: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("[ok] recording\n");

    tokio::time::sleep(Duration::from_secs(3)).await;
    drive(&desktop).await;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(5)).await;
        println!(
            "  [{:>3}s left] {} captured",
            deadline.saturating_duration_since(Instant::now()).as_secs(),
            session.admitted_so_far()
        );
    }

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: capture stop: {e}");
            return ExitCode::FAILURE;
        }
    };
    if report.actions.is_empty() {
        println!("\n(nothing captured -- cannot compile)");
        return ExitCode::from(2);
    }

    // ---- label ------------------------------------------------------------
    let redaction = RedactionPolicy::placeholder();
    let cleaned = clean(&report.actions, &redaction);
    println!("\n== label ==");
    println!("description : {}", cleaned.description);
    let label = match engine.label(&cleaned.description) {
        Ok(o) => {
            println!("label       : {:?}", o.label);
            o.label
        }
        Err(e) => {
            eprintln!("FAIL: labeling: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- compile ----------------------------------------------------------
    let reversibility = ReversibilityPolicy::placeholder();
    let playbook = compile(&report.actions, &label, &reversibility, &redaction);

    println!("\n== compiled ==");
    println!("playbook id : {}", playbook.id);
    println!("name        : {:?}", playbook.name);
    println!("source      : {:?}", playbook.source);
    println!("steps       : {}", playbook.steps.len());
    println!(
        "irreversible: {}   redacted payloads: {}",
        playbook.irreversible_count(),
        playbook.redacted_count()
    );

    println!("\n-- reversibility decisions (showing the working) --");
    for s in &playbook.steps {
        println!(
            "  step {:>2}  {:<8} {:<8} {:<12} target={:?}",
            s.step_order,
            s.action_type,
            if s.reversible { "REVERS." } else { "IRREVERS." },
            s.control_role.as_str(),
            s.target_name.as_deref().unwrap_or("-")
        );
        println!("            because: {}", s.reversibility_reason.describe());
    }

    println!("\n-- control_role mapping (raw Terminator role -> locked enum) --");
    for s in &playbook.steps {
        println!(
            "  step {:>2}  {:?} -> {:?}",
            s.step_order,
            s.raw_role.as_deref().unwrap_or("<none>"),
            s.control_role.as_str()
        );
    }

    // ---- validate ---------------------------------------------------------
    println!("\n== validate ==");
    let errors = validate(&playbook);
    if errors.is_empty() {
        println!("[ok] no validation errors");
    } else {
        for e in &errors {
            println!("  ERROR: {}", e.describe());
        }
        eprintln!("\nFAIL: validation rejected the playbook; nothing was stored.");
        return ExitCode::FAILURE;
    }

    // ---- store ------------------------------------------------------------
    println!("\n== store ==");
    let tmp = std::env::temp_dir().join("paradigm-compile-probe-db");
    let _ = std::fs::remove_dir_all(&tmp);
    let (db_path, key_path) = db::paths_in(&tmp);
    let mut conn = match db::open(&db_path, &key_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: scratch database: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("scratch db  : {}", db_path.display());
    if let Err(e) = store::store(&mut conn, &playbook) {
        eprintln!("FAIL: {e}");
        return ExitCode::FAILURE;
    }
    println!("[ok] stored in one transaction");

    // ---- read back --------------------------------------------------------
    println!("\n== read back FROM THE DATABASE ==");
    let stored = match store::load(&conn, &playbook.id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: read back: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("id          : {}", stored.id);
    println!("name        : {:?}", stored.name);
    println!("source      : {:?}", stored.source);
    println!("created_at  : {}", stored.created_at);
    println!("updated_at  : {}", stored.updated_at);
    println!("steps       : {}\n", stored.steps.len());

    for s in &stored.steps {
        println!("  [step_order {}]", s.step_order);
        println!("    id                  : {}", s.id);
        println!("    action_type         : {:?}", s.action_type);
        println!("    control_role        : {:?}", s.control_role);
        println!(
            "    reversible          : {}",
            match s.reversible {
                Some(true) => "1 (reversible)".to_string(),
                Some(false) => "0 (IRREVERSIBLE)".to_string(),
                None => "NULL".to_string(),
            }
        );
        println!("    action_payload_json : {}", s.action_payload_json);
    }

    // ---- proofs against the stored rows -----------------------------------
    println!("\n== proofs (against read-back rows, not in-memory state) ==");

    let all_stored_json: String = stored
        .steps
        .iter()
        .map(|s| s.action_payload_json.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    // 1. Redacted payloads must not appear in what was stored.
    let mut leaks = Vec::new();
    let redacted_orders: Vec<i64> = playbook
        .steps
        .iter()
        .filter(|s| s.payload_redacted)
        .map(|s| s.step_order)
        .collect();

    if redacted_orders.is_empty() {
        println!("  redaction: no payload was redacted this run");
    }
    for order in &redacted_orders {
        // step_order is 1-based and dense, so actions[order-1] is the source.
        let raw = report
            .actions
            .get((*order - 1) as usize)
            .and_then(|a| a.payload.as_deref())
            .filter(|p| !p.is_empty());
        let stored_json = stored
            .steps
            .iter()
            .find(|s| s.step_order == *order)
            .map(|s| s.action_payload_json.as_str())
            .unwrap_or("");

        match raw {
            Some(payload) => {
                let present = stored_json.contains(payload);
                println!(
                    "  redaction: step {order} raw payload ({} chars) -> {} in STORED json",
                    payload.len(),
                    if present { "PRESENT (LEAK)" } else { "absent" }
                );
                if present {
                    leaks.push(payload.to_string());
                }
            }
            None => println!("  redaction: step {order} had no payload to check"),
        }
    }

    let secret_stored = all_stored_json.contains(FAKE_SECRET);
    println!(
        "  UNCONDITIONAL: probe marker {FAKE_SECRET:?} -> {} in stored rows",
        if secret_stored { "PRESENT (FAIL)" } else { "absent" }
    );

    // 2. reversible round-tripped as a real value, never NULL.
    let null_reversible = stored.steps.iter().filter(|s| s.reversible.is_none()).count();
    println!("  reversible: {} step(s) stored as NULL (want 0)", null_reversible);

    // 3. control_role values are all in the locked enum.
    let bad_roles: Vec<&str> = stored
        .steps
        .iter()
        .map(|s| s.control_role.as_str())
        .filter(|r| {
            !matches!(
                *r,
                "button" | "textbox" | "dropdown" | "checkbox" | "radio" | "link" | "other"
            )
        })
        .collect();
    println!("  control_role: {} value(s) outside the locked enum", bad_roles.len());

    // 4. Compiled and stored classifications agree.
    let mismatches = playbook
        .steps
        .iter()
        .filter(|c| {
            stored
                .steps
                .iter()
                .find(|s| s.step_order == c.step_order)
                .map(|s| {
                    s.reversible != Some(c.reversible)
                        || store::parse_control_role(&s.control_role) != c.control_role
                })
                .unwrap_or(true)
        })
        .count();
    println!("  round-trip: {mismatches} step(s) differ between compiled and stored");

    // ---- verdict ----------------------------------------------------------
    println!("\n== result ==");
    if secret_stored {
        eprintln!("FAIL: the probe marker was written to the database.");
        return ExitCode::FAILURE;
    }
    if !leaks.is_empty() {
        eprintln!("FAIL: redacted payload(s) present in stored rows: {leaks:?}");
        return ExitCode::FAILURE;
    }
    if null_reversible > 0 || !bad_roles.is_empty() || mismatches > 0 {
        eprintln!("FAIL: stored rows did not match what was compiled.");
        return ExitCode::FAILURE;
    }

    println!(
        "PASS: {} step(s) compiled, validated, stored and read back intact.",
        stored.steps.len()
    );
    println!("      playbook {:?} (source={:?})", stored.name, stored.source);
    if redacted_orders.is_empty() {
        println!("      NOTE: no redaction occurred, so that proof was not exercised live.");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// Drive a deterministic irreversible action and a sensitive field, so the
/// proofs do not depend on a human hitting the right controls.
async fn drive(desktop: &Desktop) {
    let pw = desktop.locator("role:Edit|name:Password");
    match pw.first(Some(Duration::from_secs(20))).await {
        Ok(field) => {
            let _ = field.focus();
            match field.type_text(FAKE_SECRET, false) {
                Ok(()) => println!("  [driven] typed marker into Password"),
                Err(e) => println!("  (type into Password failed: {e})"),
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        Err(e) => println!("  (Password field not found: {e})"),
    }

    let user = desktop.locator("role:Edit|name:Username");
    if let Ok(f) = user.first(Some(Duration::from_secs(5))).await {
        let _ = f.click();
        let _ = f.type_text("probe-user", false);
        tokio::time::sleep(Duration::from_millis(600)).await;
    }

    // An irreversible-by-keyword target and a reversible one, for contrast.
    let pay = desktop.locator("role:Button|name:Submit Payment");
    if let Ok(b) = pay.first(Some(Duration::from_secs(5))).await {
        let _ = b.click();
        println!("  [driven] clicked Submit Payment");
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    let cancel = desktop.locator("role:Button|name:Cancel");
    if let Ok(b) = cancel.first(Some(Duration::from_secs(5))).await {
        let _ = b.click();
        println!("  [driven] clicked Cancel");
    }
}
