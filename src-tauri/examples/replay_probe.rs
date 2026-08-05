//! Phase 1 Step 6: record a real playbook, then replay it for real.
//!
//!     cargo run --release --example replay_probe
//!     cargo run --release --example replay_probe -- 20
//!
//! Records a short controlled session against the probe login page, compiles
//! and stores it, then replays it through Terminator and prints the resulting
//! `runs` and `run_steps_log` rows read back from the database.
//!
//! WARNING: replay performs REAL clicks and typing on your desktop. Keep the
//! recording window free of your own activity, or a stray captured step will be
//! replayed back at you.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use llama_cpp_2::LogOptions;
use paradigm_lib::capture::{CaptureSession, ExclusionList};
use paradigm_lib::compile::{compile, store, validate, ReversibilityPolicy};
use paradigm_lib::db;
use paradigm_lib::labeling::{clean, LabelingEngine, RedactionPolicy};
use paradigm_lib::replay::{self, journal, StepResult};
use terminator::Desktop;

const MODEL_FILE: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";
const DEFAULT_SECONDS: u64 = 22;
const FAKE_SECRET: &str = "hunter2-probe-not-real";

const LOGIN_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Paradigm Replay Probe</title></head>
<body style="font-family:sans-serif;padding:2rem">
<h2>Paradigm Replay Probe &mdash; test login (not a real service)</h2>
<label for="u">Username</label><br>
<input id="u" name="Username" aria-label="Username" placeholder="Username"
       style="font-size:1.2rem;padding:.4rem"><br><br>
<label for="p">Password</label><br>
<input id="p" name="Password" aria-label="Password" placeholder="Password"
       type="password" style="font-size:1.2rem;padding:.4rem"><br><br>
<button aria-label="Cancel" style="font-size:1.1rem;padding:.4rem 1rem">Cancel</button>
</body></html>
"#;

