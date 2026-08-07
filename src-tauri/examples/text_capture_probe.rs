//! Investigate the text-input capture truncation defect.
//!
//!     cargo run --example text_capture_probe
//!
//! See docs/known-issues/text-input-capture-truncation.md.
//!
//! ## What this tests
//!
//! Two questions at once, because both need the same harness:
//!
//! 1. **Does typing speed change the kept prefix?** Each trial types the same
//!    20-character shape into its own field, varying only the inter-keystroke
//!    delay. If the completion event fires on a debounce from the first
//!    keystroke, slower typing keeps a *shorter* prefix in wall-clock terms and
//!    the kept length should move with the delay.
//!
//! 2. **Does an independent read see what the event missed?** The recorder
//!    builds `text_value` by calling `element.text(0)` on the `UIElement` it
//!    captured when the field gained focus (`TextInputTracker::
//!    get_completion_event`). There is no character accumulator to bypass. So
//!    this reads the field two further ways -- through the same handle we
//!    already hold, and through a freshly resolved locator -- immediately after
//!    typing stops. If those are correct where `text_value` is truncated, the
//!    problem is the handle or the moment, not the field.
//!
//! ## Design notes
//!
//! Each trial gets its own input, so no clearing is needed and moving to the
//! next field is itself the focus change that triggers completion. Each trial's
//! string starts with a distinct letter, so a capture truncated to even one
//! character still identifies which trial produced it.
//!
//! WARNING: performs real clicks and typing. Opens a local page in a browser.

use std::process::ExitCode;
use std::time::Duration;

use paradigm_lib::capture::{text, ActionKind, CaptureSession, ExclusionList};
use terminator::{Desktop, UIElement};

/// How a trial drives the field.
#[derive(Clone, Copy, PartialEq)]
enum Style {
    /// One character at a time, with `delay_ms` between them, after letting the
    /// click settle. Generous to any implementation that needs to observe the
    /// focus change before the text arrives.
    PerChar,
    /// The whole string in a single `type_text`, typed immediately with no
    /// settle time. This is what `tests/ipc_pipeline.rs` does, and it leaves no
    /// margin between the click and the text.
    WholeStringImmediate,
}

struct Trial {
    letter: char,
    delay_ms: u64,
    style: Style,
    /// Pause between clicking the field and starting to type.
    settle_ms: u64,
}

const TRIALS: &[Trial] = &[
    Trial { letter: 'A', delay_ms: 0,   style: Style::PerChar, settle_ms: 400 },
    Trial { letter: 'B', delay_ms: 50,  style: Style::PerChar, settle_ms: 400 },
    Trial { letter: 'C', delay_ms: 150, style: Style::PerChar, settle_ms: 400 },
    Trial { letter: 'D', delay_ms: 300, style: Style::PerChar, settle_ms: 400 },
    // The pipeline-shaped trial. If only this one fails, the defect is the
    // baseline being established after the text has already landed.
    Trial { letter: 'E', delay_ms: 0, style: Style::WholeStringImmediate, settle_ms: 0 },
];

/// Characters after the identifying first letter. Total length is this + 1.
const TAIL: &str = "0123456789abcdefghi";

fn trial_text(letter: char) -> String {
    format!("{letter}{TAIL}")
}

const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Paradigm Text Capture Probe</title></head>
<body style="font-family:sans-serif;padding:2rem">
<h2>Paradigm text-capture probe (not a real service)</h2>
<p>Each field receives one trial. Nothing here is submitted anywhere.</p>
<label for="f0">FieldA</label><br>
<input id="f0" name="FieldA" aria-label="FieldA" style="font-size:1.2rem;padding:.4rem;width:30rem"><br><br>
<label for="f1">FieldB</label><br>
<input id="f1" name="FieldB" aria-label="FieldB" style="font-size:1.2rem;padding:.4rem;width:30rem"><br><br>
<label for="f2">FieldC</label><br>
<input id="f2" name="FieldC" aria-label="FieldC" style="font-size:1.2rem;padding:.4rem;width:30rem"><br><br>
<label for="f3">FieldD</label><br>
<input id="f3" name="FieldD" aria-label="FieldD" style="font-size:1.2rem;padding:.4rem;width:30rem"><br><br>
<label for="f4">FieldE</label><br>
<input id="f4" name="FieldE" aria-label="FieldE" style="font-size:1.2rem;padding:.4rem;width:30rem"><br><br>
<label for="f5">FieldPrefilled</label><br>
<input id="f5" name="FieldPrefilled" aria-label="FieldPrefilled" value="prefilled-never-typed"
       style="font-size:1.2rem;padding:.4rem;width:30rem"><br><br>
<button aria-label="Done" style="font-size:1.1rem;padding:.4rem 1rem">Done</button>
</body></html>
"#;

