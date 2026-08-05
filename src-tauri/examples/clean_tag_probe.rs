//! Phase 1 Step 4b: capture -> clean/redact -> tag.
//!
//!     cargo run --release --example clean_tag_probe
//!     cargo run --release --example clean_tag_probe -- 45
//!
//! Records a real Record Mode session, turns it into a pattern description with
//! sensitive payloads redacted BEFORE the prompt exists, labels it with the
//! local model, and folds the result into confidence_calibration.
//!
//! The redaction proof is explicit: the probe searches the built prompt for the
//! literal captured payloads and reports the search result, rather than
//! asserting that redaction happened.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use llama_cpp_2::LogOptions;
use paradigm_lib::capture::{ActionKind, CaptureSession, ExclusionList};
use paradigm_lib::db;
use paradigm_lib::labeling::calibration::{self, CalibrationSample};
use paradigm_lib::labeling::{build_prompt, clean, LabelingEngine, RedactionPolicy};
use terminator::Desktop;

const MODEL_FILE: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";
const DEFAULT_SECONDS: u64 = 40;

/// A local page with a genuine password input, so the redaction path is
/// exercised against a real UI Automation element rather than a fixture.
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
<button style="font-size:1.1rem;padding:.4rem 1rem">Sign in</button>
</body></html>
"#;