#[tokio::main]
async fn main() -> ExitCode {
    replay::ensure_dpi_aware();
    llama_cpp_2::send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));

    let seconds = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECONDS);

    println!("== replay probe: record -> store -> REPLAY -> read back ==\n");

    let model_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL_FILE);
    let engine = match LabelingEngine::load(&model_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("FAIL: model load: {e}");
            return ExitCode::FAILURE;
        }
    };
    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: accessibility engine: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- target page ------------------------------------------------------
    let page = std::env::temp_dir().join("paradigm-replay-probe.html");
    if let Err(e) = std::fs::write(&page, LOGIN_HTML) {
        eprintln!("FAIL: writing probe page: {e}");
        return ExitCode::FAILURE;
    }
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
    println!("\nRECORDING for {seconds}s -- please do NOT touch the machine.");
    println!("The probe drives the form itself: username, then password.\n");

    let session = match CaptureSession::start_session("replay-probe", ExclusionList::placeholder())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: capture start: {e}");
            return ExitCode::FAILURE;
        }
    };

    tokio::time::sleep(Duration::from_secs(2)).await;
    drive_form(&desktop).await;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(4)).await;
        println!("  [{:>3}s left] {} captured",
            deadline.saturating_duration_since(Instant::now()).as_secs(),
            session.admitted_so_far());
    }

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: capture stop: {e}");
            return ExitCode::FAILURE;
        }
    };
    if report.actions.is_empty() {
        println!("\n(nothing captured -- cannot build a playbook)");
        return ExitCode::from(2);
    }

    // ---- label, compile, store -------------------------------------------
    let redaction = RedactionPolicy::placeholder();
    let cleaned = clean(&report.actions, &redaction);
    let label = match engine.label(&cleaned.description) {
        Ok(o) => o.label,
        Err(e) => {
            eprintln!("FAIL: labeling: {e}");
            return ExitCode::FAILURE;
        }
    };

    let playbook = compile(
        &report.actions,
        &label,
        &ReversibilityPolicy::placeholder(),
        &redaction,
    );
    let errors = validate(&playbook);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("  validation: {}", e.describe());
        }
        eprintln!("FAIL: playbook rejected, nothing stored.");
        return ExitCode::FAILURE;
    }

    println!("\n== playbook to be replayed ==");
    println!("name  : {:?}", playbook.name);
    println!("steps : {}", playbook.steps.len());
    for s in &playbook.steps {
        println!(
            "  {:>2}. {:<8} {:<9} redacted={:<5} target={:?}",
            s.step_order,
            s.action_type,
            if s.reversible { "revers." } else { "IRREVERS." },
            s.payload_redacted,
            s.target_name.as_deref().unwrap_or("-")
        );
    }

    let tmp = std::env::temp_dir().join("paradigm-replay-probe-db");
    let _ = std::fs::remove_dir_all(&tmp);
    let (db_path, key_path) = db::paths_in(&tmp);
    let mut conn = match db::open(&db_path, &key_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: scratch database: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = store::store(&mut conn, &playbook) {
        eprintln!("FAIL: store: {e}");
        return ExitCode::FAILURE;
    }
    println!("\n[ok] stored playbook {}", playbook.id);

    // ---- REPLAY -----------------------------------------------------------
    println!("\n== replay (real clicks and typing) ==");
    let result = match replay::replay(&mut conn, &desktop, &playbook.id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: replay could not run: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("run id : {}", result.run_id);
    println!("status : {}", result.status);
    println!("steps  : {} attempted of {}\n", result.steps_attempted(), result.steps_total);
    for o in &result.outcomes {
        println!("  step {:>2} [{}] {}", o.step_order, o.action_type, o.result.label());
        println!("           selector: {:?}", o.selector.as_deref().unwrap_or("<none>"));
        println!("           {}", o.detail);
    }

    // ---- read the journal back FROM THE DATABASE --------------------------
    println!("\n== runs row (read back) ==");
    let run = match journal::load_run(&conn, &result.run_id) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: reading runs row: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("  id           : {}", run.id);
    println!("  playbook_id  : {:?}", run.playbook_id.as_deref().unwrap_or("<null>"));
    println!("  feature      : {:?}", run.feature);
    println!("  status       : {:?}", run.status);
    println!("  billable     : {}", run.billable);
    println!("  started_at   : {:?}", run.started_at.as_deref().unwrap_or("<null>"));
    println!("  completed_at : {:?}", run.completed_at.as_deref().unwrap_or("<null>"));

    println!("\n== run_steps_log rows (read back) ==");
    let logs = match journal::load_step_logs(&conn, &result.run_id) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FAIL: reading run_steps_log: {e}");
            return ExitCode::FAILURE;
        }
    };
    for l in &logs {
        println!("  [step_order {}] event_type={:?} action_type={:?}", l.step_order, l.event_type, l.action_type);
        println!("      is_sensitive : {}", l.is_sensitive);
        println!("      data_payload : {:?}", l.data_payload.as_deref().unwrap_or("<NULL>"));
        println!("      model_source : {:?}", l.model_source.as_deref().unwrap_or("<NULL>"));
        println!("      cost         : {}", l.cost);
        println!("      system_state : {}", l.system_state_json);
        println!("      target_ctx   : {}", l.target_ui_context_json);
        println!("      timestamp    : {}", l.timestamp);
    }

    // ---- proofs -----------------------------------------------------------
    println!("\n== proofs ==");

    let halted = result
        .outcomes
        .iter()
        .find(|o| o.result == StepResult::HaltedRedacted);
    let had_redacted_step = playbook.steps.iter().any(|s| s.payload_redacted);

    println!("  playbook contained a redacted step : {had_redacted_step}");
    println!("  replay halted on it                : {}", halted.is_some());

    // The run status must agree with the outcomes -- not be asserted separately.
    // A redaction halt is `aborted` (declined), anything else that stops the
    // run is `failed` (went wrong).
    let expected_status = if halted.is_some() {
        "aborted"
    } else if result.outcomes.iter().any(|o| o.result.is_failure()) {
        "failed"
    } else {
        "completed"
    };
    let status_agrees = run.status == expected_status;
    println!("  runs.status {:?} matches outcomes  : {}", run.status, status_agrees);
    println!("  runs.completed_at set              : {}", run.completed_at.is_some());
    println!("  runs.billable false (local)        : {}", !run.billable);

    // One journal row per attempted step, in order.
    let rows_match = logs.len() == result.steps_attempted();
    println!("  run_steps_log rows == steps tried  : {rows_match} ({} vs {})", logs.len(), result.steps_attempted());

    // The redacted row must be marked sensitive with a NULL payload, and must
    // not contain the marker anywhere.
    let all_logs = logs
        .iter()
        .map(|l| format!("{}{}", l.target_ui_context_json, l.data_payload.clone().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    let marker_leaked = all_logs.contains(FAKE_SECRET) || all_logs.contains("[REDACTED]");
    println!("  no secret/placeholder in log rows  : {}", !marker_leaked);

    let sensitive_rows_null: bool = logs
        .iter()
        .filter(|l| l.is_sensitive)
        .all(|l| l.data_payload.is_none());
    println!("  sensitive rows have NULL payload   : {sensitive_rows_null}");

    // ---- verdict ----------------------------------------------------------
    println!("\n== result ==");
    if !status_agrees || !rows_match || !sensitive_rows_null {
        eprintln!("FAIL: the journal does not honestly reflect the replay.");
        return ExitCode::FAILURE;
    }
    if marker_leaked {
        eprintln!("FAIL: a secret or placeholder value reached run_steps_log.");
        return ExitCode::FAILURE;
    }

    if had_redacted_step && halted.is_none() {
        eprintln!("FAIL: the playbook had a redacted step but replay did not halt on it.");
        return ExitCode::FAILURE;
    }

    if let Some(h) = halted {
        println!("PASS: replay ran, then halted at step {} as designed.", h.step_order);
        println!(
            "      run recorded as {:?} (declined, not an error) with an honest reason.",
            run.status
        );
    } else if run.status == "completed" {
        println!("PASS: replay executed every step and the run completed.");
    } else {
        println!("PASS: replay reported an honest failure (no redacted step involved).");
        println!("      This is a real failure being reported correctly, not a crash.");
    }

    if !had_redacted_step {
        println!("\nNOTE: no redacted step was recorded, so the halt path was not");
        println!("exercised live this run. It is covered by unit tests.");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// Click with the same coordinate fallback the replay module uses.
///
/// Plain `element.click()` is refused for anything on a secondary monitor (see
/// docs/known-issues/terminator-multi-monitor-visibility.md). The first version
/// of this probe used bare `click()` here, every driven click failed, focus
/// never left the password field, and its completion event never fired -- so
/// the redacted step was never recorded.
fn click_with_fallback(desktop: &Desktop, el: &terminator::UIElement) -> Result<String, String> {
    match el.click() {
        Ok(_) => Ok("element.click()".to_string()),
        Err(terminator::AutomationError::ElementNotVisible(msg)) => match el.bounds() {
            Ok((x, y, w, h)) if w > 0.0 && h > 0.0 => {
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                desktop
                    .click_at_coordinates(cx, cy)
                    .map(|()| format!("coordinate click ({cx:.0}, {cy:.0}) after {msg}"))
                    .map_err(|e| format!("coordinate click failed: {e}"))
            }
            Ok((_, _, w, h)) => Err(format!("refused ({msg}) and bounds unusable: {w}x{h}")),
            Err(e) => Err(format!("refused ({msg}) and bounds() failed: {e}")),
        },
        Err(e) => Err(e.to_string()),
    }
}

/// Fill the form so the capture contains a non-redacted type, then a redacted
/// one. Every Terminator call's Result is checked and reported.
async fn drive_form(desktop: &Desktop) {
    // Username first, so its text-input-completed event fires when focus moves
    // on to the password field.
    match desktop
        .locator("role:Edit|name:Username")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(f) => {
            match click_with_fallback(desktop, &f) {
                Ok(how) => println!("  [driven] clicked Username via {how}"),
                Err(e) => println!("  [driven] Username click FAILED: {e}"),
            }
            match f.type_text("probe-user", false) {
                Ok(()) => println!("  [driven] typed into Username"),
                Err(e) => println!("  [driven] Username type FAILED: {e}"),
            }
        }
        Err(e) => println!("  [driven] Username field NOT FOUND: {e}"),
    }
    tokio::time::sleep(Duration::from_millis(700)).await;

    match desktop
        .locator("role:Edit|name:Password")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(f) => {
            match click_with_fallback(desktop, &f) {
                Ok(how) => println!("  [driven] clicked Password via {how}"),
                Err(e) => println!("  [driven] Password click FAILED: {e}"),
            }
            match f.type_text(FAKE_SECRET, false) {
                Ok(()) => println!("  [driven] typed marker into Password"),
                Err(e) => println!("  [driven] Password type FAILED: {e}"),
            }
        }
        Err(e) => println!("  [driven] Password field NOT FOUND: {e}"),
    }
    tokio::time::sleep(Duration::from_millis(700)).await;

    // Blur so the password's completion event fires.
    match desktop
        .locator("role:Button|name:Cancel")
        .first(Some(Duration::from_secs(8)))
        .await
    {
        Ok(b) => match click_with_fallback(desktop, &b) {
            Ok(how) => println!("  [driven] clicked Cancel to blur, via {how}"),
            Err(e) => println!("  [driven] Cancel click FAILED: {e}"),
        },
        Err(e) => println!("  [driven] Cancel button NOT FOUND: {e}"),
    }
}
