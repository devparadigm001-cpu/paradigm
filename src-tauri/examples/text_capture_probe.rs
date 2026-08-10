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
<label for="ta">FieldMultiline</label><br>
<textarea id="ta" name="FieldMultiline" aria-label="FieldMultiline" rows="4"
       style="font-size:1.2rem;padding:.4rem;width:30rem"></textarea><br><br>
<label for="f5">FieldPrefilled</label><br>
<input id="f5" name="FieldPrefilled" aria-label="FieldPrefilled" value="prefilled-never-typed"
       style="font-size:1.2rem;padding:.4rem;width:30rem"><br><br>
<button aria-label="Done" style="font-size:1.1rem;padding:.4rem 1rem">Done</button>
</body></html>
"#;

/// Which browser to open the probe page in.
///
/// Selectable so tab load can be varied deliberately. Trial E's success rate is
/// suspected to depend on how quickly the recorder's click event is processed,
/// which a large UI Automation tree slows down -- and a browser carrying ~90
/// tabs against one carrying a handful is the cleanest way to test that without
/// closing anyone's windows.
///
///     ... -- chrome     force Chrome
///     ... -- edge       force Edge
///     (default)         first that launches
fn browser_order() -> Vec<&'static str> {
    if std::env::args().any(|a| a == "chrome") {
        vec!["chrome"]
    } else if std::env::args().any(|a| a == "edge") {
        vec!["msedge"]
    } else {
        vec!["msedge", "chrome", "firefox"]
    }
}

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

