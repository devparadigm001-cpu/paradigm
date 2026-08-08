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

// --------------------------------------------------------- handles mode ----
//
// Answers the question left open when `begin_from_keystroke` was reverted:
// is the element from `Desktop::focused_element()` the same handle, as far as
// our code can tell, as the one the recorder attaches to a Click event?
//
// It matters because `TextFieldWatcher::focus_moved` compares them:
//
//     if same_element(new, &current.element) { return None; }   // keep going
//     let leaving = self.flush(timestamp_ms);                   // else FLUSH
//
// If a watch started from `focused_element()` and the later Click event
// carries a handle that does not compare equal, the click looks like a move to
// a different field. The watcher flushes and re-baselines against text that has
// already been typed, so the final flush sees no change and emits nothing --
// which would explain the settled case regressing from 5/5 to 0/5.

/// `same_element`'s logic, duplicated because it is private to `capture::text`.
/// Kept identical on purpose; if that function changes this must too.
fn would_compare_equal(a: &UIElement, b: &UIElement) -> bool {
    match (a.id(), b.id()) {
        (Some(x), Some(y)) => x == y,
        _ => a.role() == b.role() && a.name() == b.name(),
    }
}

fn describe(label: &str, el: &UIElement) {
    println!("  {label}:");
    println!("    id   = {:?}", el.id());
    println!("    role = {:?}", el.role());
    println!("    name = {:?}", el.name());
    match el.text(0) {
        Ok(t) => println!("    text = {:?} ({} chars)", t, t.chars().count()),
        Err(e) => println!("    text = <read failed: {e}>"),
    }
}