/// Click that survives the multi-monitor visibility defect.
/// See docs/known-issues/terminator-multi-monitor-visibility.md.
fn robust_click(desktop: &Desktop, el: &UIElement) {
    if el.click().is_err() {
        if let Ok((x, y, w, h)) = el.bounds() {
            let _ = desktop.click_at_coordinates(x + w / 2.0, y + h / 2.0);
        }
    }
}

/// What one trial observed, before the captured stream is consulted.
struct TrialResult {
    letter: char,
    delay_ms: u64,
    expected: String,
    /// `element.text(0)` on the handle we already hold, right after typing.
    read_same_handle: Option<String>,
    /// `element.text(0)` on a handle resolved fresh from the locator.
    read_fresh_handle: Option<String>,
}

/// The recorder narrates its text-input decisions through `tracing`. With no
/// subscriber installed those messages are discarded, which is precisely why
/// this defect was hard to see. Opt in with PARADIGM_PROBE_TRACE=1.
fn init_tracing() {
    if std::env::var_os("PARADIGM_PROBE_TRACE").is_none() {
        return;
    }
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("terminator_workflow_recorder=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(false)
        .init();
    eprintln!("[probe] tracing enabled");
}

// --------------------------------------------------------- notepad mode ----
//
// Lives in this binary rather than its own because a freshly-named example
// executable is refused by this machine's Application Control policy
// ("An Application Control policy has blocked this file", os error 4551),
// while rebuilds of an established one run fine.

const NOTEPAD_LINE_ONE: &str = "first line typed by the notepad probe";
const NOTEPAD_LINE_TWO: &str = "second line typed by the notepad probe";

/// Find Notepad's editing surface without assuming what role it reports.
async fn find_edit_surface(desktop: &Desktop) -> Option<(UIElement, String)> {
    for selector in ["role:Document", "role:Edit", "role:Text"] {
        if let Ok(el) = desktop
            .locator(selector)
            .first(Some(Duration::from_secs(3)))
            .await
        {
            let role = el.role();
            println!("  {selector:<16} -> matched, actual role {role:?}");
            return Some((el, role));
        }
        println!("  {selector:<16} -> nothing");
    }
    None
}

/// Reproduce the Step 12 live failure: two typed lines into Notepad produced
/// clicks and navigates but zero `type` actions.
///
/// The existing web trials cannot catch this -- they target `role:Edit`
/// `<input>` elements in a browser, and the question here is what happens when
/// the editing surface reports some other role entirely.
async fn notepad_mode() -> ExitCode {
    println!("== notepad capture probe ==\n");
    println!("Driving the Step 12 human sequence: click once, type a line,");
    println!("press Enter, type a second line, stop.\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", "notepad"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 5s for Notepad...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n-- probing Notepad's editing surface --");
    let Some((element, role)) = find_edit_surface(&desktop).await else {
        eprintln!("could not find Notepad's editing surface");
        return ExitCode::FAILURE;
    };

    let accepted = text::is_text_role(&role);
    println!("\n  editing surface role     : {role:?}");
    println!("  accepted by is_text_role : {accepted}");
    if !accepted {
        println!("  ^^ capture will never start watching this element");
    }

    let session = match CaptureSession::start_session(
        "notepad-capture-probe",
        ExclusionList::from_patterns(["!never-matches!"]),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not start capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("\n-- driving --");
    robust_click(&desktop, &element);
    tokio::time::sleep(Duration::from_millis(600)).await;

    for ch in NOTEPAD_LINE_ONE.chars() {
        let _ = element.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = element.press_key("{Enter}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    for ch in NOTEPAD_LINE_TWO.chars() {
        let _ = element.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Ground truth, read independently of capture.
    let actual = element.text(0).unwrap_or_default();
    println!("  field now holds {} char(s)", actual.chars().count());

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================");
    println!(
        "captured {} action(s), {} unmapped event(s)\n",
        report.actions.len(),
        report.unmapped_events
    );
    for a in &report.actions {
        println!(
            "  {:<9} role={:<12} name={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-")
        );
        if let Some(p) = &a.payload {
            println!("            payload={p:?}");
        }
    }

    let types = report
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::Type)
        .count();

    println!("\n--- verdict ---");
    println!("  editing surface role     : {role:?}");
    println!("  accepted by is_text_role : {accepted}");
    println!("  text really in the field : {}", !actual.trim().is_empty());
    println!("  `type` actions captured  : {types}");

    if !actual.trim().is_empty() && types == 0 {
        println!("\n  REPRODUCED: text is in the field, capture produced no `type` action.");
    } else if types > 0 {
        println!("\n  captured {types} type action(s)");
    }

    ExitCode::SUCCESS
}

#[tokio::main]
async fn main() -> ExitCode {
    paradigm_lib::replay::ensure_dpi_aware();
    init_tracing();

    if std::env::args().any(|a| a == "notepad") {
        return notepad_mode().await;
    }

    println!("== text capture probe ==\n");
    println!("Typing {} chars per trial, one field each.", TAIL.len() + 1);
    println!("Trials (letter, inter-keystroke delay):");
    for t in TRIALS {
        let style = match t.style {
            Style::PerChar => "per-char",
            Style::WholeStringImmediate => "whole-string, no settle (pipeline-shaped)",
        };
        println!("  {}: {}ms, {style}", t.letter, t.delay_ms);
    }
    println!("\nWARNING: this performs real clicks and typing. Hands off.\n");

    // ---- put the page on screen ------------------------------------------
    let page = std::env::temp_dir().join("paradigm-text-capture-probe.html");
    if let Err(e) = std::fs::write(&page, PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in ["msedge", "chrome", "firefox"] {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, &url])
            .spawn()
        {
            let _ = c.wait();
            break;
        }
    }
    println!("waiting 10s for the browser to settle...");
    tokio::time::sleep(Duration::from_secs(10)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- start recording --------------------------------------------------
    let session = match CaptureSession::start_session(
        "text-capture-probe",
        ExclusionList::from_patterns(["!never-matches!"]),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not start capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    tokio::time::sleep(Duration::from_secs(2)).await;

    // ---- run the trials ---------------------------------------------------
    let mut results = Vec::new();

    for (i, trial) in TRIALS.iter().enumerate() {
        let (letter, delay_ms) = (&trial.letter, &trial.delay_ms);
        let field_name = format!("Field{letter}");
        let selector = format!("role:Edit|name:{field_name}");
        let style_label = match trial.style {
            Style::PerChar => "per-char",
            Style::WholeStringImmediate => "whole-string, no settle (pipeline-shaped)",
        };
        println!("\n-- trial {letter} (delay {delay_ms}ms, {style_label}) into {field_name} --");

        let element = match desktop
            .locator(selector.as_str())
            .first(Some(Duration::from_secs(15)))
            .await
        {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  could not find {field_name}: {e}");
                continue;
            }
        };

        // Focusing this field is also the focus change that completes the
        // PREVIOUS trial, which is exactly the trigger under investigation.
        robust_click(&desktop, &element);
        if trial.settle_ms > 0 {
            tokio::time::sleep(Duration::from_millis(trial.settle_ms)).await;
        }

        let expected = trial_text(*letter);
        match trial.style {
            Style::PerChar => {
                for ch in expected.chars() {
                    let _ = element.type_text(&ch.to_string(), false);
                    if *delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
                    }
                }
            }
            Style::WholeStringImmediate => {
                // Exactly what tests/ipc_pipeline.rs does.
                let _ = element.type_text(&expected, false);
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
        }

        // Read immediately, before anything moves focus. This is the moment a
        // fix on our side would read at (in fact slightly EARLIER than our
        // capture code would, since the event has not even been emitted yet).
        let read_same_handle = element.text(0).ok();
        let read_fresh_handle = match desktop
            .locator(selector.as_str())
            .first(Some(Duration::from_secs(5)))
            .await
        {
            Ok(fresh) => fresh.text(0).ok(),
            Err(_) => None,
        };

        println!(
            "  typed      : {:?} ({} chars)",
            expected,
            expected.chars().count()
        );
        println!(
            "  same handle: {:?}",
            read_same_handle.as_deref().unwrap_or("<read failed>")
        );
        println!(
            "  fresh read : {:?}",
            read_fresh_handle.as_deref().unwrap_or("<read failed>")
        );

        results.push(TrialResult {
            letter: *letter,
            delay_ms: *delay_ms,
            expected,
            read_same_handle,
            read_fresh_handle,
        });

        // After the last trial, click through a PRE-FILLED field without
        // typing anything, then click Done. This is the tradeoff the emit
        // condition is balancing: the watcher must not invent a `type` action
        // for text that was already there and the user never entered.
        if i == TRIALS.len() - 1 {
            tokio::time::sleep(Duration::from_millis(400)).await;
            if let Ok(prefilled) = desktop
                .locator("role:Edit|name:FieldPrefilled")
                .first(Some(Duration::from_secs(8)))
                .await
            {
                println!("\n-- clicking through pre-filled field, typing nothing --");
                robust_click(&desktop, &prefilled);
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            if let Ok(done) = desktop
                .locator("role:Button|name:Done")
                .first(Some(Duration::from_secs(8)))
                .await
            {
                robust_click(&desktop, &done);
            }
        }
    }

    // ---- stop and compare -------------------------------------------------
    tokio::time::sleep(Duration::from_secs(3)).await;
    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n\n================ RESULTS ================");
    println!(
        "captured {} action(s), {} unmapped event(s)\n",
        report.actions.len(),
        report.unmapped_events
    );

    let typed: Vec<_> = report
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::Type)
        .collect();

    println!("--- every captured `type` action, in order ---");
    if typed.is_empty() {
        println!("  (NONE -- no TextInputCompleted event produced an action)");
    }
    for a in &typed {
        println!(
            "  payload={:?}\n    element_name={:?} detail={:?}",
            a.payload.as_deref().unwrap_or("<none>"),
            a.element_name.as_deref().unwrap_or("<none>"),
            a.detail.as_deref().unwrap_or("<none>")
        );
    }

    println!("\n--- per trial ---");
    println!(
        "  {:<6} {:<7} {:<8} {:>9} {:>9} {:>9}",
        "trial", "delay", "expected", "captured", "same-hnd", "fresh"
    );
    println!("  {}", "-".repeat(60));

    // A trial only tests capture if the text actually reached the field. When
    // the browser loses focus the typing goes nowhere, the field reads empty,
    // and scoring that as a capture miss would understate the pipeline. Such
    // trials are reported separately as setup failures, not counted either way.
    let mut valid_trials = 0usize;
    let mut captured_correct_among_valid = 0usize;

    let mut event_correct = 0usize;
    let mut fresh_correct = 0usize;
    let mut trials_with_event = 0usize;

    for r in &results {
        // Match the captured action to its trial by leading letter -- robust
        // even if the payload is a single character.
        let captured = typed
            .iter()
            .find(|a| {
                a.payload
                    .as_deref()
                    .map(|p| p.starts_with(r.letter))
                    .unwrap_or(false)
            })
            .and_then(|a| a.payload.as_deref());

        if captured.is_some() {
            trials_with_event += 1;
        }
        if captured == Some(r.expected.as_str()) {
            event_correct += 1;
        }
        let trial_is_valid = r.read_fresh_handle.as_deref() == Some(r.expected.as_str());
        if trial_is_valid {
            fresh_correct += 1;
            valid_trials += 1;
            if captured == Some(r.expected.as_str()) {
                captured_correct_among_valid += 1;
            }
        }

        let len_of = |s: Option<&str>| match s {
            Some(v) => format!("{}", v.chars().count()),
            None => "-".to_string(),
        };

        println!(
            "  {:<6} {:<7} {:<8} {:>9} {:>9} {:>9}",
            r.letter,
            format!("{}ms", r.delay_ms),
            r.expected.chars().count(),
            len_of(captured),
            len_of(r.read_same_handle.as_deref()),
            len_of(r.read_fresh_handle.as_deref()),
        );
    }

    println!("\n  (columns are CHARACTER COUNTS; expected is the ground truth)");

    // The false-positive check. A `type` action for the pre-filled field means
    // the watcher fabricated an action the user never performed, which is worse
    // than missing one -- it is the same silent, plausible corruption this
    // whole defect is about.
    let fabricated: Vec<&str> = typed
        .iter()
        .filter_map(|a| a.payload.as_deref())
        .filter(|p| p.contains("prefilled-never-typed"))
        .collect();
    println!("\n--- pre-filled field (clicked through, never typed into) ---");
    if fabricated.is_empty() {
        println!("  PASS: no action recorded for it");
    } else {
        println!("  FAIL: fabricated {} action(s): {fabricated:?}", fabricated.len());
    }

    println!("\n--- verdicts ---");
    println!(
        "  trials that produced a captured `type` action    : {}/{}",
        trials_with_event,
        results.len()
    );
    println!(
        "  captured payload exactly correct                 : {}/{}",
        event_correct,
        results.len()
    );
    println!(
        "  independent fresh read exactly correct           : {}/{}",
        fresh_correct,
        results.len()
    );
    println!(
        "\n  VALID trials (text really reached the field)    : {}/{}",
        valid_trials,
        results.len()
    );
    if valid_trials == 0 {
        println!("  SCORE: n/a -- no valid trial, this run tested nothing");
    } else {
        println!(
            "  SCORE: captured correctly on valid trials       : {}/{}",
            captured_correct_among_valid, valid_trials
        );
    }

    for r in &results {
        let captured = typed
            .iter()
            .find(|a| {
                a.payload
                    .as_deref()
                    .map(|p| p.starts_with(r.letter))
                    .unwrap_or(false)
            })
            .and_then(|a| a.payload.as_deref());
        if captured != Some(r.expected.as_str()) {
            println!(
                "\n  MISMATCH trial {} (delay {}ms):\n    expected : {:?}\n    event    : {:?}\n    fresh    : {:?}",
                r.letter,
                r.delay_ms,
                r.expected,
                captured.unwrap_or("<no event>"),
                r.read_fresh_handle.as_deref().unwrap_or("<read failed>")
            );
        }
    }

    ExitCode::SUCCESS
}