/// Find the editing surface of the Notepad this probe just launched, anchored
/// on keyboard focus rather than on a desktop-wide search.
///
/// ## Why focus, and not a locator
///
/// A global `role:Document` search is what this used to do, and it is unsafe:
/// on one run it matched a Spotify tab in the user's browser and typed two
/// lines of probe text into it. Any selector that can match another
/// application's window is capable of that.
///
/// Process-id scoping was tried and failed (`locator("role:Window").all()`
/// returned no Notepad window at all), as did matching a uniquely-named file.
/// Focus is the remaining anchor, and unlike those it is *measured*: the
/// `pumpcost` experiment resolved `focused_element()` correctly 40 times out of
/// 40, in both settled and no-settle shapes, at 4-7ms per call. A freshly
/// launched Notepad owns the foreground, so its editing surface is what has
/// focus.
///
/// ## Two guards, both of which must pass
///
/// 1. the focused element's window must be named like Notepad, and
/// 2. the surface must be **empty** -- a fresh Notepad is.
///
/// Either failing aborts the run. There is deliberately no fallback to a wider
/// search: not finding our own window must stop the probe, not broaden it.
async fn find_focused_edit_surface(
    desktop: &Desktop,
    launched_pid: u32,
) -> Option<(UIElement, String)> {
    let mut described = false;

    for _ in 0..20 {
        if let Ok(el) = desktop.focused_element() {
            let role = el.role();
            let window_name = el
                .window()
                .ok()
                .flatten()
                .and_then(|w| w.name())
                .unwrap_or_default();
            let pid = el.process_id().ok();

            if !described {
                println!(
                    "  focus -> role={role:?} window={window_name:?} pid={pid:?} (launched {launched_pid})"
                );
                described = true;
            }

            // GUARD 1: identity. Window names come back empty for Notepad in
            // this environment -- Get-Process reports no MainWindowTitle and a
            // UIA role:Window sweep finds no Notepad window at all -- so the
            // process id is the identity signal that is actually available.
            // The window name is still accepted when present.
            let is_ours =
                pid == Some(launched_pid) || window_name.contains("Notepad");
            if !is_ours {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }

            if !text::is_text_role(&role) {
                println!("  (focused element is not an editable role yet)");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }

            // GUARD 2: a fresh Notepad is empty. Anything else means this is
            // not the window we just opened -- refuse rather than type into it.
            match el.text(0) {
                Ok(t) if t.trim().is_empty() => {
                    println!("  confirmed: Notepad window, editable role, empty");
                    return Some((el, role));
                }
                Ok(t) => {
                    eprintln!(
                        "  REFUSING: focused Notepad surface is not empty ({} chars). \
                         This is not the blank window this probe opened.",
                        t.chars().count()
                    );
                    return None;
                }
                Err(e) => {
                    eprintln!("  REFUSING: could not read the surface to verify it is empty: {e}");
                    return None;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
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

    // Spawned directly, not via `cmd /C start`, so the pid is ours to compare.
    let child = match std::process::Command::new("notepad.exe").spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not launch Notepad: {e}");
            return ExitCode::FAILURE;
        }
    };
    let launched_pid = child.id();
    println!("launched Notepad, pid {launched_pid}");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n-- anchoring on keyboard focus (a fresh Notepad owns it) --");
    let Some((element, role)) = find_focused_edit_surface(&desktop, launched_pid).await else {
        eprintln!(
            "could not confirm a blank Notepad surface has focus. Aborting rather \
             than typing into a window this probe does not own."
        );
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

    // The decisive check. Replay types each payload in order with no clearing
    // (`send_text`, key-by-key), so concatenating the payloads is exactly what
    // a replay would produce. If that does not equal what is really in the
    // field, replay writes the wrong thing -- duplicated text being the case
    // this fix is about.
    let replayed: String = report
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::Type)
        .filter_map(|a| a.payload.as_deref())
        .collect::<Vec<_>>()
        .join("");

    let norm = |s: &str| s.replace("\r\n", "\n").replace('\r', "\n");
    let matches = norm(&replayed) == norm(&actual);

    println!("\n--- what a replay would produce ---");
    println!("  concatenated payloads : {replayed:?}");
    println!("  actually in the field : {actual:?}");
    println!("  replay would match    : {matches}");
    if !matches {
        println!(
            "  DUPLICATION/MISMATCH: replay writes {} chars, field holds {}",
            norm(&replayed).chars().count(),
            norm(&actual).chars().count()
        );
    }

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
    for browser in browser_order() {
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

    // Is role a sound proxy for "multi-line"? Notepad's editing surface is a
    // Document and is multi-line; a single-line <input> is an Edit. The open
    // question is what a <textarea> reports, because if it is also an Edit then
    // role alone cannot distinguish multi-line from single-line, and any fix
    // keyed on `role == "document"` would silently miss web textareas.
    //
    // Terminator exposes no multiline property (no accessor, and the attribute
    // bag carries AutomationId only), so this behavioural check is the only
    // signal available without dropping to raw UIA.
    println!("\n-- is role a sound multi-line proxy? --");
    println!("  single-line <input> FieldA : role={:?}", field.role());
    match desktop
        .locator("role:Edit|name:FieldMultiline")
        .first(Some(Duration::from_secs(5)))
        .await
    {
        Ok(ta) => println!("  <textarea> FieldMultiline  : role={:?}  (as role:Edit)", ta.role()),
        Err(_) => match desktop
            .locator("role:Document|name:FieldMultiline")
            .first(Some(Duration::from_secs(5)))
            .await
        {
            Ok(ta) => println!(
                "  <textarea> FieldMultiline  : role={:?}  (as role:Document)",
                ta.role()
            ),
            Err(e) => println!("  <textarea> FieldMultiline  : not found either way: {e}"),
        },
    }

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
    for browser in browser_order() {
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

// ------------------------------------------------------- multiline mode ----
//
// The multi-line surface used for the duplication work, in place of Notepad.
//
// Notepad turned out not to be safely targetable here: Windows 11 hands a new
// `notepad.exe` launch off to an already-running instance (measured -- focus
// landed on pid 18552 while the probe had launched 9596), window names come
// back empty, and a desktop-wide `role:Document` search once matched a browser
// tab and typed into it. A <textarea> in the probe's own page has none of those
// problems: `role:Edit|name:FieldMultiline` can only match a page this probe
// wrote.
//
// It is also the more important target. A <textarea> reports role "Edit",
// identical to a single-line <input>, which is exactly why the duplication fix
// could not be keyed on role.
//
// Exit path is selectable so both flush routes can be checked:
//   ... -- multiline        click away (the Done button)
//   ... -- multiline tab    Tab out of the field

async fn multiline_mode() -> ExitCode {
    let use_tab = std::env::args().any(|a| a == "tab");

    println!("== multiline (textarea) probe ==\n");
    println!(
        "Types two lines into a <textarea>, then exits via {}.\n",
        if use_tab { "TAB" } else { "CLICK-AWAY" }
    );
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    let page = std::env::temp_dir().join("paradigm-text-capture-probe.html");
    if let Err(e) = std::fs::write(&page, PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in browser_order() {
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

    let area = match desktop
        .locator("role:Edit|name:FieldMultiline")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("could not find the textarea: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("textarea role: {:?} (same as a single-line input)", area.role());

    let session = match CaptureSession::start_session(
        "multiline-probe",
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

    println!("\n-- typing two lines separated by Enter --");
    robust_click(&desktop, &area);
    tokio::time::sleep(Duration::from_millis(600)).await;
    for ch in "alpha line".chars() {
        let _ = area.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = area.press_key("{Enter}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    for ch in "beta line".chars() {
        let _ = area.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;

    let actual = area.text(0).unwrap_or_default();
    println!("   field now holds {:?}", actual);

    if use_tab {
        println!("\n-- exiting via TAB --");
        let _ = area.press_key("{Tab}");
    } else {
        println!("\n-- exiting via CLICK-AWAY (Done button) --");
        if let Ok(done) = desktop
            .locator("role:Button|name:Done")
            .first(Some(Duration::from_secs(8)))
            .await
        {
            robust_click(&desktop, &done);
        }
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================");
    println!("captured {} action(s)\n", report.actions.len());
    for a in &report.actions {
        println!(
            "  {:<9} role={:<10} name={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-")
        );
        if let Some(p) = &a.payload {
            println!("            payload={p:?}");
        }
    }

    // Replay types each payload in order with no clearing (`send_text`), so
    // concatenating them is exactly what a replay would write.
    let replayed: String = report
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::Type)
        .filter_map(|a| a.payload.as_deref())
        .collect::<Vec<_>>()
        .join("");
    let norm = |s: &str| s.replace("\r\n", "\n").replace('\r', "\n");
    let matches = norm(&replayed) == norm(&actual);
    let type_count = report
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::Type)
        .count();

    println!("\n--- verdict ---");
    println!("  exit path             : {}", if use_tab { "Tab" } else { "click-away" });
    println!("  type actions captured : {type_count}");
    println!("  concatenated payloads : {replayed:?}");
    println!("  actually in the field : {actual:?}");
    println!("  replay would match    : {matches}");

    if actual.trim().is_empty() {
        println!("\n  INCONCLUSIVE: nothing reached the field.");
    } else if type_count == 0 {
        println!("\n  FAIL: the exit path did not flush -- no type action captured.");
    } else if matches {
        println!("\n  PASS: exit path flushed, and replay reproduces the text exactly.");
    } else {
        println!(
            "\n  FAIL: replay would write {} chars, field holds {} -- duplication.",
            norm(&replayed).chars().count(),
            norm(&actual).chars().count()
        );
    }

    ExitCode::SUCCESS
}

// ---------------------------------------------------- windowswitch mode ----
//
// Reproduces a Step 12 failure: a type action attributed to the WRONG window.
//
// Session record-403c9787 typed three lines into Notepad, then switched to
// Google Docs before the field flushed. The type action was captured with
// Notepad's selector (role:document|name:"Text editor") while appearing AFTER
// the navigate-to-Google-Docs step. Replay reported 7/7 succeeded and typed the
// payload back into Notepad.
//
// The shape being tested: type into a field, then switch applications WITHOUT
// flushing first -- no Enter, no click elsewhere in the original window. If the
// watch survives the switch, its eventual flush carries the old window's
// element and lands after the navigate.
//
// Calculator is the second window: it has no text fields, so nothing can be
// typed into it by accident.

async fn windowswitch_mode() -> ExitCode {
    const TYPED: &str = "switchtest0123456789";

    println!("== window-switch attribution probe ==\n");
    println!("Types into a browser field, then switches apps WITHOUT flushing");
    println!("first -- no Enter, no click away.\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    let page = std::env::temp_dir().join("paradigm-text-capture-probe.html");
    if let Err(e) = std::fs::write(&page, PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in browser_order() {
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

    let session = match CaptureSession::start_session(
        "windowswitch-probe",
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

    println!("-- typing into FieldA (browser) --");
    robust_click(&desktop, &field);
    tokio::time::sleep(Duration::from_millis(600)).await;
    for ch in TYPED.chars() {
        let _ = field.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Straight to the switch. No Enter, no click elsewhere in the browser --
    // nothing that would flush the field first.
    println!("-- switching to Calculator WITHOUT flushing first --");
    if let Err(e) = std::process::Command::new("calc.exe").spawn() {
        eprintln!("could not launch Calculator: {e}");
    }
    tokio::time::sleep(Duration::from_secs(6)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================");
    println!("captured {} action(s), in order:\n", report.actions.len());
    for (i, a) in report.actions.iter().enumerate() {
        println!(
            "  [{i}] {:<9} role={:<10} name={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-")
        );
        println!("      source_app={:?}", a.source_app);
        if let Some(p) = &a.payload {
            println!("      payload={p:?}");
        }
    }

    // Where does the typing sit relative to the app switch?
    let type_idx = report
        .actions
        .iter()
        .position(|a| a.kind == ActionKind::Type);
    // The switch we caused, not the browser's own initial navigate.
    let nav_idx = report.actions.iter().position(|a| {
        a.kind == ActionKind::Navigate
            && a.element_name
                .as_deref()
                .map(|n| n.contains("Calculator"))
                .unwrap_or(false)
    });

    println!("\n--- verdict ---");
    println!("  type action index         : {type_idx:?}");
    println!("  switch-to-Calculator index: {nav_idx:?}");

    match (type_idx, nav_idx) {
        (None, _) => println!("\n  INCONCLUSIVE: no type action captured at all."),
        (Some(t), Some(n)) if t > n => {
            let a = &report.actions[t];
            println!(
                "\n  REPRODUCED: the typing was recorded AFTER the app switch,\n  \
                 but carries the ORIGINAL window's target (role={:?} name={:?},\n  \
                 source_app={:?}).\n  \
                 Replayed in this order it types into the wrong window.",
                a.element_role.as_deref().unwrap_or("-"),
                a.element_name.as_deref().unwrap_or("-"),
                a.source_app
            );
        }
        (Some(t), Some(n)) => println!(
            "\n  CORRECT: typing (index {t}) is ordered before the app switch (index {n})."
        ),
        (Some(_), None) => {
            println!("\n  INCONCLUSIVE: no navigate action -- the app switch was not captured.")
        }
    }

    ExitCode::SUCCESS
}

// -------------------------------------------------------- windowid mode ----
//
// Decides what a "window session id" can safely be keyed on.
//
// `ApplicationSwitchEvent` has no HWND field -- there is no `hwnd` anywhere in
// terminator-workflow-recorder. Its identity fields are the window title, the
// process name, and `to_process_id`. A window handle is still reachable through
// `metadata.ui_element` via `UIElement::get_native_window_handle()`, which would
// be the better key because it identifies a *window* rather than a *process*
// (pid would merge two Notepad tabs sharing one process).
//
// Whether that is usable is an empirical question, and this answers it:
//
//   * is `metadata.ui_element` populated on ApplicationSwitch events?
//   * does `get_native_window_handle()` succeed, and return something non-zero?
//   * is the handle STABLE when returning to a window whose TITLE has changed?
//
// The last one is the whole point. The bug being fixed
// (replay-window-selector-ambiguity.md) happened because a Notepad window was
// "Untitled - Notepad" on the first visit and "*draft note... - Notepad" on the
// return. The page used here retitles itself as text is typed, reproducing that
// exactly without going near Notepad.

const RETITLING_PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Untitled - Probe</title></head>
<body style="font-family:sans-serif;padding:2rem">
<h2>Window identity probe</h2>
<p>This page renames its own window as you type, the way Notepad does.</p>
<label for="t">FieldTitle</label><br>
<input id="t" name="FieldTitle" aria-label="FieldTitle"
       oninput="document.title = (this.value || 'Untitled') + ' - Probe'"
       style="font-size:1.2rem;padding:.4rem;width:30rem">
</body></html>
"#;

async fn windowid_mode() -> ExitCode {
    use futures::StreamExt;
    use terminator_workflow_recorder::{
        WorkflowEvent, WorkflowRecorder, WorkflowRecorderConfig,
    };

    println!("== window identity probe ==\n");
    println!("Types into a page that renames its own window, switches away,");
    println!("then switches back -- and reports what identity each switch carries.\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    let page = std::env::temp_dir().join("paradigm-window-id-probe.html");
    if let Err(e) = std::fs::write(&page, RETITLING_PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in browser_order() {
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
        .locator("role:Edit|name:FieldTitle")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("could not find FieldTitle: {e}");
            return ExitCode::FAILURE;
        }
    };

    let config = WorkflowRecorderConfig {
        record_mouse: true,
        record_keyboard: true,
        capture_ui_elements: true,
        record_application_switches: true,
        ..Default::default()
    };
    let mut recorder = WorkflowRecorder::new("windowid-probe".to_string(), config);
    let mut events = Box::pin(recorder.event_stream());
    if let Err(e) = recorder.start().await {
        eprintln!("could not start recorder: {e}");
        return ExitCode::FAILURE;
    }

    // Collect every ApplicationSwitch with the identity it carries.
    type Switch = (String, u32, Option<Result<isize, String>>);
    let collected: std::sync::Arc<std::sync::Mutex<Vec<Switch>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&collected);

    let pump = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let WorkflowEvent::ApplicationSwitch(e) = &event {
                let hwnd = e.metadata.ui_element.as_ref().map(|el| {
                    el.get_native_window_handle().map_err(|err| err.to_string())
                });
                if let Ok(mut v) = sink.lock() {
                    v.push((
                        e.to_window_and_application_name.clone(),
                        e.to_process_id,
                        hwnd,
                    ));
                }
            }
        }
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("-- typing into the field (this renames the window) --");
    robust_click(&desktop, &field);
    tokio::time::sleep(Duration::from_millis(600)).await;
    for ch in "draft note".chars() {
        let _ = field.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("-- switching away to Calculator --");
    if let Err(e) = std::process::Command::new("calc.exe").spawn() {
        eprintln!("could not launch Calculator: {e}");
    }
    tokio::time::sleep(Duration::from_secs(5)).await;

    println!("-- switching BACK to the (now renamed) page --");
    // Activate through the window we already hold, not a title lookup: the
    // title has deliberately changed, which is the whole point of the test.
    match field.window() {
        Ok(Some(w)) => {
            if let Err(e) = w.activate_window() {
                println!("   activate_window failed ({e}); falling back to a click");
                robust_click(&desktop, &field);
            }
        }
        _ => robust_click(&desktop, &field),
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    // A second nudge: some switch detection needs real input in the window.
    robust_click(&desktop, &field);
    tokio::time::sleep(Duration::from_secs(6)).await;

    let _ = recorder.stop().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    pump.abort();
    let _ = pump.await;

    let switches = collected.lock().map(|v| v.clone()).unwrap_or_default();

    println!("\n================ RESULTS ================");
    println!("{} ApplicationSwitch event(s):\n", switches.len());
    for (i, (title, pid, hwnd)) in switches.iter().enumerate() {
        let hwnd_desc = match hwnd {
            None => "ui_element ABSENT".to_string(),
            Some(Ok(h)) => format!("hwnd={h:#x} ({h})"),
            Some(Err(e)) => format!("hwnd FAILED: {e}"),
        };
        println!("  [{i}] pid={pid:<6} {hwnd_desc}");
        println!("      title={title:?}");
    }

    println!("\n--- verdict ---");
    let with_element = switches.iter().filter(|(_, _, h)| h.is_some()).count();
    let with_hwnd = switches
        .iter()
        .filter(|(_, _, h)| matches!(h, Some(Ok(v)) if *v != 0))
        .count();
    println!("  switches carrying a ui_element : {with_element}/{}", switches.len());
    println!("  switches yielding a usable hwnd: {with_hwnd}/{}", switches.len());

    // The decisive test: two visits to the same window under different titles.
    let browserish: Vec<&Switch> = switches
        .iter()
        .filter(|(t, _, _)| t.contains("Probe"))
        .collect();
    println!("\n  visits to the probe window: {}", browserish.len());
    for (t, pid, h) in &browserish {
        println!("    title={t:?} pid={pid} hwnd={h:?}");
    }
    if browserish.len() >= 2 {
        let titles_differ = browserish[0].0 != browserish[browserish.len() - 1].0;
        let pids_same = browserish[0].1 == browserish[browserish.len() - 1].1;
        // A zero handle is NOT an identity. `get_native_window_handle` returns
        // Ok(0) rather than an error for windows it cannot resolve, so
        // comparing two zeroes reports "stable" while carrying no information.
        // An earlier version of this verdict did exactly that and claimed HWND
        // was a valid key when every browser handle was 0.
        let hwnds_same = match (&browserish[0].2, &browserish[browserish.len() - 1].2) {
            (Some(Ok(a)), Some(Ok(b))) if *a != 0 && *b != 0 => Some(a == b),
            _ => None,
        };
        println!("\n  titles differ between visits : {titles_differ}");
        println!("  pid same between visits      : {pids_same}");
        println!(
            "  hwnd usable and same         : {}",
            match hwnds_same {
                Some(true) => "yes",
                Some(false) => "no -- differs",
                None => "n/a -- at least one handle was 0 or unavailable",
            }
        );
        if titles_differ && hwnds_same == Some(true) {
            println!("\n  HWND IS A VALID KEY: stable across the title change that broke replay.");
        } else if titles_differ && pids_same {
            println!("\n  hwnd NOT usable here; pid WAS stable across the title change.");
        }
    } else {
        println!("\n  INCONCLUSIVE: fewer than two switches back to the probe window.");
    }

    ExitCode::SUCCESS
}

// ----------------------------------------------------- replaycheck mode ----
//
// Does a LEGITIMATE replay still succeed with the target check in place?
//
// This is the over-caution risk, and it is the reason the ambiguity fix was
// abandoned: a check that rejects correct replays is worse than the bug it
// prevents. The existing suite does not cover it -- `replay_aborted`'s two tests
// halt on redaction and on not-found respectively, so neither exercises a
// successful resolve-then-act, and `ipc_pipeline` now fails before reaching
// replay at all.
//
// So: record a small playbook against the probe's own page, store it in a
// TEMPORARY database, replay it, and report every step outcome. Nothing touches
// the real store, and the target is a page this probe wrote.

async fn replaycheck_mode() -> ExitCode {
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    println!("== does a legitimate replay still succeed? ==\n");
    println!("Records a small playbook, stores it in a temp database, replays it.");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    let page = std::env::temp_dir().join("paradigm-text-capture-probe.html");
    if let Err(e) = std::fs::write(&page, PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in browser_order() {
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

    // ---- record -----------------------------------------------------------
    let session = match CaptureSession::start_session(
        "replaycheck",
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

    println!("-- recording: click FieldA, type, click Done --");
    robust_click(&desktop, &field);
    tokio::time::sleep(Duration::from_millis(600)).await;
    for ch in "replaycheck".chars() {
        let _ = field.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    if let Ok(done) = desktop
        .locator("role:Button|name:Done")
        .first(Some(Duration::from_secs(8)))
        .await
    {
        robust_click(&desktop, &done);
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("   captured {} action(s)", report.actions.len());
    if report.actions.is_empty() {
        eprintln!("   nothing captured -- cannot test replay. Inconclusive.");
        return ExitCode::FAILURE;
    }

    // ---- compile + store in a TEMP database -------------------------------
    let dir = match tempfile::TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("temp dir failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (db_path, key_path) = paradigm_lib::db::paths_in(dir.path());
    let mut conn = match paradigm_lib::db::open(&db_path, &key_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("temp db failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let playbook = compile(
        &report.actions,
        "Replay Check",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    if let Err(e) = store::store(&mut conn, &playbook) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }
    println!("   stored {} step(s)", playbook.steps.len());

    // ---- replay -----------------------------------------------------------
    println!("\n-- replaying --");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let run = match paradigm_lib::replay::replay(&mut conn, &desktop, &playbook.id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay failed to run: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================\n");
    println!("  run status: {}", run.status);
    println!("  steps: {} attempted of {}\n", run.steps_attempted(), run.steps_total);

    let mut wrong_target = 0usize;
    for o in &run.outcomes {
        println!(
            "  [{}] {:<9} {}",
            o.step_order,
            o.action_type,
            o.result.label()
        );
        println!("       selector {:?}", o.selector.as_deref().unwrap_or("-"));
        if o.result.label().contains("wrong element") {
            wrong_target += 1;
            println!("       {}", o.detail);
        }
    }

    println!("\n--- verdict ---");
    println!("  steps rejected as wrong-target : {wrong_target}");
    if wrong_target == 0 && !run.outcomes.iter().any(|o| o.result.is_failure()) {
        println!("\n  PASS: a legitimate replay still succeeds end to end. The target");
        println!("  check did not reject any correct step -- no over-caution here.");
    } else if wrong_target > 0 {
        println!("\n  OVER-CAUTION: the check rejected a step of a replay that was");
        println!("  recorded moments earlier against the same page. That is a false");
        println!("  positive and the rule needs revisiting.");
    } else {
        println!("\n  Replay had failures, but none from the target check. Look at the");
        println!("  outcomes above before drawing conclusions.");
    }

    ExitCode::SUCCESS
}

// ------------------------------------------------------ titledrift mode ----
//
// Measures the false-positive risk of the target check BEFORE it is built.
//
// The check compares a resolved element's name against the recorded name for
// EQUALITY, where matching is currently containment. That can only reject cases
// containment ACCEPTED -- i.e. where the resolved name properly contains the
// recorded one. Any other drift already fails to resolve today, so it is not a
// new failure.
//
// That leaves exactly one worry: a window that gains decoration and is still the
// same window. `*Untitled - Notepad` is the canonical case. This reproduces it
// with a page that prepends `*` on input, because Notepad itself hands new
// launches to an existing instance and cannot be targeted reliably.

const DRIFT_PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>DriftProbe</title></head>
<body style="font-family:sans-serif;padding:2rem">
<h2>Title drift probe</h2>
<p>Typing here prepends a "*" to the window title, the way an editor marks
unsaved changes.</p>
<input id="f" aria-label="DriftField" style="font-size:1.2rem;width:24rem"
       oninput="document.title = this.value ? '*DriftProbe' : 'DriftProbe'">
</body></html>
"#;

async fn titledrift_mode() -> ExitCode {
    println!("== title drift: how risky is exact name matching? ==\n");

    let page = std::env::temp_dir().join("paradigm-drift-probe.html");
    if let Err(e) = std::fs::write(&page, DRIFT_PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in browser_order() {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", &url])
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
        .locator("role:Edit|name:DriftField")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("could not find the drift field: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The window name as capture would record it.
    // Read the window through a locator, not `field.window()`: the latter
    // returns an element whose name is empty for browser windows, which made an
    // earlier version of this test report "no drift" when it had measured
    // nothing at all.
    async fn read_window(desktop: &Desktop) -> String {
        for sel in ["role:Window|name:DriftProbe", "role:Window|name:Drift"] {
            if let Ok(w) = desktop
                .locator(sel)
                .first(Some(Duration::from_secs(3)))
                .await
            {
                if let Some(n) = w.name() {
                    if !n.is_empty() {
                        return n;
                    }
                }
            }
        }
        String::new()
    }

    let window_before = read_window(&desktop).await;
    println!("  window name BEFORE typing : {window_before:?}");
    if window_before.is_empty() {
        eprintln!("  could not read the window name -- cannot measure drift. Aborting.");
        return ExitCode::FAILURE;
    }

    robust_click(&desktop, &field);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = field.type_text("x", false);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let window_after = read_window(&desktop).await;
    println!("  window name AFTER typing  : {window_after:?}");
    if window_after.is_empty() {
        eprintln!("  could not read the window name after typing -- inconclusive.");
        return ExitCode::FAILURE;
    }

    println!("\n--- what each rule would do ---");
    let contains = window_after.contains(&window_before);
    let exact = window_after == window_before;
    let star_ok = window_after == format!("*{window_before}")
        || window_after
            .strip_prefix('*')
            .map(|s| s == window_before)
            .unwrap_or(false);

    println!("  today (contains)        : {}", if contains { "resolves" } else { "does NOT resolve" });
    println!("  strict exact            : {}", if exact { "accepts" } else { "REJECTS -- false positive" });
    println!("  exact, allowing a '*'   : {}", if exact || star_ok { "accepts" } else { "REJECTS" });

    println!("\n--- verdict ---");
    if !contains {
        println!("  The drifted title does not contain the recorded one, so this step");
        println!("  ALREADY fails to resolve today. Exact matching makes it no worse --");
        println!("  the false-positive worry does not apply to this kind of drift.");
    } else if !exact && star_ok {
        println!("  Real false positive for strict exact matching, and it is exactly the");
        println!("  '*' decoration case. Allowing a single leading '*' covers it while");
        println!("  still rejecting the Paradigm-style collision.");
    } else if !exact {
        println!("  Real false positive, and NOT covered by a '*' rule -- the drift is");
        println!("  something else. Strict exact matching would need reconsidering.");
    } else {
        println!("  No drift observed; the title did not change.");
    }

    ExitCode::SUCCESS
}

// ---------------------------------------------------------- verify mode ----
//
// Prototype of post-execution verification for replay, tested against the four
// bugs found this project, using their REAL recorded values.
//
// Motivation: four independently-found defects share one shape -- replay reports
// success while doing the wrong thing -- and nothing ever checks whether a
// replayed action produced what the recording expected.
//
// Three candidate checks are implemented as pure predicates and run against the
// actual data from each bug. Pure functions, because the pre-fix behaviour is
// already captured in the known-issues docs and re-creating it live would add
// nothing but risk.

/// PRE-EXECUTION. Does the element the selector resolved to actually carry the
/// name that was recorded? Cheap, and runs before anything is acted on.
fn check_target(recorded_name: &str, resolved_name: &str) -> bool {
    recorded_name == resolved_name
}

/// DESIGN A, post-execution. Did the action do what the payload said?
/// For append-style typing: after == before + payload.
///
/// Note what this compares: replay's behaviour against the recording's
/// instruction. It cannot see whether the instruction itself was right.
fn check_action_a(before: &str, payload: &str, after: &str) -> bool {
    let expected = format!("{before}{payload}");
    norm_nl(&expected) == norm_nl(after)
}

/// DESIGN B, post-execution. Does the resulting state match the state the
/// RECORDING ended in? Requires capture to store that state, which it does not
/// today -- this is the prototype's proposal, not current behaviour.
fn check_outcome_b(recorded_end_state: &str, actual_after: &str) -> bool {
    norm_nl(recorded_end_state) == norm_nl(actual_after)
}

fn norm_nl(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn yn(b: bool) -> &'static str {
    if b {
        "passes"
    } else {
        "FLAGS IT"
    }
}

async fn verify_mode() -> ExitCode {
    println!("== post-execution verification: prototype against real bugs ==\n");
    println!("Each case uses the values actually recorded when the bug was live.\n");

    // ---- 1. multi-line duplication ---------------------------------------
    //
    // Pre-fix payloads (docs/known-issues/multiline-document-capture-duplicates.md):
    // step 1 "alpha line", step 2 "alpha line\nbeta line". Replay types both at
    // the caret, producing 30 characters for a 20 character field.
    println!("---------------- 1. multi-line duplication (capture-time bug) ----------------");
    {
        let s1_before = "";
        let s1_payload = "alpha line";
        let s1_after = "alpha line";

        let s2_before = "alpha line";
        let s2_payload = "alpha line\nbeta line"; // the cumulative payload
        let s2_after = "alpha linealpha line\nbeta line"; // what replay produces

        // What the recording's own field actually held at capture time.
        let recorded_end_state = "alpha line\nbeta line";

        println!("  step 1  A: {}", yn(check_action_a(s1_before, s1_payload, s1_after)));
        println!("  step 2  A: {}", yn(check_action_a(s2_before, s2_payload, s2_after)));
        println!(
            "  step 2  B: {}   (recorded end state {:?} vs actual {:?})",
            yn(check_outcome_b(recorded_end_state, s2_after)),
            recorded_end_state,
            s2_after
        );
        println!("\n  Design A passes -- replay appended exactly what the payload said.");
        println!("  The payload itself was wrong, which A cannot see.");
        println!("  Design B catches it: the field ends up different from the recording's.");
    }

    // ---- 2. window-switch misattribution ---------------------------------
    //
    // Recorded order was click FieldA, navigate Calculator, type FieldA. Replay
    // switches to Calculator and then types into the browser.
    println!("\n---------------- 2. window-switch misattribution (capture-time bug) ----------------");
    {
        let payload = "switchtest0123456789";
        let before = "";
        let after = "switchtest0123456789";

        println!("  type step  A: {}", yn(check_action_a(before, payload, after)));
        println!("  type step  B (content only): {}", yn(check_outcome_b(after, after)));

        // The only signal that distinguishes right from wrong here is context,
        // not content: which application was in front when the step ran.
        let foreground_at_capture = "msedge.exe";
        let foreground_at_replay = "Calculator";
        let context_ok = foreground_at_capture == foreground_at_replay;
        println!(
            "  type step  B (with foreground context): {}   ({foreground_at_capture:?} at capture vs {foreground_at_replay:?} at replay)",
            yn(context_ok)
        );
        println!("\n  Content-only checks BOTH pass -- the right text reached the right field.");
        println!("  Only a recorded-context check sees that it happened in the wrong place.");
    }

    // ---- 3 & 4. selector collision ---------------------------------------
    //
    // Real stored selector role:Window|name:Paradigm resolving onto a browser.
    println!("\n---------------- 3. substring selector collision (replay-time bug) ----------------");
    {
        let recorded = "Paradigm";
        let resolved = "Paradigm Text Capture Probe and 63 more pages - Personal - Microsoft Edge";
        println!("  pre-execution target check: {}", yn(check_target(recorded, resolved)));
        println!("      recorded {recorded:?}");
        println!("      resolved {resolved:?}");
        println!("\n  Caught BEFORE acting, by comparing names for equality rather than");
        println!("  containment. The cheapest of the three checks, and the only one that");
        println!("  prevents the wrong action instead of reporting it afterwards.");
    }

    // ---- 4. ambiguity ------------------------------------------------------
    println!("\n---------------- 4. selector ambiguity (replay-time bug) ----------------");
    {
        let recorded = "Untitled - Notepad";
        let resolved = "Untitled - Notepad"; // both candidate windows carry this name
        println!("  pre-execution target check: {}", yn(check_target(recorded, resolved)));
        println!("\n  NOT caught. Two windows genuinely share the name, so the resolved");
        println!("  element's name equals the recorded one and every content check");
        println!("  afterwards succeeds -- against the wrong window. Detecting this needs");
        println!("  a candidate COUNT, which the library does not expose (see");
        println!("  replay-window-selector-ambiguity.md).");
    }

    // ---- summary -----------------------------------------------------------
    println!("\n================ COVERAGE ================\n");
    println!("  bug                              A(action)  B(outcome)  B+context  target");
    println!("  {}", "-".repeat(76));
    println!("  multi-line duplication           no         YES         YES        no");
    println!("  window-switch misattribution     no         no          YES        no");
    println!("  substring selector collision     no         no          no         YES");
    println!("  selector ambiguity               no         no          no         no");
    println!("\n  1 of 4 by pre-execution target check (cheapest, prevents the action)");
    println!("  2 of 4 by outcome + recorded context (needs capture to store state)");
    println!("  3 of 4 combined. Ambiguity remains uncovered by any of these.");

    ExitCode::SUCCESS
}

// ------------------------------------------------------- selectors mode ----
//
// Is substring name matching a real production risk, or a curiosity?
//
// Terminator matches names with `contains_name`, not exact equality
// (`platforms/windows/engine.rs:1204`, where the upstream comment says the
// choice is "undetermined"). So `name:To` matches any element whose name
// contains "to" -- which is how a Gmail selector resolved onto Claude Code's
// prompt input and a Windows taskbar button.
//
// The question that decides whether this matters is not "can substring matching
// collide" -- it obviously can -- but "does Paradigm's capture actually produce
// names short or generic enough to collide in real use?"
//
// So this reads the REAL selectors out of the on-device database and resolves
// each against the live desktop, reporting when a stored selector lands on an
// element whose name is not the one that was recorded. Real selectors, real
// multi-application desktop. Read-only: nothing is clicked or typed.
//
//     cargo run --example text_capture_probe -- selectors <app_data_dir>

async fn selectors_mode() -> ExitCode {
    let dir = match std::env::args().nth(2).map(std::path::PathBuf::from) {
        Some(d) => d,
        None => {
            eprintln!("usage: cargo run --example text_capture_probe -- selectors <app_data_dir>");
            eprintln!("  no default -- this reads a real database.");
            return ExitCode::FAILURE;
        }
    };

    println!("== selector precision: do real captured names collide? ==\n");

    let (db_path, key_path) = paradigm_lib::db::paths_in(&dir);
    let conn = match paradigm_lib::db::open(&db_path, &key_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not open {}: {e}", db_path.display());
            return ExitCode::FAILURE;
        }
    };

    // Every stored step's recorded target.
    let mut stmt = match conn.prepare(
        "SELECT action_type, action_payload_json FROM playbook_steps ORDER BY step_order",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("query failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rows: Vec<(String, String)> = match stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(|m| m.collect::<Result<Vec<_>, _>>())
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("read failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if rows.is_empty() {
        println!("  no stored steps in this database -- nothing to survey.");
        return ExitCode::SUCCESS;
    }

    // ---- 2. what does capture actually produce? ---------------------------
    println!("================ WHAT CAPTURE ACTUALLY RECORDS ================\n");
    println!("  {:<9} {:<44} {:>5}  selector", "action", "target name", "len");
    println!("  {}", "-".repeat(100));

    struct Recorded {
        name: String,
        selector: String,
    }
    let mut recorded = Vec::new();

    for (action, raw) in &rows {
        let v: serde_json::Value = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
        let name = v["target"]["name"].as_str().unwrap_or("").to_string();
        let selector = v["target"]["selector"].as_str().unwrap_or("").to_string();
        println!(
            "  {action:<9} {:<44} {:>5}  {selector}",
            if name.is_empty() { "<none>" } else { &name },
            name.chars().count()
        );
        if !selector.is_empty() {
            recorded.push(Recorded { name, selector });
        }
    }

    let short: Vec<&Recorded> = recorded
        .iter()
        .filter(|r| !r.name.is_empty() && r.name.chars().count() <= 6)
        .collect();
    println!(
        "\n  {} step(s) with a selector; {} have a target name of 6 characters or fewer",
        recorded.len(),
        short.len()
    );

    // ---- 3. do those selectors collide on a real desktop? -----------------
    println!("\n================ DO THEY COLLIDE ON THIS DESKTOP? ================\n");
    println!("  Resolving each RECORDED selector against the live desktop and");
    println!("  comparing what it lands on against what was captured.\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut collisions = 0usize;
    let mut exact = 0usize;
    let mut missing = 0usize;

    for r in &recorded {
        match desktop
            .locator(r.selector.as_str())
            .first(Some(Duration::from_secs(2)))
            .await
        {
            Ok(el) => {
                let got = el.name().unwrap_or_default();
                if got == r.name {
                    exact += 1;
                    println!("  OK        {:<44} -> exact match", r.selector);
                } else {
                    collisions += 1;
                    println!("  COLLISION {:<44}", r.selector);
                    println!("            recorded name : {:?}", r.name);
                    println!("            resolved to   : {:?} (role={:?})", got, el.role());
                }
            }
            Err(_) => {
                missing += 1;
                println!("  absent    {:<44} -> not on screen now", r.selector);
            }
        }
    }

    println!("\n================ VERDICT ================\n");
    println!("  selectors tested       : {}", recorded.len());
    println!("  resolved exactly       : {exact}");
    println!("  resolved to SOMETHING ELSE : {collisions}");
    println!("  not present right now  : {missing}");

    if collisions > 0 {
        println!(
            "\n  Real captured selectors resolve onto the wrong element on a real\n  \
             desktop. Substring matching is a production risk, not a curiosity."
        );
    } else if exact > 0 {
        println!(
            "\n  Every selector that resolved landed on its recorded target. On this\n  \
             evidence substring matching is a real mechanism that does NOT manifest,\n  \
             because capture records full labels rather than fragments."
        );
    } else {
        println!("\n  Nothing resolved -- inconclusive. Re-run with the captured apps open.");
    }

    ExitCode::SUCCESS
}

// ---------------------------------------------------- gmailcleanup mode ----
//
// Two jobs, and the first sets up a decisive test for the second.
//
//   1. Close leftover EMPTY drafts from earlier probe runs. Each is handled
//      independently: read its body, confirm it is blank, then close with
//      Escape. A draft with content, or one whose body cannot be read, is left
//      strictly alone and aborts the sweep.
//
//   2. Settle whether the three selectors that resolved in ~16ms during the
//      first gmailpicker run are part of the recipient picker or false matches
//      on unrelated Gmail UI. Resolving them BEFORE and AFTER every compose
//      window is closed answers it outright: anything still resolving with no
//      compose open cannot be part of the picker.
//
// Read-only apart from closing confirmed-empty drafts. Nothing is typed, and
// nothing is clicked.

/// The selectors under suspicion, plus the recorded one for contrast.
const SUSPECT_SELECTORS: &[&str] = &[
    "role:Group|name:To - Select contacts",
    "role:ComboBox|name:To recipients",
    "role:Edit|name:To",
    "role:Button|name:To",
];

async fn describe_suspects(desktop: &Desktop, phase: &str) -> Vec<bool> {
    println!("\n  -- {phase} --");
    let mut resolved = Vec::new();
    for sel in SUSPECT_SELECTORS {
        match desktop
            .locator(*sel)
            .first(Some(Duration::from_secs(2)))
            .await
        {
            Ok(el) => {
                let bounds = match el.bounds() {
                    Ok((x, y, w, h)) => format!("x={x:.0} y={y:.0} {w:.0}x{h:.0}"),
                    Err(e) => format!("<bounds failed: {e}>"),
                };
                println!("    {sel}");
                println!("        RESOLVED role={:?} name={:?}", el.role(), el.name());
                println!("        bounds  {bounds}");
                resolved.push(true);
            }
            Err(_) => {
                println!("    {sel}");
                println!("        not found");
                resolved.push(false);
            }
        }
    }
    resolved
}

async fn gmailcleanup_mode() -> ExitCode {
    println!("== Gmail cleanup + selector identification ==\n");
    println!("Closes leftover EMPTY drafts only, after confirming each is blank.");
    println!("Then identifies the ~16ms selectors. Nothing typed, nothing clicked.\n");

    for browser in browser_order() {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "https://mail.google.com/"])
            .spawn()
        {
            let _ = c.wait();
            break;
        }
    }
    println!("waiting 20s for Gmail to load...");
    tokio::time::sleep(Duration::from_secs(20)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- state BEFORE any cleanup ----------------------------------------
    println!("\n================ 2a. SELECTORS, COMPOSE OPEN (IF ANY) ================");
    let before = describe_suspects(&desktop, "before closing anything").await;

    // ---- 1. close each confirmed-empty draft ------------------------------
    println!("\n================ 1. LEFTOVER DRAFT CLEANUP ================");
    let body_selectors = [
        "role:Edit|name:Message Body",
        "role:Document|name:Message Body",
        "role:Edit|name:Message body",
    ];

    let mut closed = 0usize;
    for round in 0..6 {
        let mut body = None;
        for sel in body_selectors {
            if let Ok(b) = desktop
                .locator(sel)
                .first(Some(Duration::from_secs(2)))
                .await
            {
                body = Some(b);
                break;
            }
        }

        let Some(body) = body else {
            println!(
                "  round {round}: no compose body found -- {} draft(s) closed in total",
                closed
            );
            break;
        };

        match body.text(0) {
            Ok(t) if t.trim().is_empty() => {
                println!("  round {round}: body confirmed EMPTY (0 chars) -- closing with Escape");
                let _ = body.press_key("{Escape}");
                closed += 1;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            Ok(t) => {
                println!(
                    "  round {round}: STOPPING -- this draft contains {} character(s).\n\
                     \x20            It is not one of ours. Left completely untouched.",
                    t.chars().count()
                );
                break;
            }
            Err(e) => {
                println!(
                    "  round {round}: STOPPING -- could not read the body to confirm it is\n\
                     \x20            empty ({e}). Left untouched."
                );
                break;
            }
        }
    }

    // ---- state AFTER cleanup ---------------------------------------------
    println!("\n================ 2b. SELECTORS, NO COMPOSE OPEN ================");
    let after = describe_suspects(&desktop, "after closing all empty drafts").await;

    // ---- verdicts ---------------------------------------------------------
    println!("\n================ VERDICT ================\n");
    println!("  drafts closed: {closed}\n");

    let compose_gone = !after[0]; // the recorded picker selector
    println!(
        "  compose window genuinely closed (recorded picker selector gone): {}",
        if compose_gone { "yes" } else { "NO" }
    );

    if !compose_gone {
        println!(
            "\n  A compose window is still open, so this cannot distinguish the\n  \
             suspects. Reporting that rather than a conclusion."
        );
        return ExitCode::SUCCESS;
    }

    println!("\n  selector                                     open   closed   verdict");
    println!("  {}", "-".repeat(76));
    for (i, sel) in SUSPECT_SELECTORS.iter().enumerate() {
        let verdict = match (before[i], after[i]) {
            (_, true) => "FALSE MATCH -- resolves with no compose open",
            (true, false) => "genuinely part of the compose UI",
            (false, false) => "absent in both -- inconclusive",
        };
        println!(
            "  {sel:<44} {:<6} {:<8} {verdict}",
            if before[i] { "yes" } else { "no" },
            if after[i] { "yes" } else { "no" }
        );
    }

    ExitCode::SUCCESS
}

// ----------------------------------------------------- gmailpicker mode ----
//
// Answers one question from docs/known-issues/dynamic-contact-picker-replay-fails.md:
// does `role:group|name:"To - Select contacts"` EXIST when replay looks for it,
// or was the widget genuinely not rendered?
//
// Run 7b99fae2 timed out after 8s with "element not found". The untested theory
// is that the picker only appears once compose is open and settled, making this
// a timing/sequencing problem rather than an accessibility defect.
//
// ## Safety
//
// This drives a REAL Gmail account. It is deliberately constrained:
//
//   * it clicks Compose, once, and nothing else;
//   * it NEVER types, and never clicks anything whose name suggests Send,
//     Discard, Delete, or Reply;
//   * everything after the Compose click is read-only tree inspection.
//
// Side effect: one empty draft. It aborts rather than guessing if Gmail does not
// look loaded, so a login page cannot be clicked at blindly.

/// Selectors to poll. The first is the one the failing recording actually
/// stored; the rest are plausible alternatives, so "the selector was wrong" can
/// be distinguished from "the element was not there".
const PICKER_SELECTORS: &[&str] = &[
    "role:Group|name:To - Select contacts",
    "role:Group|name:To recipients",
    "role:Edit|name:To recipients",
    "role:ComboBox|name:To recipients",
    "role:Edit|name:To",
    "role:Button|name:To",
];

async fn gmailpicker_mode() -> ExitCode {
    println!("== Gmail recipient picker: existence and timing ==\n");
    println!("Clicks Compose ONCE, then only reads the accessibility tree.");
    println!("Never types. Never clicks Send/Discard/Delete.");
    println!("Side effect: one empty draft.\n");

    for browser in browser_order() {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "https://mail.google.com/"])
            .spawn()
        {
            let _ = c.wait();
            break;
        }
    }
    println!("waiting 20s for Gmail to load...");
    tokio::time::sleep(Duration::from_secs(20)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // GUARD: only proceed if this looks like a loaded mailbox. A login or
    // consent page must abort rather than be clicked at.
    let compose = match desktop
        .locator("role:Button|name:Compose")
        .first(Some(Duration::from_secs(15)))
        .await
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "no Compose button found ({e}).\n\
                 Gmail may not be loaded or signed in. Aborting rather than \
                 clicking blindly at an unknown page."
            );
            return ExitCode::FAILURE;
        }
    };
    // ---- PRE-FLIGHT: guarantee a genuinely cold start ---------------------
    //
    // The previous run measured 19ms instead of 6743ms because a draft left
    // open by the run before it meant Gmail restored the compose window, so the
    // widget was already rendered. Reloading the page is not enough -- Gmail
    // restores compose state.
    //
    // Anything found open here is closed with Escape, and ONLY after its body
    // is read and confirmed empty. A draft with content, or one whose content
    // cannot be read, aborts the probe rather than being touched.
    println!("-- pre-flight: looking for a leftover compose window --");

    let body_selectors = [
        "role:Edit|name:Message Body",
        "role:Document|name:Message Body",
        "role:Edit|name:Message body",
    ];

    for round in 0..3 {
        let still_open = desktop
            .locator(PICKER_SELECTORS[0])
            .first(Some(Duration::from_secs(2)))
            .await
            .is_ok();
        if !still_open {
            println!("  no compose window open{}", if round == 0 { "" } else { " (closed)" });
            break;
        }

        // Confirm it is empty before touching it.
        let mut body = None;
        for sel in body_selectors {
            if let Ok(b) = desktop
                .locator(sel)
                .first(Some(Duration::from_secs(2)))
                .await
            {
                body = Some(b);
                break;
            }
        }

        let Some(body) = body else {
            eprintln!(
                "  REFUSING TO PROCEED: a compose window appears open but its body\n  \
                 could not be read, so it cannot be confirmed empty. Not touching it."
            );
            return ExitCode::FAILURE;
        };

        match body.text(0) {
            Ok(t) if t.trim().is_empty() => {
                println!("  compose window open, body confirmed EMPTY -- closing with Escape");
                let _ = body.press_key("{Escape}");
            }
            Ok(t) => {
                eprintln!(
                    "  REFUSING TO PROCEED: the open draft contains {} characters.\n  \
                     That is not the empty draft this probe created. Leaving it alone.",
                    t.chars().count()
                );
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("  REFUSING TO PROCEED: could not read the draft body to confirm it is empty: {e}");
                return ExitCode::FAILURE;
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // Hard gate: the widget must be absent before this counts as cold.
    let cold = desktop
        .locator(PICKER_SELECTORS[0])
        .first(Some(Duration::from_secs(3)))
        .await
        .is_err();
    println!(
        "  cold-start precondition (picker ABSENT before clicking Compose): {}",
        if cold { "MET" } else { "NOT MET" }
    );
    if !cold {
        eprintln!(
            "\n  Cannot produce a cold start -- the picker is still present.\n  \
             Any timing measured now would repeat the warm-widget mistake.\n  \
             Reporting that rather than publishing a meaningless number."
        );
        return ExitCode::FAILURE;
    }

    println!("\nfound Compose; clicking it once\n");

    let opened_at = std::time::Instant::now();
    robust_click(&desktop, &compose);

    // Poll each selector on a schedule. `first()` with a short timeout is used
    // deliberately: a long timeout would hide WHEN the element appeared.
    let schedule_ms = [0u64, 500, 1000, 2000, 3000, 5000, 8000, 12000, 20000, 30000];
    let mut first_seen: Vec<Option<u64>> = vec![None; PICKER_SELECTORS.len()];

    println!("  elapsed   selector                                    found");
    println!("  {}", "-".repeat(78));

    for target in schedule_ms {
        let now = opened_at.elapsed().as_millis() as u64;
        if now < target {
            tokio::time::sleep(Duration::from_millis(target - now)).await;
        }
        let elapsed = opened_at.elapsed().as_millis() as u64;

        for (i, sel) in PICKER_SELECTORS.iter().enumerate() {
            let hit = desktop
                .locator(*sel)
                .first(Some(Duration::from_millis(250)))
                .await
                .is_ok();
            if hit && first_seen[i].is_none() {
                first_seen[i] = Some(elapsed);
            }
            if hit {
                println!("  {elapsed:>6}ms  {sel:<44} YES");
            }
        }
    }

    // If nothing matched, dump what IS present so a wrong selector can be told
    // apart from a missing element.
    let any_found = first_seen.iter().any(|f| f.is_some());
    if !any_found {
        println!("\n  none of the candidate selectors matched. What IS present:");
        for role in ["Group", "Edit", "ComboBox", "Button", "List"] {
            if let Ok(el) = desktop
                .locator(format!("role:{role}").as_str())
                .first(Some(Duration::from_secs(2)))
                .await
            {
                println!("    role:{role:<10} -> name={:?}", el.name());
            }
        }
    }

    println!("\n================ VERDICT ================\n");
    for (i, sel) in PICKER_SELECTORS.iter().enumerate() {
        match first_seen[i] {
            Some(ms) => println!("  FOUND at {ms:>6}ms  {sel}"),
            None => println!("  never found     {sel}"),
        }
    }

    let recorded = first_seen[0];
    println!("\n--- answering the doc's question ---");
    match recorded {
        Some(ms) if ms <= 8000 => println!(
            "  The recorded selector DOES exist, first seen {ms}ms after Compose.\n  \
             That is inside the 8s LOCATE_TIMEOUT, so the original failure is NOT\n  \
             explained by the element being absent -- something else went wrong."
        ),
        Some(ms) => println!(
            "  The recorded selector exists but only after {ms}ms, which is BEYOND\n  \
             the 8s LOCATE_TIMEOUT. A timing problem: replay gave up too early."
        ),
        None => println!(
            "  The recorded selector NEVER appeared, even after 30s.\n  \
             So the failure is not replay being impatient. Either the selector is\n  \
             wrong for this UI, or the picker needs a precondition beyond opening\n  \
             compose that replay never reproduced."
        ),
    }

    // ---- how the real budgets behave against this widget ------------------
    //
    // The point of the fix: 8s was barely wider than the widget's own latency.
    // These are the two budgets replay now actually uses, measured end to end
    // including the time the locator itself spends waiting.
    println!("\n================ BUDGETS, MEASURED ================\n");

    for (label, budget) in [("old 8s (window budget)", 8u64), ("new 15s (element budget)", 15)] {
        let t = std::time::Instant::now();
        let found = desktop
            .locator(PICKER_SELECTORS[0])
            .first(Some(Duration::from_secs(budget)))
            .await
            .is_ok();
        println!(
            "  {label:<26} -> {:<9} in {:?}",
            if found { "FOUND" } else { "NOT FOUND" },
            t.elapsed()
        );
    }

    // A selector that cannot exist, to bound the cost of an honest failure.
    let t = std::time::Instant::now();
    let bogus = desktop
        .locator("role:Group|name:ThisWidgetDoesNotExistAnywhere")
        .first(Some(Duration::from_secs(15)))
        .await
        .is_ok();
    println!(
        "  {:<26} -> {:<9} in {:?}   <-- cost of an honest failure",
        "nonexistent selector",
        if bogus { "FOUND?!" } else { "NOT FOUND" },
        t.elapsed()
    );

    println!("\n  (an empty draft was created; nothing was typed or sent)");
    ExitCode::SUCCESS
}

// --------------------------------------------------------- widgets mode ----
//
// Tests the MECHANISMS that Sheets and Gmail were speculated to share, on a
// controlled page where one thing varies at a time. The real sites can only
// show correlation; this can show causation.
//
// Two candidate mechanisms, from the two known-issue docs:
//
//   A. ARIA roles map inconsistently to UIA roles. Gmail's recipient field is
//      `role:group`; a Sheets cell is `combobox`. If ARIA roles surface in UIA
//      in surprising ways, selectors built from them are fragile by
//      construction.
//
//   B. Re-rendering replaces DOM nodes rather than mutating them, invalidating
//      element handles between capture and replay. This would explain Gmail's
//      "element not found" on a selector that captured fine.
//
// Neither has been tested. Both are testable here.

const WIDGET_PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Widget Mechanism Probe</title></head>
<body style="font-family:sans-serif;padding:1.5rem">
<h2>Widget mechanism probe</h2>

<h3>A. ARIA roles, as UIA sees them</h3>
<div role="group" aria-label="AriaGroup" style="border:1px solid #999;padding:.4rem">
  <input aria-label="InsideGroup" style="width:16rem">
</div><br>
<div role="combobox" aria-label="AriaCombobox" contenteditable="true"
     style="border:1px solid #999;padding:.4rem;width:16rem">cell text</div><br>
<div role="grid" aria-label="AriaGrid" style="border:1px solid #999;padding:.4rem">
  <div role="row"><div role="gridcell" aria-label="AriaGridCell"
       contenteditable="true" style="border:1px solid #ccc;width:10rem">A1</div></div>
</div><br>
<div role="textbox" aria-label="AriaTextbox" contenteditable="true"
     style="border:1px solid #999;padding:.4rem;width:16rem">textbox text</div><br>
<div role="listbox" aria-label="AriaListbox" style="border:1px solid #999;padding:.4rem">
  <div role="option" aria-label="AriaOption">an option</div>
</div>

<h3>B. Identity across a re-render</h3>
<div id="host"><input id="target" aria-label="StableTarget" value="original" style="width:16rem"></div>
<button aria-label="MutateBtn" onclick="document.getElementById('target').value='mutated'">Mutate in place</button>
<button aria-label="ReplaceBtn" onclick="
  var h=document.getElementById('host');
  h.innerHTML='<input id=\'target\' aria-label=\'StableTarget\' value=\'replaced\' style=\'width:16rem\'>';
">Replace the node</button>
</body></html>
"#;

async fn widgets_mode() -> ExitCode {
    println!("== widget mechanism probe ==\n");
    println!("Controlled test of the two mechanisms Sheets and Gmail were");
    println!("speculated to share. Local page only -- no accounts, no real data.\n");

    let page = std::env::temp_dir().join("paradigm-widget-probe.html");
    if let Err(e) = std::fs::write(&page, WIDGET_PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in browser_order() {
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

    // ---- A. what UIA role does each ARIA role surface as? -----------------
    println!("\n================ A. ARIA -> UIA ROLE MAPPING ================\n");
    println!("  Looking each element up by NAME under several candidate roles,");
    println!("  so the ARIA role it was declared with can be compared against");
    println!("  the role UIA actually reports.\n");

    for (aria, name) in [
        ("group", "AriaGroup"),
        ("combobox", "AriaCombobox"),
        ("grid", "AriaGrid"),
        ("gridcell", "AriaGridCell"),
        ("textbox", "AriaTextbox"),
        ("listbox", "AriaListbox"),
        ("option", "AriaOption"),
    ] {
        println!("  declared aria role={aria:?}, aria-label={name:?}");
        let mut found = false;
        for probe_role in [
            "Group", "ComboBox", "Edit", "Document", "Text", "DataGrid", "DataItem",
            "List", "ListItem", "Custom", "Pane",
        ] {
            let selector = format!("role:{probe_role}|name:{name}");
            if let Ok(el) = desktop
                .locator(selector.as_str())
                .first(Some(Duration::from_millis(700)))
                .await
            {
                println!(
                    "      MATCHED as role:{probe_role:<10} (UIA reports role={:?})",
                    el.role()
                );
                found = true;
            }
        }
        if !found {
            println!("      no candidate role matched -- UIA may not expose it at all");
        }
    }

    // ---- B. does identity survive a re-render? ----------------------------
    println!("\n================ B. IDENTITY ACROSS A RE-RENDER ================\n");

    let target_sel = "role:Edit|name:StableTarget";
    let before = desktop
        .locator(target_sel)
        .first(Some(Duration::from_secs(5)))
        .await;
    let (before_id, before_handle) = match &before {
        Ok(el) => {
            println!("  before      id={:?} text={:?}", el.id(), el.text(0).ok());
            (el.id(), Some(el.clone()))
        }
        Err(e) => {
            eprintln!("  could not find the target at all: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Mutating in place: the DOM node survives.
    println!("\n  -- clicking 'Mutate in place' (node survives) --");
    if let Ok(b) = desktop
        .locator("role:Button|name:MutateBtn")
        .first(Some(Duration::from_secs(5)))
        .await
    {
        robust_click(&desktop, &b);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let after_mutate = desktop
        .locator(target_sel)
        .first(Some(Duration::from_secs(5)))
        .await;
    match &after_mutate {
        Ok(el) => println!(
            "  after mutate id={:?} text={:?}   (id same as before: {})",
            el.id(),
            el.text(0).ok(),
            el.id() == before_id
        ),
        Err(e) => println!("  after mutate NOT FOUND: {e}"),
    }
    if let Some(h) = &before_handle {
        println!(
            "  the ORIGINAL handle still reads: {:?}",
            h.text(0).map_err(|e| e.to_string())
        );
    }

    // Replacing the node: the DOM node is destroyed and recreated.
    println!("\n  -- clicking 'Replace the node' (node destroyed + recreated) --");
    if let Ok(b) = desktop
        .locator("role:Button|name:ReplaceBtn")
        .first(Some(Duration::from_secs(5)))
        .await
    {
        robust_click(&desktop, &b);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let after_replace = desktop
        .locator(target_sel)
        .first(Some(Duration::from_secs(5)))
        .await;
    let after_replace_id = match &after_replace {
        Ok(el) => {
            println!(
                "  after replace id={:?} text={:?}   (id same as before: {})",
                el.id(),
                el.text(0).ok(),
                el.id() == before_id
            );
            el.id()
        }
        Err(e) => {
            println!("  after replace NOT FOUND: {e}");
            None
        }
    };
    if let Some(h) = &before_handle {
        match h.text(0) {
            Ok(t) => println!("  the ORIGINAL handle STILL reads: {t:?} (handle survived)"),
            Err(e) => println!("  the ORIGINAL handle is now DEAD: {e}"),
        }
    }

    println!("\n--- what B decides ---");
    match (before_id.as_deref(), after_replace_id.as_deref()) {
        (Some(a), Some(b)) if a == b => println!(
            "  Element id is STABLE across a DOM replacement ({a}).\n  \
             Handle invalidation is NOT the mechanism -- re-resolution finds the\n  \
             same identity even after the node is destroyed."
        ),
        (Some(a), Some(b)) => println!(
            "  Element id CHANGED across a DOM replacement: {a} -> {b}.\n  \
             A selector re-resolved after a re-render addresses a DIFFERENT\n  \
             element identity, which is a real candidate mechanism."
        ),
        (_, None) => println!(
            "  The element could not be re-resolved after replacement at all --\n  \
             the strongest form of the mechanism."
        ),
        _ => println!("  inconclusive: ids unavailable"),
    }

    ExitCode::SUCCESS
}

// -------------------------------------------------------- procname mode ----
//
// Layer 1 verification for the process-name plumbing: does a real driven
// session actually produce CapturedActions carrying a process name?
//
// Checked empirically rather than by reading the code, because the whole reason
// this field exists is that the obvious assumption -- "identifiers already
// carries it" -- turned out to be false: `admit` keeps only the first non-empty
// identifier as `source_app` and drops the rest.

async fn procname_mode() -> ExitCode {
    println!("== process-name plumbing probe (capture layer) ==\n");
    println!("Drives a real session and reports the process_name on each");
    println!("captured action, alongside source_app for comparison.\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    let page = std::env::temp_dir().join("paradigm-text-capture-probe.html");
    if let Err(e) = std::fs::write(&page, PAGE) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in browser_order() {
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

    let session = match CaptureSession::start_session(
        "procname-probe",
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

    // A click, some typing, and an app switch -- one of each action kind.
    robust_click(&desktop, &field);
    tokio::time::sleep(Duration::from_millis(600)).await;
    for ch in "procname-test".chars() {
        let _ = field.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    println!("-- switching to Calculator to produce a navigate action --");
    let _ = std::process::Command::new("calc.exe").spawn();
    tokio::time::sleep(Duration::from_secs(6)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================");
    println!("captured {} action(s)\n", report.actions.len());
    println!("  {:<9} {:<28} source_app (for contrast)", "kind", "process_name");
    println!("  {}", "-".repeat(92));
    for a in &report.actions {
        println!(
            "  {:<9} {:<28} {:?}",
            a.kind.as_str(),
            format!("{:?}", a.process_name),
            a.source_app
        );
    }

    let total = report.actions.len();
    let with_proc = report
        .actions
        .iter()
        .filter(|a| a.process_name.as_deref().map(|p| !p.trim().is_empty()) == Some(true))
        .count();
    let navigates_with_proc = report
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::Navigate)
        .filter(|a| a.process_name.is_some())
        .count();
    let navigates = report
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::Navigate)
        .count();

    println!("\n--- verdict (capture layer) ---");
    println!("  actions carrying a process_name : {with_proc}/{total}");
    println!("  navigate actions with one       : {navigates_with_proc}/{navigates}");
    if with_proc == 0 {
        println!("\n  LAYER 1 FAILS: nothing carries a process name. Stop here.");
    } else if navigates > 0 && navigates_with_proc == 0 {
        println!("\n  LAYER 1 PARTIAL: navigate actions -- the ones whose selector needs");
        println!("  scoping -- carry no process name. The fix cannot work as designed.");
    } else {
        println!("\n  LAYER 1 OK: process names reach real captured actions.");
    }

    ExitCode::SUCCESS
}

// ------------------------------------------------------- ambiguity mode ----
//
// Feasibility check for the fail-loud fix, plus a reproduction of the
// ambiguity itself.
//
// The fix needs replay to know, at the moment it resolves a window selector,
// whether that selector matched more than one window. `Locator::all()` is the
// obvious way to count -- but during earlier work tonight
// `locator("role:Window").all()` returned ZERO Notepad windows while `first()`
// succeeded on the same tree, so its reliability for window selectors cannot be
// assumed. If `.all()` under-reports, counting with it would either miss real
// ambiguity or fail legitimate replays.
//
// This opens the same page in TWO browser windows so one title genuinely
// matches two windows, then reports what each path sees.

async fn ambiguity_mode() -> ExitCode {
    println!("== window selector ambiguity probe ==\n");
    println!("Opens one page in TWO windows, then asks what the resolution");
    println!("path replay uses actually reports.\n");

    let page = std::env::temp_dir().join("paradigm-ambiguity-probe.html");
    let html = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Ambiguity Probe Window</title></head>
<body style="font-family:sans-serif;padding:2rem"><h2>Ambiguity probe</h2>
<input id="f" name="AmbigField" aria-label="AmbigField" style="font-size:1.2rem;width:20rem">
</body></html>
"#;
    if let Err(e) = std::fs::write(&page, html) {
        eprintln!("could not write probe page: {e}");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));

    let browser = browser_order()[0];
    println!("opening window 1 in {browser}...");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", &url])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(8)).await;

    println!("opening window 2 (same page, same title)...");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", &url])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(8)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================\n");

    // The scoped forms are what the fix actually builds. `process:` is what
    // `Locator::all()` demanded when it rejected the desktop-wide versions --
    // this is where we find out whether supplying it is enough.
    let browser_proc = if browser == "chrome" { "chrome.exe" } else { "msedge.exe" };
    println!("(scoped selectors use process:{browser_proc})\n");

    for selector in [
        // Desktop-wide, as stored today -- known to be rejected by .all().
        "role:Window|name:Ambiguity Probe Window",
        "role:Edit|name:AmbigField",
        // Scoped, as the fix builds them.
        &format!("process:{browser_proc}|role:Window|name:Ambiguity Probe Window") as &str,
        &format!("process:{browser_proc}|role:Edit|name:AmbigField") as &str,
    ] {
        println!("selector {selector:?}");

        let first = desktop
            .locator(selector)
            .first(Some(Duration::from_secs(5)))
            .await;
        match &first {
            Ok(el) => println!(
                "  .first() -> Ok   role={:?} name={:?}   <-- replay acts on this today",
                el.role(),
                el.name()
            ),
            Err(e) => println!("  .first() -> Err  {e}"),
        }

        match desktop
            .locator(selector)
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            Ok(all) => {
                println!("  .all()   -> {} candidate(s)", all.len());
                for (i, el) in all.iter().take(6).enumerate() {
                    println!("      [{i}] role={:?} name={:?}", el.role(), el.name());
                }
            }
            Err(e) => println!("  .all()   -> Err  {e}"),
        }
        println!();
    }

    println!("--- what this decides ---");
    println!("  If .all() reports >= 2 for the duplicated window title while");
    println!("  .first() silently returns one, the fail-loud fix is implementable");
    println!("  and the ambiguity is reproduced.");
    println!("  If .all() reports 0 or 1 for a title that demonstrably matches two");
    println!("  windows, it CANNOT be used to detect ambiguity and the design");
    println!("  needs a different candidate-counting path.");

    ExitCode::SUCCESS
}

// ------------------------------------------------------- rootcount mode ----
// Third attempt at counting selector candidates.
//
// Route 1 satisfied `find_elements`' scoping guard with a `process:` prefix,
// which routes through the Chain arm and returned every top-level window of
// the process, ignoring role and name. This mode tests the OTHER way to
// satisfy that guard, which neither prior route used: supply a root element
// via `Locator::within()`. That reaches `Selector::Role`'s matcher directly,
// where `control_type` and `contains_name` filters are actually applied.
//
// The controls are the point. A counter that reports 2 for a duplicated title
// is worthless unless it also reports 1 for a unique one -- over-rejection of
// legitimate replays is what sank both previous attempts.
async fn rootcount_mode() -> ExitCode {
    println!("== root-scoped candidate counting ==\n");
    println!("Opens TWO windows sharing a title and ONE with a unique title,");
    println!("then counts candidates via within(desktop.root()).\n");

    let dup = std::env::temp_dir().join("paradigm-rootcount-dup.html");
    let uniq = std::env::temp_dir().join("paradigm-rootcount-uniq.html");
    let mk = |title: &str| {
        format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{title}</title></head>\n\
             <body style=\"font-family:sans-serif;padding:2rem\"><h2>{title}</h2>\n\
             <input id=\"f\" name=\"RootCountField\" aria-label=\"RootCountField\" \
             style=\"font-size:1.2rem;width:20rem\"></body></html>\n"
        )
    };
    if std::fs::write(&dup, mk("RootCount Duplicated")).is_err()
        || std::fs::write(&uniq, mk("RootCount Unique")).is_err()
    {
        eprintln!("could not write probe pages");
        return ExitCode::FAILURE;
    }
    let as_url = |p: &std::path::Path| format!("file:///{}", p.to_string_lossy().replace('\\', "/"));

    let browser = browser_order()[0];
    let browser_proc = if browser == "chrome" { "chrome.exe" } else { "msedge.exe" };
    for (n, path) in [("1 (dup)", &dup), ("2 (dup, same title)", &dup), ("3 (unique)", &uniq)] {
        println!("opening window {n}...");
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", &as_url(path)])
            .spawn()
        {
            let _ = c.wait();
        }
        tokio::time::sleep(Duration::from_secs(8)).await;
    }

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================\n");

    // (selector, what a correct counter must report)
    let cases: [(&str, &str); 4] = [
        ("role:Window|name:RootCount Duplicated", "2  (genuine ambiguity)"),
        ("role:Window|name:RootCount Unique", "1  (must NOT over-reject)"),
        ("role:Window|name:RootCount Absent Window", "0  (nothing matches)"),
        ("role:Edit|name:RootCountField", ">=3 (one per window)"),
    ];

    let mut verdict_ok = true;
    for (selector, expected) in cases {
        println!("selector {selector:?}");
        println!("  expected: {expected}");

        for depth in [Some(1usize), Some(3), None] {
            let started = std::time::Instant::now();
            let res = desktop
                .locator(selector)
                .within(desktop.root())
                .all(Some(Duration::from_secs(5)), depth)
                .await;
            let ms = started.elapsed().as_millis();
            let label = match depth {
                Some(d) => format!("depth {d}"),
                None => "depth default".to_string(),
            };
            match res {
                Ok(all) => {
                    println!("  {label:<14} -> {} candidate(s)   [{ms} ms]", all.len());
                    for (i, el) in all.iter().take(5).enumerate() {
                        println!("        [{i}] role={:?} name={:?}", el.role(), el.name());
                    }
                    // Sanity: every returned element must actually match the name.
                    let wanted = selector.rsplit("name:").next().unwrap_or("");
                    let bad = all
                        .iter()
                        .filter(|el| !el.name().unwrap_or_default().contains(wanted))
                        .count();
                    if bad > 0 {
                        println!("        !! {bad} returned element(s) do NOT contain the name");
                        verdict_ok = false;
                    }
                }
                Err(e) => println!("  {label:<14} -> Err  {e}   [{ms} ms]"),
            }
        }
        println!();
    }

    // The other untried primitive: a purpose-built window enumerator that does
    // not go through selector matching at all.
    println!("--- desktop.windows_for_application({browser_proc:?}) ---");
    let started = std::time::Instant::now();
    match desktop.windows_for_application(browser_proc).await {
        Ok(ws) => {
            println!("  -> {} window(s)   [{} ms]", ws.len(), started.elapsed().as_millis());
            for (i, el) in ws.iter().take(8).enumerate() {
                println!("      [{i}] name={:?}", el.name());
            }
        }
        Err(e) => println!("  -> Err  {e}   [{} ms]", started.elapsed().as_millis()),
    }

    println!("\n--- what this decides ---");
    println!("  A usable counter must report 2 for the duplicated title AND 1 for");
    println!("  the unique one. Reporting 2 for both means it counts windows, not");
    println!("  matches -- the same failure that closed Route 1, and unusable.");
    if !verdict_ok {
        println!("  NOTE: at least one result contained non-matching elements.");
    }

    ExitCode::SUCCESS
}

#[tokio::main]
async fn main() -> ExitCode {
    paradigm_lib::replay::ensure_dpi_aware();
    init_tracing();

    if std::env::args().any(|a| a == "replaycheck") {
        return replaycheck_mode().await;
    }
    if std::env::args().any(|a| a == "titledrift") {
        return titledrift_mode().await;
    }
    if std::env::args().any(|a| a == "verify") {
        return verify_mode().await;
    }
    if std::env::args().any(|a| a == "selectors") {
        return selectors_mode().await;
    }
    if std::env::args().any(|a| a == "gmailcleanup") {
        return gmailcleanup_mode().await;
    }
    if std::env::args().any(|a| a == "gmailpicker") {
        return gmailpicker_mode().await;
    }
    if std::env::args().any(|a| a == "widgets") {
        return widgets_mode().await;
    }
    if std::env::args().any(|a| a == "procname") {
        return procname_mode().await;
    }
    if std::env::args().any(|a| a == "rootcount") {
        return rootcount_mode().await;
    }
    if std::env::args().any(|a| a == "ambiguity") {
        return ambiguity_mode().await;
    }
    if std::env::args().any(|a| a == "windowid") {
        return windowid_mode().await;
    }
    if std::env::args().any(|a| a == "windowswitch") {
        return windowswitch_mode().await;
    }
    if std::env::args().any(|a| a == "multiline") {
        return multiline_mode().await;
    }
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
    for browser in browser_order() {
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