async fn handles_mode() -> ExitCode {
    use futures::StreamExt;
    use terminator_workflow_recorder::{
        WorkflowEvent, WorkflowRecorder, WorkflowRecorderConfig,
    };

    println!("== handle comparison probe ==\n");
    println!("Compares the element the recorder attaches to a Click event against");
    println!("the one Desktop::focused_element() returns for the same field.\n");
    println!("WARNING: performs a real click. Hands off.\n");

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
    println!("waiting 10s for the browser...");
    tokio::time::sleep(Duration::from_secs(10)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    let field = match desktop
        .locator("role:Edit|name:FieldA")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("could not find FieldA: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Subscribe before start(): the channel is a broadcast, so a later
    // subscription would miss everything in between.
    let config = WorkflowRecorderConfig {
        record_mouse: true,
        record_keyboard: true,
        capture_ui_elements: true,
        ..Default::default()
    };
    let mut recorder = WorkflowRecorder::new("handle-probe".to_string(), config);
    let mut events = Box::pin(recorder.event_stream());
    if let Err(e) = recorder.start().await {
        eprintln!("could not start recorder: {e}");
        return ExitCode::FAILURE;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("-- clicking FieldA --");
    robust_click(&desktop, &field);

    // Resolve focus immediately, the way `begin_from_keystroke` did.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let focused = desktop.focused_element().ok();

    // Then wait for the recorder's Click event for the same click.
    let mut click_element = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), events.next()).await {
            Ok(Some(WorkflowEvent::Click(e))) => {
                if let Some(el) = e.metadata.ui_element.clone() {
                    println!(
                        "  got Click event: role={:?} text={:?}",
                        e.element_role, e.element_text
                    );
                    click_element = Some(el);
                    break;
                }
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    let _ = recorder.stop().await;

    println!("\n================ RESULTS ================\n");

    let Some(click_el) = click_element else {
        println!("  no Click event carrying an element arrived -- inconclusive");
        return ExitCode::FAILURE;
    };
    let Some(focused_el) = focused else {
        println!("  focused_element() returned nothing -- inconclusive");
        return ExitCode::FAILURE;
    };

    describe("from the recorder's Click event", &click_el);
    println!();
    describe("from Desktop::focused_element()", &focused_el);

    let equal = would_compare_equal(&click_el, &focused_el);
    println!("\n--- verdict ---");
    println!("  same_element() would consider them equal : {equal}");
    if equal {
        println!("\n  H1 REFUTED: the handles compare equal, so a watch started from");
        println!("  focused_element() would NOT be spuriously flushed by the later click.");
    } else {
        println!("\n  H1 SUPPORTED: the handles do NOT compare equal. A watch started");
        println!("  from focused_element() would be flushed and re-baselined when the");
        println!("  click event arrived -- destroying the capture.");
    }

    ExitCode::SUCCESS
}

// -------------------------------------------------------- pumpcost mode ----
//
// Measures what the reverted fix actually cost. `begin_from_keystroke` called
// `Desktop::focused_element()` from inside the event pump, holding the watcher
// lock, on every typing keystroke while nothing was being watched. This
// reproduces that shape and times each call.
//
// If the calls are slow, the pump falls behind the recorder's broadcast
// channel. That channel drops events silently on lag --
// `Lagged(skipped) => continue`, reported through `tracing`, which goes nowhere
// unless a subscriber is installed. Run with PARADIGM_PROBE_TRACE=1 to see the
// "Event stream LAGGED!" line if it happens.

async fn pumpcost_mode() -> ExitCode {
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;
    use terminator_workflow_recorder::{
        WorkflowEvent, WorkflowRecorder, WorkflowRecorderConfig,
    };

    println!("== pump cost probe ==\n");
    println!("Times Desktop::focused_element() called from inside the event pump,");
    println!("the way the reverted begin_from_keystroke fix did.\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

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
    println!("waiting 10s for the browser...");
    tokio::time::sleep(Duration::from_secs(10)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    let field = match desktop
        .locator("role:Edit|name:FieldA")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("could not find FieldA: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A baseline, measured from the main task with the pump idle.
    let t0 = std::time::Instant::now();
    let _ = desktop.focused_element();
    println!("baseline focused_element() from an idle task: {:?}\n", t0.elapsed());

    let config = WorkflowRecorderConfig {
        record_mouse: true,
        record_keyboard: true,
        capture_ui_elements: true,
        ..Default::default()
    };
    let mut recorder = WorkflowRecorder::new("pumpcost-probe".to_string(), config);
    let mut events = Box::pin(recorder.event_stream());
    if let Err(e) = recorder.start().await {
        eprintln!("could not start recorder: {e}");
        return ExitCode::FAILURE;
    }

    let events_seen = StdArc::new(AtomicUsize::new(0));
    let keys_seen = StdArc::new(AtomicUsize::new(0));
    let pump_events = StdArc::clone(&events_seen);
    let pump_keys = StdArc::clone(&keys_seen);

    // Collected through a shared handle rather than the task's return value:
    // the task is aborted to stop it, and an aborted JoinHandle yields
    // Cancelled, silently discarding everything it had gathered.
    // (latency, resolved role, resolved name) per keystroke, so we can see
    // WHAT focus resolved to and not merely how long it took.
    type Resolution = (Duration, String, Option<String>);
    let timings_shared: StdArc<std::sync::Mutex<Vec<Resolution>>> =
        StdArc::new(std::sync::Mutex::new(Vec::new()));
    let pump_timings = StdArc::clone(&timings_shared);

    // The pump, shaped like ours: serial, and doing the UIA call inline.
    // Passed in from the caller's thread, exactly as the reverted fix did --
    // it built the Desktop in start_session and moved it into the pump.
    let shared_desktop = StdArc::new(match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("could not build the shared Desktop: {e}");
            return ExitCode::FAILURE;
        }
    });
    let pump_shared = StdArc::clone(&shared_desktop);

    let pump = tokio::spawn(async move {
        // Two candidates: one built HERE (inside the task) and one built on the
        // caller's thread and moved in. If UIA is apartment-bound, these behave
        // differently, and the reverted fix used the second.
        let inner = match Desktop::new_default() {
            Ok(d) => {
                println!("  [pump] Desktop::new_default() inside the task: OK");
                Some(d)
            }
            Err(e) => {
                println!("  [pump] Desktop::new_default() inside the task FAILED: {e}");
                None
            }
        };

        let probe_target = inner.as_ref().unwrap_or(&pump_shared);
        match probe_target.focused_element() {
            Ok(el) => println!(
                "  [pump] focused_element() works from the pump: role={:?}",
                el.role()
            ),
            Err(e) => println!("  [pump] focused_element() FAILED from the pump: {e}"),
        }

        let mut timings: Vec<Duration> = Vec::new();

        while let Some(event) = events.next().await {
            pump_events.fetch_add(1, Ordering::Relaxed);

            if let WorkflowEvent::Keyboard(e) = &event {
                if e.is_key_down && paradigm_lib::capture::text::is_typing_key(e.key_code) {
                    pump_keys.fetch_add(1, Ordering::Relaxed);
                    let d = inner.as_ref().unwrap_or(&pump_shared);
                    let t = std::time::Instant::now();
                    let got = d.focused_element();
                    let elapsed = t.elapsed();
                    timings.push(elapsed);

                    // Record what focus actually resolved to. This is the
                    // question: does it name the field being typed into, or
                    // something else (an address bar, the document, a stale
                    // element) because OS focus has not settled?
                    let (role, name) = match &got {
                        Ok(el) => (el.role(), el.name()),
                        Err(e) => (format!("<err: {e}>"), None),
                    };
                    if let Ok(mut shared) = pump_timings.lock() {
                        shared.push((elapsed, role, name));
                    }
                }
            }
        }
        timings
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // PHASE 1 -- settled: click, wait 400ms, then type. Trial A's shape, the
    // case that regressed 5/5 -> 0/5 and is supposed to work.
    println!("-- PHASE 1 (settled): click FieldA, wait 400ms, then type --");
    robust_click(&desktop, &field);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let typed = trial_text('A');
    let type_start = std::time::Instant::now();
    for ch in typed.chars() {
        let _ = field.type_text(&ch.to_string(), false);
    }
    println!("   typing took {:?} of wall clock", type_start.elapsed());
    tokio::time::sleep(Duration::from_secs(3)).await;

    let phase1_len = timings_shared.lock().map(|t| t.len()).unwrap_or(0);

    // PHASE 2 -- no settle: click and type immediately, the trial E / ipc_pipeline
    // shape. This is where OS focus may genuinely not have settled, and the
    // case never previously measured.
    println!("\n-- PHASE 2 (no settle): click FieldB and type IMMEDIATELY --");
    if let Ok(field_b) = desktop
        .locator("role:Edit|name:FieldB")
        .first(Some(Duration::from_secs(10)))
        .await
    {
        robust_click(&desktop, &field_b);
        let _ = field_b.type_text(&trial_text('B'), false);
    } else {
        println!("   could not find FieldB -- phase 2 skipped");
    }

    tokio::time::sleep(Duration::from_secs(5)).await;
    let _ = recorder.stop().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    pump.abort();
    let _ = pump.await;
    let resolutions: Vec<Resolution> =
        timings_shared.lock().map(|t| t.clone()).unwrap_or_default();
    let timings: Vec<Duration> = resolutions.iter().map(|(d, _, _)| *d).collect();

    println!("\n================ RESULTS ================\n");
    println!("  events reaching the pump : {}", events_seen.load(Ordering::Relaxed));
    println!("  typing keydowns seen     : {}", keys_seen.load(Ordering::Relaxed));
    println!("  focused_element() calls  : {}", timings.len());

    if timings.is_empty() {
        println!("\n  no calls timed -- inconclusive");
        return ExitCode::FAILURE;
    }

    let total: Duration = timings.iter().sum();
    let max = timings.iter().max().copied().unwrap_or_default();
    let min = timings.iter().min().copied().unwrap_or_default();
    let mean = total / timings.len() as u32;

    println!("\n  per-call latency: min {min:?}, mean {mean:?}, max {max:?}");
    println!("  TOTAL time the pump spent blocked: {total:?}");

    // The decisive part: WHAT did focus resolve to on the first keystroke of
    // each phase? If the no-settle phase names something other than FieldB --
    // an address bar, the document, a stale element -- that is the mechanism.
    println!("\n--- what focus resolved to, per phase ---");
    let show = |label: &str, slice: &[Resolution]| {
        println!("  {label}:");
        if slice.is_empty() {
            println!("    (no keystrokes recorded)");
            return;
        }
        println!(
            "    FIRST keystroke -> role={:?} name={:?}",
            slice[0].1, slice[0].2
        );
        let mut distinct: Vec<String> = slice
            .iter()
            .map(|(_, r, n)| format!("{r:?}/{n:?}"))
            .collect();
        distinct.dedup();
        distinct.sort();
        distinct.dedup();
        println!("    distinct targets across the phase: {distinct:?}");
    };
    show("PHASE 1 (settled)", &resolutions[..phase1_len.min(resolutions.len())]);
    if resolutions.len() > phase1_len {
        show("PHASE 2 (no settle)", &resolutions[phase1_len..]);
    } else {
        println!("  PHASE 2 (no settle): (no keystrokes recorded)");
    }
    println!("\n  (the pump is serial, so this is time during which NO event --");
    println!("   including the click events the settled case depends on -- was");
    println!("   being processed)");

    ExitCode::SUCCESS
}

#[tokio::main]
async fn main() -> ExitCode {
    paradigm_lib::replay::ensure_dpi_aware();
    init_tracing();

    if std::env::args().any(|a| a == "notepad") {
        return notepad_mode().await;
    }
    if std::env::args().any(|a| a == "handles") {
        return handles_mode().await;
    }
    if std::env::args().any(|a| a == "pumpcost") {
        return pumpcost_mode().await;
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