/// Not a real credential. It exists so the probe can search the prompt for it.
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

    println!("== clean/tag probe ==\n");

    // ---- 1. model, loaded before capture so it costs no recording time -----
    let model_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL_FILE);
    let engine = match LabelingEngine::load(&model_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: could not load the labeling model.");
            eprintln!("  {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "[ok] model resident: {} ({:.3}s)\n",
        engine.model_source(),
        engine.load_time().as_secs_f64()
    );

    // ---- 2. put a real password field on screen ---------------------------
    let page = std::env::temp_dir().join("paradigm-probe-login.html");
    if let Err(e) = std::fs::write(&page, LOGIN_HTML) {
        eprintln!("FAIL: could not write the probe login page: {e}");
        return ExitCode::FAILURE;
    }
    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Launch a browser EXPLICITLY rather than via open_file(): .html has no
    // default app association on this machine, so open_file() raised Windows'
    // "How do you want to open this file?" dialog and the page never loaded.
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    println!("[..] opening test login page: {url}");
    let launched = ["msedge", "chrome", "firefox"].iter().any(|browser| {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, &url])
            .spawn()
            .map(|mut c| {
                let _ = c.wait();
                true
            })
            .unwrap_or(false)
    });
    if !launched {
        println!("     (could not launch a browser)");
        println!("     open this file manually: {}", page.display());
    }
    // Browser cold-start plus page load.
    tokio::time::sleep(Duration::from_secs(10)).await;

    // ---- 3. record --------------------------------------------------------
    println!("\nWHAT TO DO once recording starts ({seconds}s):");
    println!("  1. Click the Password field on the page and type anything.");
    println!("  2. Click the Username field and type anything.");
    println!("  3. Click Sign in, or click around in any other app.");
    println!("  The probe will also type a marker into the password field itself.");
    println!("  Nothing you type is real -- but do not type an actual password.\n");

    let session =
        match CaptureSession::start_session("clean-tag-probe", ExclusionList::placeholder()).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("FAIL: could not start capture: {e}");
                return ExitCode::FAILURE;
            }
        };
    println!("[ok] recording\n");

    // Drive one deterministic redactable action so the proof does not depend on
    // a human hitting the right field.
    tokio::time::sleep(Duration::from_secs(3)).await;
    drive_password_field(&desktop).await;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(5)).await;
        println!(
            "  [{:>3}s left] {} action(s) captured",
            deadline.saturating_duration_since(Instant::now()).as_secs(),
            session.admitted_so_far()
        );
    }

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- 4. the raw stream, payloads intact -------------------------------
    println!("\n== raw captured stream ({} actions) ==", report.actions.len());
    for (i, a) in report.actions.iter().enumerate() {
        println!(
            "[{:>2}] {:<8} app={:<16} role={:?} name={:?}",
            i + 1,
            a.kind.as_str(),
            a.source_app,
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-")
        );
        if let Some(p) = &a.payload {
            println!("     payload (RAW, pre-redaction): {p:?}");
        }
    }
    if report.actions.is_empty() {
        println!("(nothing captured -- cannot demonstrate clean/tag)");
        return ExitCode::from(2);
    }

    // ---- 5. clean + redact ------------------------------------------------
    let policy = RedactionPolicy::placeholder();
    let cleaned = clean(&report.actions, &policy);

    println!("\n== cleaned pattern ==");
    println!(
        "steps described : {} of {}",
        cleaned.steps_described, cleaned.steps_total
    );
    println!("redactions      : {}", cleaned.redacted_count());
    for r in &cleaned.redactions {
        println!(
            "  step {} role={:?} name={:?}",
            r.step_index,
            r.element_role.as_deref().unwrap_or("-"),
            r.element_name.as_deref().unwrap_or("-")
        );
        println!("    reason: {}", r.reason.describe());
        println!(
            "    withheld {} characters (content not retained)",
            r.withheld_len
        );
    }
    println!("\ndescription     : {}", cleaned.description);

    // ---- 6. the redaction proof -------------------------------------------
    let prompt = build_prompt(&cleaned.description);
    println!("\n== redaction proof: searching the ACTUAL prompt for captured payloads ==");
    println!("prompt length   : {} chars", prompt.len());

    // Each redaction record correlated back to the exact action it came from,
    // via step_index. clean() enumerates report.actions in order, so
    // report.actions[r.step_index] IS the action that produced this record.
    // These payloads must not appear in the prompt.
    let mut leaked: Vec<String> = Vec::new();
    if cleaned.redactions.is_empty() {
        println!("  (no payloads were redacted this run)");
    }
    for r in &cleaned.redactions {
        match report
            .actions
            .get(r.step_index)
            .and_then(|a| a.payload.as_deref())
            .filter(|p| !p.is_empty())
        {
            Some(payload) => {
                let present = prompt.contains(payload);
                println!(
                    "  step {} REDACTED payload {:?} ({} chars) -> {} in prompt",
                    r.step_index,
                    truncate(payload, 32),
                    payload.len(),
                    if present { "PRESENT (LEAK)" } else { "absent" }
                );
                if present {
                    leaked.push(payload.to_string());
                }
            }
            None => println!(
                "  step {} redacted, but its source action has no payload to check",
                r.step_index
            ),
        }
    }

    // Payloads that were NOT redacted legitimately appear, summarised. Listed
    // so "present" is never ambiguous between expected and a leak.
    for (i, a) in report.actions.iter().enumerate() {
        if a.kind != ActionKind::Type || cleaned.redactions.iter().any(|r| r.step_index == i) {
            continue;
        }
        if let Some(p) = a.payload.as_deref().filter(|p| !p.is_empty()) {
            println!(
                "  step {i} not redacted, payload {:?} -> {} in prompt (expected)",
                truncate(p, 32),
                if prompt.contains(p) { "present" } else { "absent" }
            );
        }
    }

    // UNCONDITIONAL. The probe types this into the password field every run, so
    // it must never reach the prompt by any route -- redacted, summarised, or
    // truncated. Gated independently of `leaked`.
    let secret_in_prompt = prompt.contains(FAKE_SECRET);
    println!(
        "\n  UNCONDITIONAL: probe marker {FAKE_SECRET:?} -> {}",
        if secret_in_prompt { "PRESENT (FAIL)" } else { "absent" }
    );

    println!("\n---- prompt actually sent to the model ----");
    println!("{prompt}");
    println!("---- end of prompt ----");

    // ---- 7. tag -----------------------------------------------------------
    println!("\n== tag ==");
    let outcome = match engine.label(&cleaned.description) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("FAIL: labeling failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("raw output      : {:?}", outcome.raw_output);
    println!(
        "label           : {:?}{}",
        outcome.label,
        if outcome.repaired {
            "  (AFTER REPAIR)"
        } else {
            ""
        }
    );
    println!(
        "inference       : {:.3}s ({} tokens)",
        outcome.inference_time.as_secs_f64(),
        outcome.tokens_generated
    );
    println!(
        "confidence      : {:.4} mean token probability",
        outcome.mean_token_probability
    );

    // ---- 8. calibration accumulation --------------------------------------
    println!("\n== calibration ==");
    let sample = CalibrationSample::from_outcome(&outcome);
    let (bin_min, bin_max) = sample.bin();
    println!(
        "sample          : model={:?} raw_score={:.4} success={} -> bin [{:.2}, {:.2})",
        sample.model_source, sample.raw_score, sample.success, bin_min, bin_max
    );

    // A scratch database, so the probe never writes into the user's real store.
    let tmp = std::env::temp_dir().join("paradigm-probe-calibration");
    let _ = std::fs::remove_dir_all(&tmp);
    let (db_path, key_path) = db::paths_in(&tmp);
    match db::open(&db_path, &key_path) {
        Ok(mut conn) => {
            if let Err(e) = calibration::record(&mut conn, &sample) {
                println!("could not record: {e}");
            } else {
                match calibration::bins_for(&conn, &sample.model_source) {
                    Ok(bins) => {
                        println!("confidence_calibration rows for this model:");
                        for (min, max, n, ok, norm) in bins {
                            println!(
                                "  [{min:.2}, {max:.2})  sample_count={n}  success_count={ok}  \
                                 normalized_score={}",
                                norm.map_or("NULL (Phase 2 writes this)".to_string(), |v| format!(
                                    "{v:.3}"
                                ))
                            );
                        }
                    }
                    Err(e) => println!("could not read back: {e}"),
                }
            }
        }
        Err(e) => println!("scratch database unavailable: {e}"),
    }

    // ---- 9. verdict -------------------------------------------------------
    println!("\n== result ==");
    println!("redactions applied   : {}", cleaned.redacted_count());
    println!("payload leaks found  : {}", leaked.len());

    if secret_in_prompt {
        eprintln!("\nFAIL: the probe marker reached the prompt. Redaction did not hold,");
        eprintln!("regardless of what the per-record checks reported.");
        return ExitCode::FAILURE;
    }
    if !leaked.is_empty() {
        eprintln!("\nFAIL: redacted payload(s) survived into the prompt: {leaked:?}");
        return ExitCode::FAILURE;
    }
    if cleaned.redacted_count() == 0 {
        println!("\nINCOMPLETE: the pipeline ran and leaked nothing, but no action matched");
        println!("the redaction policy, so the redaction path was not exercised live.");
        println!("(It is covered by unit tests in src/labeling/clean.rs.)");
        return ExitCode::from(2);
    }

    println!("\nPASS: captured a real session, redacted before prompt construction,");
    println!("and labelled it: {:?}", outcome.label);
    ExitCode::SUCCESS
}

/// Type a marker into the page's password field, so there is always at least
/// one action the redaction policy must catch.
async fn drive_password_field(desktop: &Desktop) {
    // Desktop::locator returns a Locator directly; it is UIElement::locator
    // that returns a Result.
    let locator = desktop.locator("role:Edit|name:Password");
    match locator.first(Some(Duration::from_secs(20))).await {
        Ok(field) => {
            let _ = field.focus();
            match field.type_text(FAKE_SECRET, false) {
                Ok(()) => println!("  [driven] typed probe marker into the Password field"),
                Err(e) => println!("  (could not type into the password field: {e})"),
            }
            // Move focus away so the text-input-completed event fires.
            tokio::time::sleep(Duration::from_millis(800)).await;
            let username = desktop.locator("role:Edit|name:Username");
            if let Ok(user) = username.first(Some(Duration::from_secs(3))).await {
                let _ = user.click();
                let _ = user.type_text("probe-user", false);
            }
        }
        Err(e) => {
            println!("  (password field not found: {e})");
            println!("  click it and type manually to exercise redaction");
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "..."
}
