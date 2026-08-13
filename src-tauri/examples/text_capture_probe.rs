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
// Four candidate checks are implemented as pure predicates and run against the
// actual data from each bug. Pure functions, because the pre-fix behaviour is
// already captured in the known-issues docs and re-creating it live would add
// nothing but risk.
//
// Two of the four are no longer candidates: the target-name check and the
// ambiguity count are both shipped in `replay/mod.rs`. They are kept here
// because this mode's purpose is the coverage table -- which bug each check
// does and does not catch -- and dropping the shipped ones would leave that
// table unable to say what is actually covered today.

/// PRE-EXECUTION. Does the element the selector resolved to actually carry the
/// name that was recorded? Cheap, and runs before anything is acted on.
fn check_target(recorded_name: &str, resolved_name: &str) -> bool {
    recorded_name == resolved_name
}

/// PRE-EXECUTION. Of the candidates the selector reaches, how many actually
/// carry the recorded name? More than one and the step is not decidable.
///
/// This is the shipped `replay::resolve_recorded` rule applied to a fixed
/// candidate list rather than a live desktop, and it calls the same predicate
/// the real code does. Note the two filters: the candidate list is what
/// `contains_name` reaches, and the count is of EXACT matches -- counting raw
/// candidates would flag the containment decoy below and refuse a replay that
/// is not ambiguous at all.
fn check_ambiguity(recorded: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .filter(|n| paradigm_lib::replay::resolved_is_recorded_target(recorded, n))
        .count()
        <= 1
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
        println!("\n  The target check cannot see it. Two windows genuinely share the");
        println!("  name, so the resolved element's name equals the recorded one and");
        println!("  every content check afterwards succeeds -- against the wrong window.");

        // Two windows, both genuinely named "Untitled - Notepad".
        let ambiguous = ["Untitled - Notepad", "Untitled - Notepad"];
        println!(
            "\n  pre-execution ambiguity count: {}",
            yn(check_ambiguity(recorded, &ambiguous))
        );
        println!("      candidates: {ambiguous:?}");
        println!("  Caught BEFORE acting, by counting how many candidates carry the");
        println!("  recorded name rather than checking only the one `first()` returned.");

        // The control that decides whether the count is usable at all: a decoy
        // whose title merely CONTAINS the recorded one must not be counted.
        let decoy = ["Untitled - Notepad", "Copy of Untitled - Notepad"];
        println!(
            "\n  same count, containment decoy present: {}",
            yn(check_ambiguity(recorded, &decoy))
        );
        println!("      candidates: {decoy:?}");
        println!("  Passes, correctly. Both candidates reach the selector by containment");
        println!("  but only one IS the recorded window, so this replay is unambiguous.");
        println!("  Counting raw candidates here would refuse it -- the over-rejection");
        println!("  that closed two earlier attempts. See");
        println!("  replay-window-selector-ambiguity.md.");
    }

    // ---- summary -----------------------------------------------------------
    println!("\n================ COVERAGE ================\n");
    println!("  bug                              A(action)  B(outcome)  B+context  target  ambig");
    println!("  {}", "-".repeat(84));
    println!("  multi-line duplication           no         YES         YES        no      no");
    println!("  window-switch misattribution     no         no          YES        no      no");
    println!("  substring selector collision     no         no          no         YES     no");
    println!("  selector ambiguity               no         no          no         no      YES");
    println!("\n  2 of 4 by pre-execution checks that are SHIPPED -- the target-name");
    println!("    check and the ambiguity count. The cheapest of the four, and the only");
    println!("    ones that prevent the wrong action instead of reporting it afterwards.");
    println!("  2 of 4 by outcome + recorded context, which capture does not store yet,");
    println!("    so those two remain uncovered in the product as it stands.");
    println!("  4 of 4 combined, but only once the outcome check is built.");

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

// ------------------------------------------------------ decoycount mode ----
// The decisive test for Route 3, per
// docs/known-issues/replay-window-selector-ambiguity.md.
//
// `rootcount` established that root-scoped counting discriminates: 2 for a
// duplicated title, 1 for a unique one. But its synthetic titles were chosen
// NOT to collide, so it never tested the failure that closed Routes 1 and 2 --
// OVER-REJECTION. Names match by containment, so an unrelated window whose
// title merely contains the recorded name inflates the count and would refuse
// a legitimate, unambiguous replay.
//
// The decoy here is a natural shape, not a contrived one. Browser windows are
// titled "<page> - <profile> - <browser>", so a page titled "Draft X" produces
// a window title that ENDS WITH, and therefore contains, the whole window title
// of a page titled "X". A leading extension collides; a trailing one does not.
// Both are measured.
//
// The mitigation under test is the already-shipped `resolved_is_recorded_target`
// (equality, tolerating one leading `*`) used as a filter before counting. It is
// called here directly -- not reimplemented -- so the test exercises the real
// predicate.
//
// Two things must both hold for Route 3 to survive:
//   * the filter drops the decoy, leaving 1 for the genuinely unambiguous case
//   * the filter still reports 2 for two genuinely identical windows
// A filter that rescues the first by suppressing the second is worthless.

/// Root-scoped count for `selector`, returned as (raw, filtered, names).
async fn root_count(
    desktop: &Desktop,
    selector: &str,
    recorded: &str,
    depth: Option<usize>,
) -> Result<(usize, usize, Vec<String>), String> {
    let all = desktop
        .locator(selector)
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), depth)
        .await
        .map_err(|e| e.to_string())?;
    let names: Vec<String> = all.iter().map(|el| el.name().unwrap_or_default()).collect();
    let filtered = names
        .iter()
        .filter(|n| paradigm_lib::replay::resolved_is_recorded_target(recorded, n))
        .count();
    Ok((all.len(), filtered, names))
}

/// Name of a target window that has a window to itself.
///
/// A title containing "and N more pages" is a shared, multi-tab window: its name
/// depends on tabs that are not ours, so it is not a usable recorded name.
async fn clean_target_name(desktop: &Desktop) -> Option<String> {
    desktop
        .locator("role:Window|name:DecoyCount Invoice")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), Some(3))
        .await
        .ok()
        .and_then(|all| {
            all.iter()
                .map(|w| w.name().unwrap_or_default())
                .find(|n| !n.contains("more pages"))
        })
}

async fn decoycount_mode() -> ExitCode {
    println!("== over-rejection test for root-scoped candidate counting ==\n");

    let page = |title: &str| {
        format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{title}</title></head>\n\
             <body style=\"font-family:sans-serif;padding:2rem\"><h2>{title}</h2></body></html>\n"
        )
    };
    // Page titles. TARGET is what a recording would have captured; DECOY is an
    // unrelated window whose full title will contain TARGET's full title;
    // NEARMISS extends the title on the other side and must NOT collide.
    let specs = [
        ("warmup", "DecoyCount Warmup"),
        ("target", "DecoyCount Invoice"),
        ("decoy", "Draft DecoyCount Invoice"),
        ("nearmiss", "DecoyCount Invoice Notes"),
        ("twin", "DecoyCount Twin"),
    ];
    let mut paths = std::collections::HashMap::new();
    for (key, title) in specs {
        let p = std::env::temp_dir().join(format!("paradigm-decoycount-{key}.html"));
        if std::fs::write(&p, page(title)).is_err() {
            eprintln!("could not write probe page {key}");
            return ExitCode::FAILURE;
        }
        paths.insert(key, p);
    }
    let as_url =
        |p: &std::path::Path| format!("file:///{}", p.to_string_lossy().replace('\\', "/"));

    let browser = browser_order()[0];
    let open = |path: &std::path::Path| {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", &as_url(path)])
            .spawn()
        {
            let _ = c.wait();
        }
    };

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A first `--new-window` against an already-running Edge can land the URL as
    // a TAB in an existing window instead of creating one. Measured: the target
    // came back as "DecoyCount Invoice and 39 more pages - …", a title carrying
    // 39 unrelated tabs. The warm-up window absorbs that so the target gets a
    // window of its own.
    println!("opening warm-up window (absorbs Edge's merge-into-existing behaviour)...");
    open(&paths["warmup"]);
    tokio::time::sleep(Duration::from_secs(8)).await;

    // The target opens ALONE and its real title is read before anything that
    // could collide exists, so `recorded` is what capture would have stored.
    println!("opening target window...");
    open(&paths["target"]);
    tokio::time::sleep(Duration::from_secs(8)).await;

    let mut recorded_target = clean_target_name(&desktop).await;
    if recorded_target.is_none() {
        println!("  target merged into a multi-tab window; opening one more...");
        open(&paths["target"]);
        tokio::time::sleep(Duration::from_secs(8)).await;
        recorded_target = clean_target_name(&desktop).await;
    }
    let Some(recorded_target) = recorded_target else {
        eprintln!(
            "could not obtain a single-tab target window, so no usable recorded name exists"
        );
        return ExitCode::FAILURE;
    };
    println!("  recorded target name: {recorded_target:?}\n");

    for key in ["decoy", "nearmiss", "twin", "twin"] {
        println!("opening {key} window...");
        open(&paths[key]);
        tokio::time::sleep(Duration::from_secs(8)).await;
    }

    let recorded_twin = match desktop
        .locator("role:Window|name:DecoyCount Twin")
        .first(Some(Duration::from_secs(5)))
        .await
    {
        Ok(w) => w.name().unwrap_or_default(),
        Err(e) => {
            eprintln!("could not resolve a twin window: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("  recorded twin name:   {recorded_twin:?}\n");

    // ---- precondition -------------------------------------------------------
    // If the decoy does not actually contain the recorded name, the containment
    // collision this test exists to measure was never built, and any "filter
    // works" verdict below would be meaningless. Prove it before trusting it.
    println!("================ PRECONDITION ================\n");
    let live: Vec<String> = match desktop
        .locator("role:Window|name:DecoyCount")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), Some(3))
        .await
    {
        Ok(all) => all.iter().map(|e| e.name().unwrap_or_default()).collect(),
        Err(e) => {
            eprintln!("could not enumerate probe windows: {e}");
            return ExitCode::FAILURE;
        }
    };
    for (i, n) in live.iter().enumerate() {
        println!("  window[{i}] {n:?}");
    }
    let decoy_name = live
        .iter()
        .find(|n| n.starts_with("Draft DecoyCount Invoice"))
        .cloned();
    let nearmiss_name = live
        .iter()
        .find(|n| n.starts_with("DecoyCount Invoice Notes"))
        .cloned();
    let decoy_collides = decoy_name
        .as_deref()
        .map(|n| n.contains(&recorded_target))
        .unwrap_or(false);
    let nearmiss_collides = nearmiss_name
        .as_deref()
        .map(|n| n.contains(&recorded_target))
        .unwrap_or(false);
    println!("\n  decoy    present={}  contains recorded name={decoy_collides}", decoy_name.is_some());
    println!("  nearmiss present={}  contains recorded name={nearmiss_collides}", nearmiss_name.is_some());
    if !decoy_collides {
        println!("\n  !! PRECONDITION FAILED: no containment collision was built.");
        println!("     Everything below is measuring a scenario that does not exist.");
    }

    // ---- repeated measurement ----------------------------------------------
    println!("\n================ MEASUREMENTS ================");
    let target_sel = format!("role:Window|name:{recorded_target}");
    let twin_sel = format!("role:Window|name:{recorded_twin}");

    let mut target_raw = Vec::new();
    let mut target_filtered = Vec::new();
    let mut twin_raw = Vec::new();
    let mut twin_filtered = Vec::new();

    for round in 1..=3 {
        println!("\n--- round {round} ---");
        for (label, selector, recorded, raws, filts, want) in [
            (
                "TARGET (must not over-reject)",
                &target_sel,
                &recorded_target,
                &mut target_raw,
                &mut target_filtered,
                1usize,
            ),
            (
                "TWIN   (must still catch ambiguity)",
                &twin_sel,
                &recorded_twin,
                &mut twin_raw,
                &mut twin_filtered,
                2usize,
            ),
        ] {
            let started = std::time::Instant::now();
            match root_count(&desktop, selector, recorded, Some(3)).await {
                Ok((raw, filtered, names)) => {
                    let ms = started.elapsed().as_millis();
                    println!(
                        "{label}\n  raw={raw}  filtered={filtered}  (want filtered={want})  [{ms} ms]"
                    );
                    for (i, n) in names.iter().enumerate() {
                        let kept =
                            paradigm_lib::replay::resolved_is_recorded_target(recorded, n);
                        println!("      [{i}] {} {n:?}", if kept { "KEEP" } else { "drop" });
                    }
                    raws.push(raw);
                    filts.push(filtered);
                }
                Err(e) => {
                    println!("{label}\n  -> Err {e}");
                    raws.push(usize::MAX);
                    filts.push(usize::MAX);
                }
            }
        }
    }

    // Reconfirm the depth finding from the previous run, once, on the target.
    println!("\n--- depth default, target, once (reconfirms the depth-3 finding) ---");
    let started = std::time::Instant::now();
    match root_count(&desktop, &target_sel, &recorded_target, None).await {
        Ok((raw, filtered, _)) => println!(
            "  raw={raw}  filtered={filtered}   [{} ms]",
            started.elapsed().as_millis()
        ),
        Err(e) => println!("  -> Err {e}   [{} ms]", started.elapsed().as_millis()),
    }

    // ---- verdict ------------------------------------------------------------
    println!("\n================ VERDICT ================\n");
    println!("  target   raw={target_raw:?}  filtered={target_filtered:?}");
    println!("  twin     raw={twin_raw:?}  filtered={twin_filtered:?}");

    let inflated = target_raw.iter().all(|&n| n >= 2);
    let rescued = target_filtered.iter().all(|&n| n == 1);
    let still_catches = twin_filtered.iter().all(|&n| n == 2);

    println!("\n  containment inflates the raw count : {inflated}");
    println!("  filter rescues the unambiguous case: {rescued}");
    println!("  filter still catches real ambiguity: {still_catches}");
    println!();
    if !decoy_collides {
        println!("  INVALID -- the decoy never collided; this run decides nothing.");
    } else if !inflated {
        println!("  Containment did NOT inflate the count, so the over-rejection");
        println!("  risk did not reproduce here. The filter was not exercised.");
    } else if rescued && still_catches {
        println!("  Route 3 SURVIVES this test: containment does inflate the raw");
        println!("  count, the shipped filter removes exactly the containment-only");
        println!("  match, and genuine ambiguity is still reported as 2.");
    } else {
        println!("  Route 3 DIES the same way as Routes 1 and 2: the count cannot");
        println!("  separate a legitimate replay from an ambiguous one.");
    }

    // ---- cleanup ------------------------------------------------------------
    // Leftover windows would poison a repeat run's counts, which is the whole
    // point of repeating it.
    println!("\nclosing probe windows...");
    if let Ok(all) = desktop
        .locator("role:Window|name:DecoyCount")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), Some(3))
        .await
    {
        // NEVER close a window whose title says "and N more pages". That is a
        // shared, multi-tab window: our page is one tab in it and the rest are
        // the user's. An earlier run of this probe closed one such window and
        // took 39 unrelated tabs with it.
        let mut closed = 0;
        let mut spared = 0;
        for el in &all {
            let name = el.name().unwrap_or_default();
            if name.contains("more pages") {
                println!("  SPARED (shared multi-tab window) {name:?}");
                spared += 1;
                continue;
            }
            if el.close().is_ok() {
                closed += 1;
            }
        }
        println!("  closed {closed}, spared {spared}, of {} matched", all.len());
    }

    ExitCode::SUCCESS
}

// ------------------------------------------------------ ambigreplay mode ----
// End-to-end verification of the wired-in ambiguity check, through the REAL
// `replay::replay` -> `navigate()` / element path. `decoycount` measured the
// mechanism in isolation; this measures the shipped behaviour.
//
// Four trials against one stored playbook, each adding a window to the desktop:
//
//   1. clean          only the target is open      -> must COMPLETE
//   2. + containment  a window and a field whose
//      decoy          names CONTAIN the recorded
//                     ones                          -> must still COMPLETE
//   3. + field twin   a differently-titled window
//                     holding a field with the
//                     EXACT recorded field name     -> click must refuse
//   4. + window twin  a window with the EXACT
//                     recorded title                -> navigate must refuse
//
// Trials 3 and 4 hit the two resolution sites separately, which is why the
// field twin has a different window title: a window-level refusal stops the run
// before any element step is reached.
//
// The playbook is constructed from live-observed values -- the real window title
// read off the desktop, the real field name -- rather than driven through OS
// capture. Capture is not what changed and has its own probes; routing through
// `compile` + `store` + `replay` exercises everything that did.

/// Page with a static title, so nothing drifts between the four replays.
fn ambig_page(title: &str, field: &str) -> String {
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{title}</title></head>\n\
         <body style=\"font-family:sans-serif;padding:2rem\"><h2>{title}</h2>\n\
         <input id=\"f\" aria-label=\"{field}\" style=\"font-size:1.2rem;width:24rem\">\n\
         </body></html>\n"
    )
}

/// Print what actually matches a selector right now, and how many carry the
/// recorded name exactly.
///
/// Every trial below asserts something about a count, so the count has to be
/// shown before the trial is believed. The previous probe in this investigation
/// reported a clean pass against a decoy that had never been created.
async fn show_candidates(desktop: &Desktop, selector: &str, recorded: &str) -> usize {
    let all = match desktop
        .locator(selector)
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        Ok(all) => all,
        Err(e) => {
            println!("  premise: {selector:?} -> Err {e}");
            return 0;
        }
    };
    let mut exact = 0;
    println!("  premise: {selector:?} -> {} raw candidate(s)", all.len());
    for el in &all {
        let name = el.name().unwrap_or_default();
        let is_exact = paradigm_lib::replay::resolved_is_recorded_target(recorded, &name);
        if is_exact {
            exact += 1;
        }
        println!("      {} {name:?}", if is_exact { "EXACT" } else { "  ~  " });
    }
    println!("  premise: {exact} candidate(s) carry the recorded name exactly");
    exact
}

// ----------------------------------------------------- resolveorder mode ----
// The gap the ambigreplay run left open.
//
// Constructive resolution -- acting on the uniquely-matching candidate rather
// than on `first()`'s pick -- was only ever tested in the direction where
// `first()` picks WRONG and the change rescues the step. That is the direction
// that makes it look good. The reverse was never exercised: when `first()` would
// already have returned the correct element, does the change leave it alone, or
// does it quietly substitute something else?
//
// Traversal order is the lever, and window z-order is what moves it. Each trial
// activates the three windows in a chosen sequence, records what `first()`
// actually returns under that ordering, then replays.
//
// The check is not "did the run complete". A run can complete having typed into
// the wrong window -- that is the entire bug this fix exists for. So each trial
// reads all three fields before and after and confirms the text landed in the
// target's field and nowhere else.

/// Bring the window whose title starts with `needle` to the front.
async fn activate_by_title(desktop: &Desktop, needle: &str) -> bool {
    let sel = format!("role:Window|name:{needle}");
    match desktop
        .locator(sel.as_str())
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        // `contains_name` also reaches "Draft <needle>", so match on the prefix.
        Ok(all) => all
            .iter()
            .find(|el| el.name().unwrap_or_default().starts_with(needle))
            .map(|el| el.activate_window().is_ok())
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// Current contents of every field reachable as `OrderField`, keyed by name.
async fn read_order_fields(desktop: &Desktop) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Ok(all) = desktop
        .locator("role:Edit|name:OrderField")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for el in &all {
            out.insert(
                el.name().unwrap_or_default(),
                el.text(0).unwrap_or_default(),
            );
        }
    }
    out
}

/// Name of whatever `first()` returns for `selector` right now -- i.e. what
/// replay would have acted on before the constructive-resolution change.
async fn first_pick(desktop: &Desktop, selector: &str) -> String {
    match desktop
        .locator(selector)
        .first(Some(Duration::from_secs(5)))
        .await
    {
        Ok(el) => el.name().unwrap_or_default(),
        Err(e) => format!("<Err {e}>"),
    }
}

/// Replay one stored playbook, time it, and print every step outcome.
async fn run_trial(
    conn: &mut rusqlite::Connection,
    desktop: &Desktop,
    playbook_id: &str,
    label: &str,
    expect: &str,
    rows: &mut Vec<(String, String, u128, String)>,
) {
    println!("\n--- {label} ---");
    println!("  expect: {expect}");
    let started = std::time::Instant::now();
    let run = paradigm_lib::replay::replay(conn, desktop, playbook_id).await;
    let ms = started.elapsed().as_millis();
    match run {
        Ok(r) => {
            println!("  status: {}   [{ms} ms]", r.status);
            let mut worst = String::from("(none)");
            for o in &r.outcomes {
                println!(
                    "    [{}] {:<9} {}",
                    o.step_order,
                    o.action_type,
                    o.result.label()
                );
                if o.result.is_failure() {
                    worst = format!("{} @ {}", o.result.label(), o.action_type);
                    for line in o.detail.lines() {
                        println!("         {line}");
                    }
                }
            }
            rows.push((label.to_string(), r.status.clone(), ms, worst));
        }
        Err(e) => {
            println!("  replay errored: {e}");
            rows.push((label.to_string(), format!("ERROR {e}"), ms, String::new()));
        }
    }
}

async fn ambigreplay_mode() -> ExitCode {
    use paradigm_lib::capture::stream::{ActionCandidate, CapturedStream};
    use paradigm_lib::capture::ExclusionList;
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    println!("== ambiguity check, end to end through a real replay ==\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    let pages = [
        ("warmup", ambig_page("AmbigReplay Warmup", "WarmupField")),
        ("target", ambig_page("AmbigReplay Invoice", "AmbigField")),
        // Contains the recorded window title AND the recorded field name.
        ("decoy", ambig_page("Draft AmbigReplay Invoice", "Draft AmbigField")),
        // Different title, EXACT field name -> element-level ambiguity only.
        ("fieldtwin", ambig_page("AmbigReplay Ledger", "AmbigField")),
    ];
    let mut paths = std::collections::HashMap::new();
    for (key, html) in &pages {
        let p = std::env::temp_dir().join(format!("paradigm-ambigreplay-{key}.html"));
        if std::fs::write(&p, html).is_err() {
            eprintln!("could not write probe page {key}");
            return ExitCode::FAILURE;
        }
        paths.insert(*key, p);
    }
    let as_url =
        |p: &std::path::Path| format!("file:///{}", p.to_string_lossy().replace('\\', "/"));
    let browser = browser_order()[0];
    let open = |path: &std::path::Path| {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", &as_url(path)])
            .spawn()
        {
            let _ = c.wait();
        }
    };

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("opening warm-up window...");
    open(&paths["warmup"]);
    tokio::time::sleep(Duration::from_secs(8)).await;
    println!("opening target window...");
    open(&paths["target"]);
    tokio::time::sleep(Duration::from_secs(8)).await;

    // Read the real title off the desktop. A multi-tab window's title depends on
    // tabs that are not ours, so it is not a usable recorded name.
    let recorded_window = match desktop
        .locator("role:Window|name:AmbigReplay Invoice")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| {
            all.iter()
                .map(|w| w.name().unwrap_or_default())
                .find(|n| !n.contains("more pages"))
        }) {
        Some(n) => n,
        None => {
            eprintln!("no single-tab target window; cannot build a usable recorded name");
            return ExitCode::FAILURE;
        }
    };
    println!("  recorded window name: {recorded_window:?}\n");

    // ---- build the playbook through the real compile + store path -----------
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
    for (kind, role, name, payload) in [
        (ActionKind::Navigate, "Window", recorded_window.as_str(), None),
        (ActionKind::Click, "Edit", "AmbigField", None),
        (ActionKind::Type, "Edit", "AmbigField", Some("ambig")),
    ] {
        stream.admit(ActionCandidate {
            kind,
            identifiers: vec![recorded_window.clone()],
            process_name: Some(format!("{browser}.exe")),
            element_role: Some(role.to_string()),
            element_name: Some(name.to_string()),
            payload: payload.map(str::to_string),
            detail: None,
            timestamp_ms: 0,
        });
    }
    let actions = stream.actions().to_vec();
    println!("  built {} action(s)", actions.len());

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

    let checked = compile(
        &actions,
        "Ambiguity Check",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );

    // The same playbook as an OLD one: `target.name` absent, so both the target
    // check and the ambiguity check skip. Same selectors, same steps, same UI --
    // so the difference in wall-clock IS the cost of the checks, and replaying it
    // also proves old playbooks still behave exactly as they did.
    let mut unchecked = compile(
        &actions,
        "Ambiguity Check (old playbook, no target name)",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    for step in &mut unchecked.steps {
        let mut v: serde_json::Value = match serde_json::from_str(&step.action_payload_json) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("could not parse compiled payload: {e}");
                return ExitCode::FAILURE;
            }
        };
        if let Some(target) = v.get_mut("target").and_then(|t| t.as_object_mut()) {
            target.remove("name");
        }
        step.action_payload_json = v.to_string();
    }

    for pb in [&checked, &unchecked] {
        if let Err(e) = store::store(&mut conn, pb) {
            eprintln!("store failed: {e}");
            return ExitCode::FAILURE;
        }
    }

    // ---- trials -------------------------------------------------------------
    let mut rows: Vec<(String, String, u128, String)> = Vec::new();

    // Trial 1 + the timing A/B, both in the clean state.
    run_trial(
        &mut conn,
        &desktop,
        &checked.id,
        "1. clean, checks ON",
        "completed",
        &mut rows,
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    run_trial(
        &mut conn,
        &desktop,
        &unchecked.id,
        "1b. clean, old playbook (checks OFF)",
        "completed",
        &mut rows,
    )
    .await;

    println!("\nopening the containment decoy...");
    open(&paths["decoy"]);
    tokio::time::sleep(Duration::from_secs(8)).await;
    let win_sel = format!("role:Window|name:{recorded_window}");
    let w2 = show_candidates(&desktop, &win_sel, &recorded_window).await;
    let f2 = show_candidates(&desktop, "role:Edit|name:AmbigField", "AmbigField").await;
    if w2 != 1 || f2 != 1 {
        println!("  !! trial 2 premise broken: expected exactly 1 exact match at each level");
    }
    run_trial(
        &mut conn,
        &desktop,
        &checked.id,
        "2. containment decoy present",
        "completed -- decoy must NOT be counted",
        &mut rows,
    )
    .await;

    println!("\nopening the field twin (same field name, different window title)...");
    open(&paths["fieldtwin"]);
    tokio::time::sleep(Duration::from_secs(8)).await;
    let w3 = show_candidates(&desktop, &win_sel, &recorded_window).await;
    let f3 = show_candidates(&desktop, "role:Edit|name:AmbigField", "AmbigField").await;
    if w3 != 1 || f3 < 2 {
        println!("  !! trial 3 premise broken: need 1 exact window and >=2 exact fields");
    }
    run_trial(
        &mut conn,
        &desktop,
        &checked.id,
        "3. element-level ambiguity",
        "failed -- click refuses, navigate is still fine",
        &mut rows,
    )
    .await;

    println!("\nopening the window twin (identical title)...");
    open(&paths["target"]);
    tokio::time::sleep(Duration::from_secs(8)).await;
    let w4 = show_candidates(&desktop, &win_sel, &recorded_window).await;
    if w4 < 2 {
        println!("  !! trial 4 premise broken: Edge did not create a second window with the");
        println!("     recorded title, so there is no window-level ambiguity to detect");
    }
    run_trial(
        &mut conn,
        &desktop,
        &checked.id,
        "4. window-level ambiguity",
        "failed -- navigate refuses",
        &mut rows,
    )
    .await;

    // ---- summary ------------------------------------------------------------
    println!("\n================ SUMMARY ================\n");
    println!("  {:<38} {:<10} {:>8}  {}", "trial", "status", "ms", "failure");
    for (label, status, ms, worst) in &rows {
        println!("  {label:<38} {status:<10} {ms:>8}  {worst}");
    }

    if rows.len() >= 2 {
        let on = rows[0].2 as i128;
        let off = rows[1].2 as i128;
        println!("\n--- cost of the check, same steps, same UI ---");
        println!("  checks ON : {on} ms");
        println!("  checks OFF: {off} ms");
        println!("  added     : {} ms over 3 steps ({} ms/step)", on - off, (on - off) / 3);
    }

    println!("\n--- cleanup ---");
    if let Ok(all) = desktop
        .locator("role:Window|name:AmbigReplay")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        let mut closed = 0;
        for el in &all {
            let name = el.name().unwrap_or_default();
            // Never close a shared multi-tab window: only one tab is ours.
            if name.contains("more pages") {
                println!("  SPARED {name:?}");
                continue;
            }
            if el.close().is_ok() {
                closed += 1;
            }
        }
        println!("  closed {closed} probe window(s)");
    }
    // The decoy's title does not start with "AmbigReplay", so it needs its own sweep.
    if let Ok(all) = desktop
        .locator("role:Window|name:Draft AmbigReplay")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for el in &all {
            if !el.name().unwrap_or_default().contains("more pages") {
                let _ = el.close();
            }
        }
    }

    ExitCode::SUCCESS
}

async fn resolveorder_mode() -> ExitCode {
    use paradigm_lib::capture::stream::{ActionCandidate, CapturedStream};
    use paradigm_lib::capture::ExclusionList;
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    println!("== constructive resolution under varied traversal order ==\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    // Two decoys, so orderings can put the target first, last, or in the middle.
    // Both window titles contain the target's whole title once the browser
    // appends its suffix; both field names contain the target's field name.
    let pages = [
        ("warmup", ambig_page("ResolveOrder Warmup", "WarmupField")),
        ("target", ambig_page("ResolveOrder Invoice", "OrderField")),
        ("decoya", ambig_page("Draft ResolveOrder Invoice", "Draft OrderField")),
        ("decoyb", ambig_page("Copy of ResolveOrder Invoice", "Copy of OrderField")),
    ];
    let mut paths = std::collections::HashMap::new();
    for (key, html) in &pages {
        let p = std::env::temp_dir().join(format!("paradigm-resolveorder-{key}.html"));
        if std::fs::write(&p, html).is_err() {
            eprintln!("could not write probe page {key}");
            return ExitCode::FAILURE;
        }
        paths.insert(*key, p);
    }
    let as_url =
        |p: &std::path::Path| format!("file:///{}", p.to_string_lossy().replace('\\', "/"));
    let browser = browser_order()[0];
    let open = |path: &std::path::Path| {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", &as_url(path)])
            .spawn()
        {
            let _ = c.wait();
        }
    };

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    for key in ["warmup", "target", "decoya", "decoyb"] {
        println!("opening {key} window...");
        open(&paths[key]);
        tokio::time::sleep(Duration::from_secs(8)).await;
    }

    let recorded_window = match desktop
        .locator("role:Window|name:ResolveOrder Invoice")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| {
            all.iter()
                .map(|w| w.name().unwrap_or_default())
                .find(|n| n.starts_with("ResolveOrder Invoice") && !n.contains("more pages"))
        }) {
        Some(n) => n,
        None => {
            eprintln!("no single-tab target window; cannot build a usable recorded name");
            return ExitCode::FAILURE;
        }
    };
    println!("\n  recorded window name: {recorded_window:?}");

    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
    for (kind, role, name, payload) in [
        (
            paradigm_lib::capture::ActionKind::Navigate,
            "Window",
            recorded_window.as_str(),
            None,
        ),
        (
            paradigm_lib::capture::ActionKind::Click,
            "Edit",
            "OrderField",
            None,
        ),
        (
            paradigm_lib::capture::ActionKind::Type,
            "Edit",
            "OrderField",
            Some("ordr"),
        ),
    ] {
        stream.admit(ActionCandidate {
            kind,
            identifiers: vec![recorded_window.clone()],
            process_name: Some(format!("{browser}.exe")),
            element_role: Some(role.to_string()),
            element_name: Some(name.to_string()),
            payload: payload.map(str::to_string),
            detail: None,
            timestamp_ms: 0,
        });
    }

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
        &stream.actions().to_vec(),
        "Resolve Order",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    if let Err(e) = store::store(&mut conn, &playbook) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }

    let win_sel = format!("role:Window|name:{recorded_window}");
    let fld_sel = "role:Edit|name:OrderField";

    // Activation sequences. The LAST window activated ends up frontmost, which
    // is what moves it earlier in the desktop's traversal order.
    let orderings: [(&str, [&str; 3]); 6] = [
        ("target front, A then B", ["Draft ResolveOrder", "Copy of ResolveOrder", "ResolveOrder Invoice"]),
        ("target front, B then A", ["Copy of ResolveOrder", "Draft ResolveOrder", "ResolveOrder Invoice"]),
        ("decoy A frontmost", ["ResolveOrder Invoice", "Copy of ResolveOrder", "Draft ResolveOrder"]),
        ("decoy B frontmost", ["ResolveOrder Invoice", "Draft ResolveOrder", "Copy of ResolveOrder"]),
        ("target middle, A front", ["Copy of ResolveOrder", "ResolveOrder Invoice", "Draft ResolveOrder"]),
        ("target middle, B front", ["Draft ResolveOrder", "ResolveOrder Invoice", "Copy of ResolveOrder"]),
    ];

    struct Row {
        label: String,
        first_win_ok: bool,
        first_fld_ok: bool,
        status: String,
        resolution_ok: bool,
        text_exact: bool,
    }
    let mut rows: Vec<Row> = Vec::new();

    for (label, sequence) in orderings {
        println!("\n================ ordering: {label} ================");
        for needle in sequence {
            let ok = activate_by_title(&desktop, needle).await;
            println!("  activate {needle:?} -> {ok}");
            tokio::time::sleep(Duration::from_millis(900)).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;

        // What would replay have acted on WITHOUT the constructive change?
        let fw = first_pick(&desktop, &win_sel).await;
        let ff = first_pick(&desktop, fld_sel).await;
        let first_win_ok = fw == recorded_window;
        let first_fld_ok = ff == "OrderField";
        println!("  first() window -> {fw:?}   correct on its own: {first_win_ok}");
        println!("  first() field  -> {ff:?}   correct on its own: {first_fld_ok}");

        let before = read_order_fields(&desktop).await;
        let mut trial_rows = Vec::new();
        run_trial(
            &mut conn,
            &desktop,
            &playbook.id,
            label,
            "completed, and the text lands in the target field only",
            &mut trial_rows,
        )
        .await;
        let status = trial_rows
            .first()
            .map(|r| r.1.clone())
            .unwrap_or_else(|| "?".to_string());
        let after = read_order_fields(&desktop).await;

        let get = |m: &std::collections::HashMap<String, String>, k: &str| {
            m.get(k).cloned().unwrap_or_default()
        };
        // Two different questions, deliberately not collapsed into one flag.
        //
        // Resolution is about WHERE the text went: the target field changed and
        // neither decoy did. Text fidelity is about WHAT arrived, which depends
        // on real keystrokes reaching a real browser and can be disturbed by
        // anything holding focus. A run that types the wrong characters into the
        // RIGHT field says nothing about the resolution logic, and reporting it
        // as a resolution failure would be exactly the kind of diagnostic that
        // cannot tell two causes apart.
        let target_changed = get(&after, "OrderField") != get(&before, "OrderField");
        let decoys_clean = get(&after, "Draft OrderField") == get(&before, "Draft OrderField")
            && get(&after, "Copy of OrderField") == get(&before, "Copy of OrderField");
        let resolution_ok = target_changed && decoys_clean;
        let text_exact = get(&after, "OrderField")
            == format!("{}{}", get(&before, "OrderField"), "ordr");

        println!("  field readback:");
        for name in ["OrderField", "Draft OrderField", "Copy of OrderField"] {
            println!(
                "    {name:<20} {:?} -> {:?}",
                get(&before, name),
                get(&after, name)
            );
        }
        println!("  resolution -- target changed, decoys untouched: {resolution_ok}");
        println!("  text fidelity -- exactly \"ordr\" appended      : {text_exact}");
        if resolution_ok && !text_exact {
            println!("      (right field, wrong characters: a typing disturbance, not a");
            println!("       resolution fault -- resolution is the decoys staying clean)");
        }

        rows.push(Row {
            label: label.to_string(),
            first_win_ok,
            first_fld_ok,
            status,
            resolution_ok,
            text_exact,
        });
    }

    // ---- summary ------------------------------------------------------------
    println!("\n================ SUMMARY ================\n");
    println!(
        "  {:<24} {:>9} {:>9}  {:<10} {:>10} {:>6}",
        "ordering", "first():W", "first():F", "status", "resolution", "text"
    );
    for r in &rows {
        println!(
            "  {:<24} {:>9} {:>9}  {:<10} {:>10} {:>6}",
            r.label,
            if r.first_win_ok { "correct" } else { "WRONG" },
            if r.first_fld_ok { "correct" } else { "WRONG" },
            r.status,
            if r.resolution_ok { "ok" } else { "WRONG" },
            if r.text_exact { "ok" } else { "off" }
        );
    }

    let all_completed = rows.iter().all(|r| r.status == "completed");
    let all_resolved = rows.iter().all(|r| r.resolution_ok);
    let text_off = rows.iter().filter(|r| !r.text_exact).count();
    let reverse: Vec<&Row> = rows
        .iter()
        .filter(|r| r.first_win_ok && r.first_fld_ok)
        .collect();
    let forward = rows.len() - reverse.len();

    println!("\n--- what this covers ---");
    println!("  orderings where first() was already correct    : {}", reverse.len());
    println!("  orderings where first() would have picked wrong: {forward}");

    if reverse.is_empty() {
        println!("\n  INCONCLUSIVE for the gap being tested: no ordering put first() on");
        println!("  the correct element, so the reverse direction was never exercised.");
    } else if !all_resolved {
        println!("\n  RESOLUTION FAILURE: some ordering sent the text somewhere other than");
        println!("  the target field. This is a genuine defect in constructive resolution.");
    } else {
        let reverse_ok = reverse.iter().all(|r| r.resolution_ok && r.status == "completed");
        println!(
            "\n  Reverse direction (first() already correct): {} of {} completed and",
            reverse.iter().filter(|r| r.status == "completed" && r.resolution_ok).count(),
            reverse.len()
        );
        println!("  resolved to the target -- the change left a correct pick alone.");
        if reverse_ok && all_completed {
            println!("\n  PASS in both directions across every ordering tried.");
        }
        if text_off > 0 {
            println!(
                "\n  NOTE: {text_off} run(s) put the right text in the wrong shape -- correct"
            );
            println!("  field, disturbed characters. Real keystrokes into a real browser are");
            println!("  not deterministic; this is orthogonal to resolution, which is judged");
            println!("  by the decoy fields staying empty. Reported rather than smoothed over.");
        }
    }

    println!("\n--- cleanup ---");
    for needle in ["ResolveOrder", "Draft ResolveOrder", "Copy of ResolveOrder"] {
        if let Ok(all) = desktop
            .locator(format!("role:Window|name:{needle}").as_str())
            .within(desktop.root())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            for el in &all {
                if !el.name().unwrap_or_default().contains("more pages") {
                    let _ = el.close();
                }
            }
        }
    }
    println!("  probe windows closed");

    ExitCode::SUCCESS
}

// ---------------------------------------------------------- sheets mode ----
// What does UIA actually see inside a Google Sheets grid?
//
// complex-web-grid-capture-unreliable.md records two hypotheses for why cell
// typing and attribution are unreliable, neither tested:
//
//   H1  cells are canvas-rendered and are not real accessible elements at all
//   H2  cell identity comes from the Name Box, which updates asynchronously, so
//       capture reads it before Sheets has caught up
//
// The two make different, checkable predictions. If H1 holds, moving between
// cells changes nothing about the focused element's identity, because there is
// no per-cell element to change to. If H2 holds, there IS a per-cell identity
// somewhere and the question is only when it settles -- so the Name Box should
// be observably stale for some measurable interval after a move.
//
// This mode measures both, plus a typing race, and prints raw values throughout
// so the conclusion can be checked rather than taken.
//
// Opens sheets.new, which creates a blank "Untitled spreadsheet" in the signed-in
// account's Drive. Nothing is read from any existing document.

/// One line describing an element, for identity comparison across steps.
fn snap(el: &UIElement) -> String {
    let a = el.attributes();
    let b = el
        .bounds()
        .ok()
        .map(|(x, y, w, h)| format!("({:.0},{:.0},{:.0},{:.0})", x, y, w, h))
        .unwrap_or_else(|| "-".to_string());
    format!(
        "role={:<12} id={:<12} name={:?} value={:?} bounds={b}",
        a.role,
        el.id().unwrap_or_else(|| "-".to_string()),
        a.name.unwrap_or_default(),
        a.value.unwrap_or_default(),
    )
}

/// Does this name look like a spreadsheet cell reference (A1, BC12)?
fn looks_like_cell_ref(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 2 || s.len() > 8 {
        return false;
    }
    let mut letters = 0usize;
    let mut digits = 0usize;
    for c in s.chars() {
        if c.is_ascii_alphabetic() && digits == 0 {
            letters += 1;
        } else if c.is_ascii_digit() {
            digits += 1;
        } else {
            return false;
        }
    }
    letters >= 1 && digits >= 1
}

/// Walk the subtree, counting roles and collecting anything named like a cell.
fn census(
    el: &UIElement,
    depth: usize,
    max_depth: usize,
    counts: &mut std::collections::BTreeMap<String, usize>,
    cell_named: &mut Vec<(String, String)>,
    budget: &mut usize,
) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    let role = el.role();
    *counts.entry(role.clone()).or_insert(0) += 1;
    if let Some(name) = el.name() {
        if looks_like_cell_ref(&name) && cell_named.len() < 40 {
            cell_named.push((role, name));
        }
    }
    if depth >= max_depth {
        return;
    }
    if let Ok(children) = el.children() {
        for c in &children {
            census(c, depth + 1, max_depth, counts, cell_named, budget);
        }
    }
}

/// Print the whole subtree, indented. Small trees only -- Sheets' turned out to
/// be 56 nodes, which is itself the headline finding.
fn dump_tree(el: &UIElement, depth: usize, max_depth: usize, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    let a = el.attributes();
    let name = a.name.unwrap_or_default();
    let value = a.value.unwrap_or_default();
    let extra = if value.is_empty() {
        String::new()
    } else {
        format!("  value={value:?}")
    };
    println!(
        "  {:indent$}{} {:?}{extra}",
        "",
        a.role,
        name,
        indent = depth * 2
    );
    if depth >= max_depth {
        return;
    }
    if let Ok(children) = el.children() {
        for c in &children {
            dump_tree(c, depth + 1, max_depth, budget);
        }
    }
}

/// Every element that could plausibly carry "which cell am I on" -- the Name Box
/// input and the formula bar are both `Edit`s in Chromium's tree.
/// Scoped to the Sheets window on purpose: a desktop-wide search pulled in Edits
/// from every other browser window on the machine, including a second Sheets
/// document left over from an earlier run, which is exactly the sort of
/// contamination that makes a reading look meaningful when it is not.
async fn identity_carriers(desktop: &Desktop, window: &UIElement) -> Vec<UIElement> {
    let mut out = Vec::new();
    for sel in ["role:Edit", "role:ComboBox"] {
        if let Ok(all) = desktop
            .locator(sel)
            .within(window.clone())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            out.extend(all);
        }
    }
    out
}

fn describe_carriers(label: &str, carriers: &[UIElement]) {
    println!("  {label}");
    for (i, el) in carriers.iter().enumerate() {
        let a = el.attributes();
        println!(
            "    [{i}] role={:<9} name={:?} value={:?} text={:?}",
            a.role,
            a.name.unwrap_or_default(),
            a.value.unwrap_or_default(),
            el.text(0).unwrap_or_default(),
        );
    }
}

/// Print and return the focused element's identity: (id, name, bounds).
#[allow(clippy::type_complexity)]
fn focused_snapshot(
    desktop: &Desktop,
    label: &str,
) -> Option<(String, String, Option<(f64, f64, f64, f64)>)> {
    match desktop.focused_element() {
        Ok(el) => {
            println!("  {label:<18} {}", snap(&el));
            Some((
                el.id().unwrap_or_default(),
                el.name().unwrap_or_default(),
                el.bounds().ok(),
            ))
        }
        Err(e) => {
            println!("  {label:<18} Err {e}");
            None
        }
    }
}

async fn sheets_mode() -> ExitCode {
    println!("== what UIA sees inside a Google Sheets grid ==\n");
    println!("Opens sheets.new -- creates a blank 'Untitled spreadsheet' in the");
    println!("signed-in account's Drive. No existing document is read.\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- precondition: is a Sheets document actually open? ------------------
    // A login redirect or a slow load must read as INCONCLUSIVE, not as "no
    // cell elements found".
    let window = match desktop
        .locator("role:Window|name:Google Sheets")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    {
        Some(w) => w,
        None => {
            println!("\n  INCONCLUSIVE: no window titled '… Google Sheets' was found.");
            println!("  Most likely the account is not signed in, or the document did not");
            println!("  finish loading. Nothing below would be measuring Sheets, so the");
            println!("  probe stops rather than reporting findings about whatever else is");
            println!("  on screen.");
            return ExitCode::FAILURE;
        }
    };
    println!("\n  window: {:?}", window.name().unwrap_or_default());

    // ---- phase A: tree census ----------------------------------------------
    println!("\n================ A. what the tree contains ================\n");
    let mut counts = std::collections::BTreeMap::new();
    let mut cell_named = Vec::new();
    let mut budget = 6000usize;
    let started = std::time::Instant::now();
    census(&window, 0, 12, &mut counts, &mut cell_named, &mut budget);
    println!(
        "  walked {} nodes to depth 12 in {} ms\n",
        6000 - budget,
        started.elapsed().as_millis()
    );
    println!("  roles present:");
    let mut by_count: Vec<_> = counts.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1));
    for (role, n) in by_count.iter().take(20) {
        println!("    {n:>5}  {role}");
    }
    println!(
        "\n  elements whose NAME looks like a cell reference: {}",
        cell_named.len()
    );
    for (role, name) in cell_named.iter().take(15) {
        println!("    role={role:<12} name={name:?}");
    }
    if budget == 0 {
        println!("\n  NOTE: node budget exhausted -- the census is a sample, not a total.");
    }

    println!("\n  full tree (it is small enough to print in full):");
    let mut dump_budget = 200usize;
    dump_tree(&window, 0, 12, &mut dump_budget);

    // ---- phase B: does focus identity change per cell? ----------------------
    println!("\n================ B. focus identity across cell moves ================\n");
    println!("  If cells are real accessible elements, moving between them should");
    println!("  change the focused element's identity, name, or bounds.\n");

    let mut identities = Vec::new();
    if let Some(v) = focused_snapshot(&desktop, "start") {
        identities.push(("start", v));
    }
    for (label, key) in [
        ("after Right", "{Right}"),
        ("after Right", "{Right}"),
        ("after Down", "{Down}"),
        ("after Left", "{Left}"),
    ] {
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key(key);
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
        if let Some(v) = focused_snapshot(&desktop, label) {
            identities.push((label, v));
        }
    }

    let distinct_ids: std::collections::BTreeSet<_> =
        identities.iter().map(|(_, (id, _, _))| id.clone()).collect();
    let distinct_names: std::collections::BTreeSet<_> = identities
        .iter()
        .map(|(_, (_, name, _))| name.clone())
        .collect();
    let distinct_bounds: std::collections::BTreeSet<_> = identities
        .iter()
        .map(|(_, (_, _, b))| format!("{b:?}"))
        .collect();
    println!("\n  across {} positions:", identities.len());
    println!("    distinct focused-element ids   : {}", distinct_ids.len());
    println!("    distinct focused-element names : {}", distinct_names.len());
    println!("    distinct focused-element bounds: {}", distinct_bounds.len());

    // ---- phase C: the Name Box, and whether it lags -------------------------
    println!("\n================ C. the Name Box ================\n");
    let name_box = desktop
        .locator("role:ComboBox|name:Name box")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
        .or_else(|| None);
    let name_box = match name_box {
        Some(nb) => {
            println!("  found via role:ComboBox|name:Name box");
            Some(nb)
        }
        None => {
            // Try without the role, in case Sheets does not expose it as ComboBox.
            match desktop
                .locator("name:Name box")
                .within(desktop.root())
                .all(Some(Duration::from_secs(5)), None)
                .await
                .ok()
                .and_then(|all| all.into_iter().next())
            {
                Some(nb) => {
                    println!("  found via name:Name box (role is {:?})", nb.role());
                    Some(nb)
                }
                None => {
                    println!("  NOT FOUND by either selector. The Name Box hypothesis cannot");
                    println!("  be tested through this element if capture cannot see it either.");
                    None
                }
            }
        }
    };

    if let Some(nb) = &name_box {
        println!("  {}", snap(nb));
        println!("\n  its subtree -- the editable Name Box should be a child:");
        let mut nb_budget = 40usize;
        dump_tree(nb, 0, 4, &mut nb_budget);
    }

    // The Group above is a container. Whatever actually holds "B2" has to be an
    // Edit or ComboBox somewhere, so sample every one of them across moves.
    println!("\n  every Edit / ComboBox in the window, sampled across cell moves:");
    let carriers = identity_carriers(&desktop, &window).await;
    if carriers.is_empty() {
        println!("    none found -- there is no element of either role to carry cell identity");
    }
    describe_carriers("at current cell:", &carriers);
    for (label, key) in [("after Right", "{Right}"), ("after Down", "{Down}")] {
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key(key);
        }
        tokio::time::sleep(Duration::from_millis(900)).await;
        describe_carriers(&format!("{label}:"), &carriers);
    }

    // ---- the actual H2 test -------------------------------------------------
    // Whichever carrier reads like a cell reference IS the cell-identity signal.
    // The hypothesis is that it updates asynchronously, so capture can read it
    // before it has caught up. 900 ms sampling above is far too coarse to see
    // that. Press a key and sample hard.
    let cell_carrier = carriers.iter().find(|el| {
        let t = el.text(0).unwrap_or_default();
        looks_like_cell_ref(&t)
    });
    match cell_carrier {
        None => {
            println!("\n  no carrier reads like a cell reference, so there is no Name Box");
            println!("  signal to race against and H2 cannot be tested this way.");
        }
        Some(cc) => {
            println!("\n  H2: how fast does the cell reference update after a move?");
            let before = cc.text(0).unwrap_or_default();
            println!("    before keypress            {before:?}");
            if let Ok(el) = desktop.focused_element() {
                let _ = el.press_key("{Right}");
            }
            let t0 = std::time::Instant::now();
            let mut first_change: Option<u128> = None;
            for _ in 0..40 {
                let now = cc.text(0).unwrap_or_default();
                let elapsed = t0.elapsed().as_millis();
                if now != before && first_change.is_none() {
                    first_change = Some(elapsed);
                    println!("    CHANGED at +{elapsed} ms          {now:?}");
                }
                if elapsed > 1500 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let settled = cc.text(0).unwrap_or_default();
            println!("    settled                    {settled:?}");
            match first_change {
                Some(ms) => println!(
                    "    -> the cell reference lagged the keypress by ~{ms} ms (read every 25 ms)"
                ),
                None => println!(
                    "    -> it never changed within 1.5 s. Either the move did not happen, \
                     or this element does not track the cursor."
                ),
            }
        }
    }

    // ---- phase D: typing race ----------------------------------------------
    println!("\n================ D. typing into a cell ================\n");
    println!("  Types 4 characters, then samples what UIA reports over time.\n");
    if let Ok(el) = desktop.focused_element() {
        println!("  before typing      {}", snap(&el));
        let t0 = std::time::Instant::now();
        let _ = el.type_text("7391", false);
        println!("  typed in {} ms", t0.elapsed().as_millis());

        let t1 = std::time::Instant::now();
        for delay in [0u64, 100, 300, 800, 1500] {
            while (t1.elapsed().as_millis() as u64) < delay {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let f = desktop.focused_element();
            match f {
                Ok(cur) => println!("    +{:<5}ms  focused {}", t1.elapsed().as_millis(), snap(&cur)),
                Err(e) => println!("    +{:<5}ms  Err {e}", t1.elapsed().as_millis()),
            }
            describe_carriers("               carriers:", &carriers);
        }

        // Commit and see what the cell reports afterwards.
        if let Ok(cur) = desktop.focused_element() {
            let _ = cur.press_key("{Enter}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(cur) = desktop.focused_element() {
            println!("\n  after Enter        {}", snap(&cur));
        }
        describe_carriers("after Enter carriers:", &carriers);

        // ---- premise check --------------------------------------------------
        // Everything in this phase is meaningless if the keystrokes never
        // reached the sheet. Go back to the cell and ask the formula bar what
        // it holds. If nothing anywhere reports "7391", the typing is
        // unverified and phase D decides nothing.
        if let Ok(cur) = desktop.focused_element() {
            let _ = cur.press_key("{Up}");
        }
        tokio::time::sleep(Duration::from_millis(900)).await;
        let after = identity_carriers(&desktop, &window).await;
        describe_carriers("back on the typed cell:", &after);

        let landed = after.iter().any(|el| {
            let a = el.attributes();
            a.value.unwrap_or_default().contains("7391")
                || el.text(0).unwrap_or_default().contains("7391")
        });
        println!("\n  did '7391' reach anything UIA can read: {landed}");
        if !landed {
            println!("  PREMISE UNVERIFIED for phase D: no readable element reports the");
            println!("  typed text, so this phase cannot distinguish 'typing did not");
            println!("  happen' from 'typing happened and is invisible to UIA'. Both are");
            println!("  consistent with the output above, and they are different findings.");
        }
    }

    println!("\n--- note ---");
    println!("  A blank 'Untitled spreadsheet' now exists in the signed-in Drive");
    println!("  account. This probe does not delete it.");

    ExitCode::SUCCESS
}

// ----------------------------------------------------- sheetstrash mode ----
// Cleanup for the throwaway spreadsheets the `sheets` probe creates.
//
// This is a destructive action against a real Google account, so it is built to
// be incapable of touching the wrong document: it navigates by exact document
// ID and refuses to act unless the window's own address bar contains that ID.
// A window that does not match is skipped and reported, never guessed at.
//
// "Move to trash" is deliberately the action rather than permanent deletion --
// Drive keeps trashed items for 30 days, so a mistake here is recoverable.

/// Is this open document in the trash?
///
/// The exact strings matter and were measured, not guessed. A trashed document
/// shows the Text "File is in trash" and a Button "Take out of trash", and has
/// no File menu. An earlier version of this check looked for "in the trash" and
/// a "Restore" button -- neither of which Sheets uses -- and so reported
/// confirmed-trashed documents as untrashed.
async fn is_trashed(desktop: &Desktop, window: &UIElement) -> (bool, String) {
    let button = find_named(desktop, window, &["role:Button"], |n| {
        n.to_lowercase().contains("take out of trash")
    })
    .await;
    let banner = find_named(desktop, window, &["role:Text"], |n| {
        let n = n.to_lowercase();
        n.contains("is in trash") || n.contains("in the trash")
    })
    .await;
    let evidence = banner
        .and_then(|b| b.name())
        .or_else(|| button.as_ref().and_then(|b| b.name()))
        .unwrap_or_default();
    (button.is_some() || !evidence.is_empty(), evidence)
}

/// Find the open Sheets window whose address bar contains `id`.
async fn window_for_doc(desktop: &Desktop, id: &str) -> Option<UIElement> {
    let windows = desktop
        .locator("role:Window|name:Google Sheets")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()?;
    for w in windows {
        if let Ok(bars) = desktop
            .locator("role:Edit|name:Address and search bar")
            .within(w.clone())
            .all(Some(Duration::from_secs(4)), None)
            .await
        {
            for bar in &bars {
                if bar.text(0).unwrap_or_default().contains(id) {
                    return Some(w);
                }
            }
        }
    }
    None
}

/// First element inside `window` with the given role whose name matches.
async fn find_named(
    desktop: &Desktop,
    window: &UIElement,
    roles: &[&str],
    pred: impl Fn(&str) -> bool,
) -> Option<UIElement> {
    for role in roles {
        if let Ok(all) = desktop
            .locator(*role)
            .within(window.clone())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            for el in all {
                if pred(&el.name().unwrap_or_default()) {
                    return Some(el);
                }
            }
        }
    }
    None
}

async fn sheetstrash_mode() -> ExitCode {
    let ids: Vec<String> = std::env::args()
        .filter(|a| {
            a.len() >= 40
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .collect();
    if ids.is_empty() {
        eprintln!("pass one or more Google Drive document IDs as arguments");
        return ExitCode::FAILURE;
    }

    println!("== move throwaway spreadsheets to trash ==\n");
    println!("Acts only on a window whose address bar contains the exact target ID.");
    println!("Uses 'Move to trash', which Drive keeps recoverable for 30 days.\n");
    println!("targets:");
    for id in &ids {
        println!("  {id}");
    }

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let browser = browser_order()[0];

    let mut results: Vec<(String, String)> = Vec::new();
    for id in &ids {
        println!("\n================ {id} ================");
        let url = format!("https://docs.google.com/spreadsheets/d/{id}/edit");
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", &url])
            .spawn()
        {
            let _ = c.wait();
        }
        tokio::time::sleep(Duration::from_secs(22)).await;

        let Some(window) = window_for_doc(&desktop, id).await else {
            println!("  SKIPPED: no open window's address bar contains this ID.");
            results.push((id.clone(), "skipped (window not found)".into()));
            continue;
        };
        let title = window.name().unwrap_or_default();
        println!("  window: {title:?}");
        if !title.contains("Untitled spreadsheet") {
            println!("  SKIPPED: title is not 'Untitled spreadsheet'. Refusing to trash a");
            println!("  document that is not one of the blank throwaways.");
            results.push((id.clone(), format!("skipped (title {title:?})")));
            continue;
        }

        // Already trashed? A trashed document opens with a banner and a Restore
        // button. Checking first makes this idempotent, so a partial run can be
        // finished without re-acting on documents that are already done -- and
        // it is how "clicked, but no confirmation seen" gets resolved into a
        // fact rather than left as a guess.
        let (already, evidence) = is_trashed(&desktop, &window).await;
        if already {
            println!("  ALREADY TRASHED -- {evidence:?}");
            results.push((id.clone(), "already in trash (verified)".into()));
            let _ = window.close();
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        println!("  not trashed yet");

        let Some(file_menu) =
            find_named(&desktop, &window, &["role:MenuItem", "role:Button"], |n| n == "File").await
        else {
            println!("  could not locate the File menu; leaving this document alone.");
            results.push((id.clone(), "failed (no File menu)".into()));
            continue;
        };
        println!("  found File menu (role={})", file_menu.role());

        // The dropdown only opens if the window is actually foreground -- a click
        // into a background window activates it and is swallowed. Measured: the
        // first document worked because its window happened to be frontmost, and
        // three later ones failed with only the nine menu-bar items visible.
        let mut trash_item = None;
        for attempt in 1..=3 {
            let _ = window.activate_window();
            tokio::time::sleep(Duration::from_millis(800)).await;
            robust_click(&desktop, &file_menu);
            tokio::time::sleep(Duration::from_millis(2500)).await;

            trash_item = find_named(&desktop, &window, &["role:MenuItem"], |n| {
                let n = n.to_lowercase();
                n.contains("move to trash") || n.contains("move to bin")
            })
            .await;
            if trash_item.is_some() {
                println!("  menu opened on attempt {attempt}");
                break;
            }
            println!("  attempt {attempt}: dropdown did not open");
        }
        let Some(item) = trash_item else {
            println!("  File menu opened but no 'Move to trash' item was found.");
            if let Ok(items) = desktop
                .locator("role:MenuItem")
                .within(window.clone())
                .all(Some(Duration::from_secs(5)), None)
                .await
            {
                println!("  menu items visible ({}):", items.len());
                for el in items.iter().take(30) {
                    println!("    {:?}", el.name().unwrap_or_default());
                }
            }
            results.push((id.clone(), "failed (no trash item)".into()));
            continue;
        };
        // The item is named "Move to trash t" -- the trailing letter is its
        // keyboard accelerator. Coordinate-clicking it activated the item on
        // three documents and silently did nothing on two others, so prefer the
        // keystroke, which does not depend on hit-testing a menu popup.
        let iname = item.name().unwrap_or_default();
        let accel = iname.rsplit(' ').next().unwrap_or("").to_string();
        if accel.chars().count() == 1 {
            println!("  activating {iname:?} via accelerator {accel:?}");
            if let Ok(f) = desktop.focused_element() {
                let _ = f.press_key(&accel);
            }
        } else {
            println!("  clicking {iname:?}");
            robust_click(&desktop, &item);
        }
        tokio::time::sleep(Duration::from_secs(4)).await;

        // Verify by RELOADING, not by catching the toast. The "File moved to
        // trash" toast fades within a few seconds, so its absence proved
        // nothing and produced two false "UNCONFIRMED" results earlier. A fresh
        // load either shows the trash banner or it does not.
        let _ = window.close();
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", &url])
            .spawn()
        {
            let _ = c.wait();
        }
        tokio::time::sleep(Duration::from_secs(20)).await;

        match window_for_doc(&desktop, id).await {
            Some(w2) => {
                let (trashed, evidence) = is_trashed(&desktop, &w2).await;
                println!("  on reload: trashed={trashed} {evidence:?}");
                results.push((
                    id.clone(),
                    if trashed {
                        format!("TRASHED (verified on reload: {evidence:?})")
                    } else {
                        "STILL PRESENT -- the click did not take".into()
                    },
                ));
                let _ = w2.close();
            }
            None => {
                println!("  could not reopen to verify");
                results.push((id.clone(), "UNVERIFIED (could not reopen)".into()));
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    println!("\n================ RESULT ================\n");
    for (id, outcome) in &results {
        println!("  {id}  {outcome}");
    }

    // Close anything still open from the investigation.
    println!("\n--- closing remaining Sheets windows ---");
    if let Ok(all) = desktop
        .locator("role:Window|name:Google Sheets")
        .within(desktop.root())
        .all(Some(Duration::from_secs(6)), None)
        .await
    {
        let mut closed = 0;
        for el in &all {
            let name = el.name().unwrap_or_default();
            if name.contains("more pages") {
                println!("  SPARED (shared multi-tab window) {name:?}");
                continue;
            }
            if el.close().is_ok() {
                closed += 1;
                println!("  closed {name:?}");
            }
        }
        println!("  {closed} window(s) closed");
    } else {
        println!("  none found");
    }

    ExitCode::SUCCESS
}

// ------------------------------------------------------ sheetsa11y mode ----
// The one untested variable from the Sheets investigation: does Google Sheets'
// screen-reader support mode materialise a real per-cell accessibility tree?
//
// Measured BEFORE and AFTER in the SAME document, so the comparison controls for
// document, window, machine and browser state. Comparing against the numbers
// from the earlier session would not.
//
// The trap this has to avoid: if the toggle silently fails, "the tree did not
// change" looks identical to "screen reader mode does not change the tree", and
// they are opposite findings. So the toggle needs confirmation that does not
// come from the thing being measured -- Sheets' own on-screen announcement.

struct GridSnapshot {
    nodes: usize,
    roles: std::collections::BTreeMap<String, usize>,
    cell_named: Vec<(String, String)>,
    focus_ids: std::collections::BTreeSet<String>,
    focus_names: std::collections::BTreeSet<String>,
    focus_lines: Vec<String>,
}

async fn measure_grid(desktop: &Desktop, window: &UIElement, label: &str) -> GridSnapshot {
    println!("\n---------------- {label} ----------------");
    let mut roles = std::collections::BTreeMap::new();
    let mut cell_named = Vec::new();
    let mut budget = 8000usize;
    census(window, 0, 14, &mut roles, &mut cell_named, &mut budget);
    let nodes = 8000 - budget;
    println!("  nodes walked (depth 14): {nodes}");
    let mut by_count: Vec<_> = roles.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1));
    println!("  roles:");
    for (role, n) in by_count.iter().take(14) {
        println!("    {n:>5}  {role}");
    }
    // The roles a real grid would have to use.
    for role in ["DataItem", "Table", "Grid", "Cell", "DataGrid", "ListItem", "Custom"] {
        println!("    grid-role {role:<9}: {}", roles.get(role).copied().unwrap_or(0));
    }
    println!("  names that look like cell references: {}", cell_named.len());
    for (role, name) in cell_named.iter().take(10) {
        println!("    role={role:<10} name={name:?}");
    }

    let mut focus_ids = std::collections::BTreeSet::new();
    let mut focus_names = std::collections::BTreeSet::new();
    let mut focus_lines = Vec::new();
    println!("  focused element across cell moves:");
    for (i, key) in [None, Some("{Right}"), Some("{Right}"), Some("{Down}")]
        .into_iter()
        .enumerate()
    {
        if let Some(k) = key {
            if let Ok(el) = desktop.focused_element() {
                let _ = el.press_key(k);
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        if let Ok(el) = desktop.focused_element() {
            let line = snap(&el);
            println!("    [{i}] {line}");
            focus_ids.insert(el.id().unwrap_or_default());
            focus_names.insert(el.name().unwrap_or_default());
            focus_lines.push(line);
        }
    }
    println!(
        "  distinct focused ids={} names={}",
        focus_ids.len(),
        focus_names.len()
    );

    GridSnapshot {
        nodes,
        roles,
        cell_named,
        focus_ids,
        focus_names,
        focus_lines,
    }
}

async fn sheetsa11y_mode() -> ExitCode {
    println!("== does Sheets' screen-reader mode materialise real cells? ==\n");
    println!("Measures the same document before and after toggling the mode.\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("\n  INCONCLUSIVE: no 'Untitled spreadsheet' window found. Not signed in,");
        println!("  or the document did not load. Nothing below would be measuring Sheets.");
        return ExitCode::FAILURE;
    };
    println!("  window: {:?}", window.name().unwrap_or_default());

    // Print the document id so it can be cleaned up afterwards.
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                println!("  DOCUMENT ID: {}", rest.split('/').next().unwrap_or(""));
            }
        }
    }

    let before = measure_grid(&desktop, &window, "BEFORE: screen reader mode OFF").await;

    // ---- toggle, and prove it took ------------------------------------------
    println!("\n================ enabling screen reader support ================\n");
    println!("  sending {{ctrl}}{{alt}}z");
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{alt}z");
    }

    // Sheets announces this on screen. That announcement is the independent
    // confirmation -- it is not part of the grid tree being measured.
    let mut confirmation = String::new();
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(8) {
        if let Ok(texts) = desktop
            .locator("role:Text")
            .within(window.clone())
            .all(Some(Duration::from_secs(2)), None)
            .await
        {
            for t in &texts {
                let n = t.name().unwrap_or_default();
                let low = n.to_lowercase();
                if low.contains("screen reader") || low.contains("braille") {
                    confirmation = n;
                    break;
                }
            }
        }
        if !confirmation.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    if confirmation.is_empty() {
        println!("  NO CONFIRMATION SEEN within 8s.");
    } else {
        println!("  confirmed by Sheets: {confirmation:?}  (after {} ms)", t0.elapsed().as_millis());
    }

    println!("  waiting 8s for the tree to rebuild...");
    tokio::time::sleep(Duration::from_secs(8)).await;

    let after = measure_grid(&desktop, &window, "AFTER: screen reader mode ON").await;

    // ---- verdict ------------------------------------------------------------
    println!("\n================ VERDICT ================\n");
    println!("  {:<34} {:>8} {:>8}", "measure", "before", "after");
    println!("  {:<34} {:>8} {:>8}", "accessible nodes", before.nodes, after.nodes);
    for role in ["DataItem", "Table", "Grid", "Cell", "DataGrid", "ListItem", "Custom", "Edit"] {
        println!(
            "  {:<34} {:>8} {:>8}",
            format!("role {role}"),
            before.roles.get(role).copied().unwrap_or(0),
            after.roles.get(role).copied().unwrap_or(0)
        );
    }
    println!(
        "  {:<34} {:>8} {:>8}",
        "cell-reference-looking names",
        before.cell_named.len(),
        after.cell_named.len()
    );
    println!(
        "  {:<34} {:>8} {:>8}",
        "distinct focused ids over 4 cells",
        before.focus_ids.len(),
        after.focus_ids.len()
    );
    println!(
        "  {:<34} {:>8} {:>8}",
        "distinct focused names over 4 cells",
        before.focus_names.len(),
        after.focus_names.len()
    );

    let materialised = after.focus_ids.len() > before.focus_ids.len()
        || after.focus_names.len() > before.focus_names.len()
        || after.cell_named.len() > before.cell_named.len()
        || ["DataItem", "Table", "Grid", "Cell", "DataGrid"].iter().any(|r| {
            after.roles.get(*r).copied().unwrap_or(0) > before.roles.get(*r).copied().unwrap_or(0)
        });

    println!();
    if confirmation.is_empty() {
        println!("  INCONCLUSIVE. The toggle was never confirmed, so an unchanged tree");
        println!("  cannot be told apart from a toggle that did not take. These are");
        println!("  opposite findings and this run does not distinguish them.");
    } else if materialised {
        println!("  SCREEN READER MODE CHANGES THE TREE. Cells, or per-cell focus, appear");
        println!("  that were not there before. Targeting individual cells may be possible");
        println!("  with the mode enabled -- see the numbers above for what exactly changed.");
    } else {
        println!("  NO CHANGE. Screen reader support is confirmed on, and the grid is still");
        println!("  absent from the accessibility tree: same node count, no grid roles, no");
        println!("  cell-reference names, and focus still does not move between cells.");
        println!("  Canvas rendering persists regardless of the accessibility setting.");
    }

    println!("\n  focus detail after (verbatim):");
    for line in &after.focus_lines {
        println!("    {line}");
    }

    // ---- restore the account setting ----------------------------------------
    // Screen reader support is a persistent per-account Docs preference. Leave it
    // as it was found.
    println!("\n--- restoring the setting ---");
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{alt}z");
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut off_confirm = String::new();
    if let Ok(texts) = desktop
        .locator("role:Text")
        .within(window.clone())
        .all(Some(Duration::from_secs(3)), None)
        .await
    {
        for t in &texts {
            let n = t.name().unwrap_or_default();
            if n.to_lowercase().contains("screen reader") {
                off_confirm = n;
                break;
            }
        }
    }
    println!("  toggled back off; Sheets says {off_confirm:?}");

    ExitCode::SUCCESS
}

// ----------------------------------------------------- sheetsstate mode ----
// Read-only. Reports what a document's page actually looks like, so "is it in
// the trash" can be answered from evidence instead of from a toast that may
// already have faded. Touches nothing.
async fn sheetsstate_mode() -> ExitCode {
    let ids: Vec<String> = std::env::args()
        .filter(|a| {
            a.len() >= 40
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .collect();
    if ids.is_empty() {
        eprintln!("pass one or more document IDs");
        return ExitCode::FAILURE;
    }

    println!("== document state (read-only) ==\n");
    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let browser = browser_order()[0];

    for id in &ids {
        println!("\n================ {id} ================");
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args([
                "/C",
                "start",
                "",
                browser,
                "--new-window",
                &format!("https://docs.google.com/spreadsheets/d/{id}/edit"),
            ])
            .spawn()
        {
            let _ = c.wait();
        }
        tokio::time::sleep(Duration::from_secs(20)).await;

        let Some(window) = window_for_doc(&desktop, id).await else {
            println!("  window not found");
            continue;
        };
        println!("  title: {:?}", window.name().unwrap_or_default());

        let has_file_menu = find_named(&desktop, &window, &["role:MenuItem"], |n| n == "File")
            .await
            .is_some();
        println!("  has File menu: {has_file_menu}");

        if let Ok(texts) = desktop
            .locator("role:Text")
            .within(window.clone())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            println!("  Text elements ({}):", texts.len());
            for t in texts.iter().take(20) {
                let n = t.name().unwrap_or_default();
                if !n.trim().is_empty() {
                    println!("    {n:?}");
                }
            }
        }
        if let Ok(btns) = desktop
            .locator("role:Button")
            .within(window.clone())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            let names: Vec<String> = btns
                .iter()
                .filter_map(|b| b.name())
                .filter(|n| !n.trim().is_empty())
                .take(25)
                .collect();
            println!("  Buttons: {names:?}");
        }
        let _ = window.close();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    ExitCode::SUCCESS
}

// ------------------------------------------------------ sheetsedit mode ----
// Part 2: can capture read the transient cell editor while it exists?
//
// Established: Sheets has no persistent cell elements. Typing creates a
// ComboBox overlay named after the cell, carrying the typed text, destroyed on
// commit. So the design question is not "how do we read a cell" but "can the
// editor be observed during its lifetime, and is what it reports correct".
//
// Four things get measured, in order of how much they decide:
//
//   1. LIFECYCLE   -- when the editor appears and dies, sampled at 25 ms, so we
//                     know whether there is a window to read in at all.
//   2. PIPELINE    -- what the REAL CaptureSession records for the same edits.
//                     This is the actual question: not "is the data reachable"
//                     but "does the existing event-driven capture see it".
//   3. GROUND TRUTH-- what the spreadsheet actually saved, via Sheets' own CSV
//                     export, not via the UI that produced the reading.
//   4. Z3          -- the reproducible position artifact.

/// Strip the U+FEFF that Sheets seeds its editor with, plus trailing newline.
fn clean_cell_text(raw: &str) -> String {
    raw.replace('\u{feff}', "").trim_end_matches('\n').to_string()
}

/// Newest .csv in the Downloads folder, with its modified time.
fn newest_csv() -> Option<(std::path::PathBuf, std::time::SystemTime)> {
    let dir = dirs_downloads()?;
    let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("csv") {
            continue;
        }
        let m = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(_, bm)| m > *bm).unwrap_or(true) {
            best = Some((p, m));
        }
    }
    best
}

fn dirs_downloads() -> Option<std::path::PathBuf> {
    std::env::var("USERPROFILE")
        .ok()
        .map(|p| std::path::PathBuf::from(p).join("Downloads"))
}

async fn sheetsedit_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};

    println!("== capturing Sheets cell edits from the transient editor ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("\n  INCONCLUSIVE: no 'Untitled spreadsheet' window. Not signed in, or the");
        println!("  document did not load.");
        return ExitCode::FAILURE;
    };
    println!("  window: {:?}", window.name().unwrap_or_default());
    let mut doc_id = String::new();
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                doc_id = rest.split('/').next().unwrap_or("").to_string();
            }
        }
    }
    println!("  DOCUMENT ID: {doc_id}");

    // ---- 1. lifecycle -------------------------------------------------------
    println!("\n================ 1. editor lifecycle (25 ms sampling) ================\n");
    println!("  typing '111' into the current cell, then Enter\n");

    let mut timeline: Vec<(u128, String, String, String)> = Vec::new();
    let mut record = |t: u128, desktop: &Desktop, timeline: &mut Vec<_>| {
        if let Ok(el) = desktop.focused_element() {
            let role = el.role();
            let name = el.name().unwrap_or_default();
            let text = el.text(0).unwrap_or_default();
            let last_differs = timeline
                .last()
                .map(|(_, r, n, x): &(u128, String, String, String)| {
                    *r != role || *n != name || *x != text
                })
                .unwrap_or(true);
            if last_differs {
                timeline.push((t, role, name, text));
            }
        }
    };

    let t0 = std::time::Instant::now();
    record(0, &desktop, &mut timeline);
    if let Ok(el) = desktop.focused_element() {
        let _ = el.type_text("111", false);
    }
    while t0.elapsed() < Duration::from_millis(1200) {
        record(t0.elapsed().as_millis(), &desktop, &mut timeline);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{Enter}");
    }
    let commit_at = t0.elapsed().as_millis();
    while t0.elapsed() < Duration::from_millis(3000) {
        record(t0.elapsed().as_millis(), &desktop, &mut timeline);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    println!("  state transitions (Enter sent at +{commit_at} ms):");
    for (t, role, name, text) in &timeline {
        println!("    +{t:<5}ms  role={role:<10} name={name:?} text={text:?}");
    }
    let editor_alive: Vec<&(u128, String, String, String)> = timeline
        .iter()
        .filter(|(_, r, n, _)| r == "ComboBox" && looks_like_cell_ref(n))
        .collect();
    println!(
        "\n  editor observed in {} of {} transitions",
        editor_alive.len(),
        timeline.len()
    );
    if let (Some(first), Some(last)) = (editor_alive.first(), editor_alive.last()) {
        println!("  editor first seen +{} ms, last seen +{} ms", first.0, last.0);
        println!("  -> a read window of ~{} ms exists", last.0.saturating_sub(first.0));
    } else {
        println!("  editor NEVER observed as a focused ComboBox -- the read window this");
        println!("  design depends on was not seen in this run.");
    }

    // ---- 2. what the real pipeline records ----------------------------------
    println!("\n================ 2. what CaptureSession actually records ================\n");
    let intended = [("A1", "alpha"), ("A2", "beta"), ("A3", "gamma")];
    println!("  driving three edits, then reading back what capture produced");

    let session = match CaptureSession::start_session(
        "sheetsedit",
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

    // Ctrl+Home to A1, then type down the column.
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{home}");
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
    let mut observed_at_edit: Vec<(String, String)> = Vec::new();
    for (cell, value) in intended {
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text(value, false);
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
        // Read the editor at the moment it is alive -- the proposed mechanism.
        if let Ok(el) = desktop.focused_element() {
            let n = el.name().unwrap_or_default();
            let raw = el.text(0).unwrap_or_default();
            if el.role() == "ComboBox" && looks_like_cell_ref(&n) {
                observed_at_edit.push((n.clone(), clean_cell_text(&raw)));
                println!("    at edit of {cell}: editor name={n:?} text={:?}", clean_cell_text(&raw));
            } else {
                println!("    at edit of {cell}: focus is role={} name={n:?}", el.role());
            }
        }
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key("{Enter}");
        }
        tokio::time::sleep(Duration::from_millis(900)).await;
    }

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "\n  capture produced {} action(s), {} unmapped event(s)",
        report.actions.len(),
        report.unmapped_events
    );
    for a in &report.actions {
        println!(
            "    {:<9} role={:?} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
    }

    println!("\n  read directly from the editor at edit time (the proposed mechanism):");
    for (cell, text) in &observed_at_edit {
        println!("    {cell} = {text:?}");
    }
    let direct_ok = intended
        .iter()
        .zip(observed_at_edit.iter())
        .filter(|((_, want), (_, got))| want == got)
        .count();
    println!(
        "  direct reads matching intent: {}/{}",
        direct_ok,
        intended.len()
    );

    // ---- 3. ground truth ----------------------------------------------------
    println!("\n================ 3. did it actually commit? (CSV export) ================\n");
    let before_csv = newest_csv().map(|(p, _)| p);
    let export = format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv&gid=0");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, &export])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("  requested {export}");
    let mut found: Option<std::path::PathBuf> = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before_csv.as_ref() {
                found = Some(p);
                break;
            }
        }
    }
    match found {
        Some(path) => {
            println!("  downloaded {}", path.display());
            match std::fs::read_to_string(&path) {
                Ok(body) => {
                    println!("  --- saved spreadsheet contents ---");
                    for line in body.lines().take(10) {
                        println!("    {line:?}");
                    }
                    let mut committed = 0;
                    for (_, v) in intended {
                        if body.contains(v) {
                            committed += 1;
                        }
                    }
                    println!(
                        "  values present in the SAVED file: {committed}/{}",
                        intended.len()
                    );
                    if body.contains("111") {
                        println!("  the lifecycle test's '111' is also present");
                    }
                }
                Err(e) => println!("  could not read it: {e}"),
            }
        }
        None => println!("  no new CSV appeared -- commit could not be verified this way"),
    }

    // ---- 4. the Z3 artifact -------------------------------------------------
    println!("\n================ 4. the Z3 artifact ================\n");
    if let Ok(all) = desktop
        .locator("role:ComboBox")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        println!("  ComboBoxes in the window: {}", all.len());
        for el in &all {
            let n = el.name().unwrap_or_default();
            println!(
                "    id={:<10} name={n:?} text={:?} bounds={:?}",
                el.id().unwrap_or_default(),
                el.text(0).unwrap_or_default(),
                el.bounds().ok().map(|(x, y, w, h)| (x as i64, y as i64, w as i64, h as i64))
            );
        }
    }

    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ----------------------------------------------------- sheetswatch mode ----
// Prototype of the proposed capture mechanism, validated against ground truth.
//
// The rule under test, in full:
//
//   while focus is on a ComboBox whose name parses as a cell reference,
//   remember (name, text); when that stops being true, emit one Type action
//   carrying the LAST remembered pair, with U+FEFF stripped.
//
// "Last remembered before it stops being true" is the whole design. Reading
// after the editor is recycled yields U+FEFF and a different cell, which is
// precisely the corruption the original recordings showed.
//
// Cells are navigated via the Name Box rather than arrow keys, so the target
// cell is deterministic and the check is against a known intent.
async fn sheetswatch_mode() -> ExitCode {
    println!("== prototype: derive cell edits from the transient editor ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    let mut doc_id = String::new();
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                doc_id = rest.split('/').next().unwrap_or("").to_string();
            }
        }
    }
    println!("  DOCUMENT ID: {doc_id}");

    // The Name Box input is the Edit child of the "Name box" group.
    let name_box_input = match desktop
        .locator("name:Name box")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    {
        Some(group) => group.children().ok().and_then(|c| {
            c.into_iter().find(|e| e.role() == "Edit")
        }),
        None => None,
    };
    if name_box_input.is_none() {
        println!("  could not find the Name Box input; cannot target cells deterministically.");
        return ExitCode::FAILURE;
    }
    let name_box_input = name_box_input.unwrap();

    let plan = [("B2", "apple"), ("D5", "banana"), ("C9", "cherry")];
    let mut derived: Vec<(String, String)> = Vec::new();

    for (cell, value) in plan {
        println!("\n---- intent: {cell} = {value:?} ----");
        // Navigate deterministically.
        let _ = name_box_input.type_text(cell, true);
        tokio::time::sleep(Duration::from_millis(400)).await;
        // CLEAN Enter. `press_key("{Enter}")` injects {LEFT} then {END} first as
        // a browser-autocomplete workaround, and {END} relocates the cursor in a
        // grid -- which corrupted the previous run of this experiment.
        // `type_text` goes through send_text and adds nothing.
        let _ = name_box_input.type_text("\n", false);
        tokio::time::sleep(Duration::from_millis(1200)).await;

        // Type, then run the watcher over the editor's lifetime.
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text(value, false);
        }
        let mut last_seen: Option<(String, String)> = None;
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_millis(900) {
            if let Ok(el) = desktop.focused_element() {
                let n = el.name().unwrap_or_default();
                if el.role() == "ComboBox" && looks_like_cell_ref(&n) {
                    let text = clean_cell_text(&el.text(0).unwrap_or_default());
                    if !text.is_empty() {
                        last_seen = Some((n, text));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // Commit with a clean Enter, for the same reason as above. Everything
        // after this point reads as U+FEFF at a stale cell, which is why the
        // emitted value is the one remembered before this line.
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text("\n", false);
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let after = desktop
            .focused_element()
            .ok()
            .map(|el| {
                (
                    el.name().unwrap_or_default(),
                    el.text(0).unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        match &last_seen {
            Some((c, v)) => {
                println!("  watcher emitted: {c} = {v:?}");
                derived.push((c.clone(), v.clone()));
            }
            None => println!("  watcher emitted NOTHING -- editor never observed"),
        }
        println!("  (post-commit read would have been: {:?})", after);
    }

    // ---- ground truth -------------------------------------------------------
    println!("\n================ ground truth (CSV export) ================\n");
    let before_csv = newest_csv().map(|(p, _)| p);
    let export = format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, &export])
        .spawn()
    {
        let _ = c.wait();
    }
    let mut found = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before_csv.as_ref() {
                found = Some(p);
                break;
            }
        }
    }
    let mut saved = String::new();
    match found {
        Some(path) => {
            println!("  downloaded {}", path.display());
            saved = std::fs::read_to_string(&path).unwrap_or_default();
            for (i, line) in saved.lines().enumerate().take(12) {
                println!("    row {:<3} {line:?}", i + 1);
            }
        }
        None => println!("  no CSV appeared; cannot verify against saved state"),
    }

    println!("\n================ VERDICT ================\n");
    println!("  {:<10} {:<10} {:<12} {}", "intent", "value", "watcher saw", "in saved file");
    let mut right_cell = 0;
    let mut value_saved = 0;
    for (i, (cell, value)) in plan.iter().enumerate() {
        let got = derived.get(i);
        let got_cell = got.map(|(c, _)| c.clone()).unwrap_or_else(|| "-".into());
        let got_val = got.map(|(_, v)| v.clone()).unwrap_or_else(|| "-".into());
        let in_file = saved.contains(value);
        if got_cell == *cell {
            right_cell += 1;
        }
        if in_file {
            value_saved += 1;
        }
        println!(
            "  {:<10} {:<10} {:<12} {}",
            cell,
            value,
            format!("{got_cell}={got_val}"),
            in_file
        );
    }
    println!(
        "\n  watcher reported the intended cell: {right_cell}/{}",
        plan.len()
    );
    println!("  value present in saved file       : {value_saved}/{}", plan.len());
    println!("\n  DOCUMENT ID for cleanup: {doc_id}");

    ExitCode::SUCCESS
}

// ----------------------------------------------------- sheetsclean mode ----
// The unconfounded re-run of the editor-watcher experiment.
//
// Two earlier attempts were spoiled by how Enter gets sent:
//   * press_key("{Enter}") injects {LEFT}{END} first, and {END} relocates the
//     cursor in a grid, so the cell under edit moved before every commit.
//   * type_text("\n") adds nothing, but does not commit either -- the editor
//     stayed open and accumulated "apple\nbanana\ncherry" while the saved file
//     stayed empty.
//
// Tab commits a cell edit and moves one column right, and "{Tab}" contains
// neither ENTER nor RETURN, so press_key sends it verbatim. That gives a commit
// with no injected keystrokes.
//
// Which cells get used no longer matters: the test is whether the cell the
// watcher REPORTS is the cell the value actually landed in, checked against the
// exported CSV by parsing the reference into row and column.

/// "B2" -> (col 2, row 2). 1-based.
fn parse_cell_ref(s: &str) -> Option<(usize, usize)> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let mut col = 0usize;
    for c in letters.chars() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    Some((col, digits.parse().ok()?))
}

/// Value at (col, row) of a CSV, 1-based. Naive split: probe values have no commas.
fn csv_at(body: &str, col: usize, row: usize) -> Option<String> {
    let line = body.lines().nth(row.checked_sub(1)?)?;
    let field = line.split(',').nth(col.checked_sub(1)?)?;
    Some(field.trim_matches('"').to_string())
}

async fn sheetsclean_mode() -> ExitCode {
    println!("== editor watcher, committing with Tab (no injected keystrokes) ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    let mut doc_id = String::new();
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                doc_id = rest.split('/').next().unwrap_or("").to_string();
            }
        }
    }
    println!("  DOCUMENT ID: {doc_id}");

    // Ctrl+Home is clean -- no ENTER substring, so no preamble.
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{home}");
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let values = ["apple", "banana", "cherry"];
    let mut derived: Vec<(String, String)> = Vec::new();

    for value in values {
        println!("\n---- typing {value:?} ----");
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text(value, false);
        }
        let mut last_seen: Option<(String, String)> = None;
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_millis(900) {
            if let Ok(el) = desktop.focused_element() {
                let n = el.name().unwrap_or_default();
                if el.role() == "ComboBox" && looks_like_cell_ref(&n) {
                    let text = clean_cell_text(&el.text(0).unwrap_or_default());
                    if !text.is_empty() {
                        last_seen = Some((n, text));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        match &last_seen {
            Some((c, v)) => {
                println!("  watcher emitted: {c} = {v:?}");
                derived.push((c.clone(), v.clone()));
            }
            None => println!("  watcher emitted NOTHING"),
        }
        // Tab commits and moves right. No preamble.
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key("{Tab}");
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }

    // ---- ground truth -------------------------------------------------------
    println!("\n================ ground truth (CSV export) ================\n");
    let before_csv = newest_csv().map(|(p, _)| p);
    let export = format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, &export])
        .spawn()
    {
        let _ = c.wait();
    }
    let mut saved = String::new();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before_csv.as_ref() {
                println!("  downloaded {}", p.display());
                saved = std::fs::read_to_string(&p).unwrap_or_default();
                break;
            }
        }
    }
    if saved.is_empty() {
        println!("  no CSV -- cannot verify. Inconclusive.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::SUCCESS;
    }
    for (i, line) in saved.lines().enumerate().take(8) {
        println!("    row {:<3} {line:?}", i + 1);
    }

    // ---- verdict ------------------------------------------------------------
    println!("\n================ VERDICT ================\n");
    println!(
        "  {:<10} {:<12} {:<10} {:<12} {}",
        "typed", "watcher cell", "watcher v", "csv at cell", "match"
    );
    let mut correct = 0usize;
    for (i, value) in values.iter().enumerate() {
        let (cell, got) = derived
            .get(i)
            .cloned()
            .unwrap_or_else(|| ("-".into(), "-".into()));
        let at = parse_cell_ref(&cell)
            .and_then(|(c, r)| csv_at(&saved, c, r))
            .unwrap_or_else(|| "<none>".into());
        let ok = at == *value && got == *value;
        if ok {
            correct += 1;
        }
        println!("  {value:<10} {cell:<12} {got:<10} {at:<12} {ok}");
    }
    println!(
        "\n  watcher cell AND value confirmed by the saved file: {correct}/{}",
        values.len()
    );
    if correct == values.len() {
        println!("\n  CLEAN PASS. With no injected keystrokes, every value the watcher read");
        println!("  from the transient editor landed in exactly the cell the watcher named.");
    } else {
        println!("\n  NOT CLEAN. The watcher's cell attribution does not consistently match");
        println!("  where the data actually went, even with a commit that injects nothing.");
    }
    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ------------------------------------------------------ sheetskeys mode ----
// The design question that decides how Sheets capture must be built.
//
// The editor overlay is created by TYPING, not by clicking, so the Click event
// that drives `TextFieldWatcher::focus_moved` never fires for it. The pipeline
// therefore never learns the editor exists. The question is whether some event
// it already receives carries that element anyway.
//
// `KeyboardEvent.metadata.ui_element` is an `Option<UIElement>`. Whether the
// recorder POPULATES it, and whether the element it populates is the editor
// rather than the page, is not something to assume -- if it is populated, the
// fix is a few lines in an existing handler; if it is not, capture needs its own
// element resolution and a `Desktop` handle it does not currently hold.
async fn sheetskeys_mode() -> ExitCode {
    use futures::StreamExt;
    use terminator_workflow_recorder::{WorkflowEvent, WorkflowRecorder, WorkflowRecorderConfig};

    println!("== do keyboard events carry the Sheets cell editor? ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    let mut doc_id = String::new();
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                doc_id = rest.split('/').next().unwrap_or("").to_string();
            }
        }
    }
    println!("  DOCUMENT ID: {doc_id}");

    let config = WorkflowRecorderConfig {
        record_mouse: true,
        record_keyboard: true,
        capture_ui_elements: true,
        ..Default::default()
    };
    let mut recorder = WorkflowRecorder::new("sheetskeys".to_string(), config);
    let mut events = Box::pin(recorder.event_stream());
    if let Err(e) = recorder.start().await {
        eprintln!("could not start recorder: {e}");
        return ExitCode::FAILURE;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Collect events in the background while we drive typing.
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = std::sync::Arc::clone(&collected);
    let pump = tokio::spawn(async move {
        while let Some(ev) = events.next().await {
            if let WorkflowEvent::Keyboard(e) = ev {
                if !e.is_key_down {
                    continue;
                }
                let desc = match &e.metadata.ui_element {
                    Some(el) => format!(
                        "key={:<4} ui_element: role={:<10} name={:?} text={:?}",
                        e.key_code,
                        el.role(),
                        el.name().unwrap_or_default(),
                        el.text(0).unwrap_or_default()
                    ),
                    None => format!("key={:<4} ui_element: NONE", e.key_code),
                };
                if let Ok(mut v) = sink.lock() {
                    v.push(desc);
                }
            }
        }
    });

    println!("\n  typing 'apple' TAB 'banana' TAB into cells...");
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{home}");
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    for value in ["apple", "banana"] {
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text(value, false);
        }
        tokio::time::sleep(Duration::from_millis(900)).await;
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key("{Tab}");
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = recorder.stop().await;
    pump.abort();

    println!("\n================ KEYBOARD EVENTS ================\n");
    let rows = collected.lock().map(|v| v.clone()).unwrap_or_default();
    if rows.is_empty() {
        println!("  no key-down events captured at all.");
    }
    for r in rows.iter().take(40) {
        println!("  {r}");
    }
    let with_el = rows.iter().filter(|r| !r.contains("NONE")).count();
    let editorish = rows.iter().filter(|r| r.contains("ComboBox")).count();
    println!("\n  key-down events            : {}", rows.len());
    println!("  carrying a ui_element      : {with_el}");
    println!("  whose element is a ComboBox: {editorish}");
    println!("\n  DOCUMENT ID for cleanup: {doc_id}");

    if editorish > 0 {
        println!("\n  The editor IS reachable from keyboard events -- capture can hook the");
        println!("  existing Keyboard handler without new element resolution.");
    } else if with_el > 0 {
        println!("\n  Keyboard events carry an element, but never the editor. Capture would");
        println!("  have to resolve the focused element itself.");
    } else {
        println!("\n  Keyboard events carry no element at all. Capture needs its own");
        println!("  focused-element resolution, and therefore a Desktop handle.");
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------- sheetscapture mode ----
// Integration test for the shipped grid capture path.
//
// Drives real edits into a real Sheets document through a real CaptureSession,
// then checks the captured actions against the document's own CSV export. The
// export is the arbiter -- the UI that produced the reading cannot also verify
// it.
async fn sheetscapture_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};

    println!("== integrated Sheets cell capture, end to end ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    let mut doc_id = String::new();
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                doc_id = rest.split('/').next().unwrap_or("").to_string();
            }
        }
    }
    println!("  DOCUMENT ID: {doc_id}");

    let session = match CaptureSession::start_session(
        "sheetscapture",
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

    // A click first, so the watcher has app identity for the exclusion gate.
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{home}");
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Tab commits and moves right; four cells across row 1.
    let values = ["apple", "banana", "cherry", "date"];
    for value in values {
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text(value, false);
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key("{Tab}");
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ CAPTURED ================\n");
    println!(
        "  {} action(s), {} unmapped event(s)",
        report.actions.len(),
        report.unmapped_events
    );
    // Distinguishes "the watcher produced nothing" from "it produced candidates
    // the gate rejected" -- opposite diagnoses with opposite fixes.
    println!("  {} exclusion(s):", report.exclusions.len());
    for e in report.exclusions.iter().take(12) {
        println!("    {:?} {:?}", e.kind.as_str(), e.reason);
    }
    let mut cell_types: Vec<(String, String)> = Vec::new();
    for a in &report.actions {
        println!(
            "    {:<9} role={:<10} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
        if a.kind.as_str() == "type" {
            if let (Some(n), Some(p)) = (a.element_name.clone(), a.payload.clone()) {
                cell_types.push((n, p));
            }
        }
    }

    // ---- ground truth -------------------------------------------------------
    println!("\n================ GROUND TRUTH (CSV) ================\n");
    let before_csv = newest_csv().map(|(p, _)| p);
    let export = format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, &export])
        .spawn()
    {
        let _ = c.wait();
    }
    let mut saved = String::new();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before_csv.as_ref() {
                saved = std::fs::read_to_string(&p).unwrap_or_default();
                break;
            }
        }
    }
    for (i, line) in saved.lines().enumerate().take(6) {
        println!("    row {:<3} {line:?}", i + 1);
    }

    println!("\n================ VERDICT ================\n");
    println!("  {:<8} {:<12} {:<10} {:<10} {}", "typed", "captured as", "payload", "csv there", "ok");
    let mut correct = 0usize;
    for (i, value) in values.iter().enumerate() {
        let (cell, payload) = cell_types
            .get(i)
            .cloned()
            .unwrap_or_else(|| ("-".into(), "-".into()));
        let at = parse_cell_ref(&cell)
            .and_then(|(c, r)| csv_at(&saved, c, r))
            .unwrap_or_else(|| "<none>".into());
        let clean = !payload.contains('\u{feff}');
        let ok = payload == *value && at == *value && clean;
        if ok {
            correct += 1;
        }
        println!("  {value:<8} {cell:<12} {payload:<10} {at:<10} {ok}");
    }
    println!(
        "\n  type actions captured                 : {}",
        cell_types.len()
    );
    println!("  cell + payload confirmed by CSV       : {correct}/{}", values.len());
    println!(
        "  payloads containing U+FEFF            : {}",
        cell_types
            .iter()
            .filter(|(_, p)| p.contains('\u{feff}'))
            .count()
    );
    if correct == values.len() {
        println!("\n  PASS: every cell edit was captured, attributed to the right cell,");
        println!("  with clean text, and confirmed against the saved document.");
    } else {
        println!("\n  NOT A CLEAN PASS -- see the rows above.");
    }
    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ----------------------------------------------------- notepadgrid mode ----
// Does Document-role capture still work with GridCellWatcher in the pipeline?
//
// `notepad` mode cannot answer this here: its precondition compares the focused
// element's pid against the pid it launched, and Windows 11 Notepad hands new
// launches to an existing instance, so the window is owned by a process we did
// not spawn. That is the same pid instability the window-identity work already
// measured. It correctly refuses to proceed, twice, and forcing it through would
// mean typing into a window this probe cannot vouch for.
//
// So identity comes from the element's own properties instead of from a pid:
//
//   * its window's title contains "Notepad"
//   * its role is one `capture::text` accepts
//   * its text is EMPTY -- a fresh surface, not a document with the user's work
//
// And the window is ACTIVATED first rather than assumed to have taken focus,
// which is why the original precondition never converged: focus was sitting on
// an unrelated Button the whole time.
async fn notepadgrid_mode() -> ExitCode {
    use paradigm_lib::capture::{text, CaptureSession, ExclusionList};

    println!("== Notepad capture with GridCellWatcher in the pipeline ==\n");
    println!("WARNING: performs real clicks and typing. Hands off.\n");

    // Launch an EMPTY, uniquely-named file rather than a bare Notepad.
    //
    // `notepad.exe` with no argument does not reliably give a fresh buffer on
    // Windows 11: it restores the previous session's tabs. Measured here --
    // after closing every Notepad window, a bare launch reopened a stale
    // `*paradigm-probe-…` document and no `Untitled - Notepad` existed at all,
    // so the probe had nothing it was willing to type into.
    //
    // A named empty file fixes both halves: the title is unique, so the window
    // is unambiguous, and the buffer is empty by construction rather than by
    // hope. The `paradigm-probe-` prefix is what `notepadclose` recognises.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let doc_name = format!("paradigm-probe-{stamp}");
    let doc_path = std::env::temp_dir().join(format!("{doc_name}.txt"));
    if std::fs::write(&doc_path, "").is_err() {
        eprintln!("could not create the probe document");
        return ExitCode::FAILURE;
    }
    println!("  probe document: {}", doc_path.display());
    if std::process::Command::new("notepad.exe")
        .arg(&doc_path)
        .spawn()
        .is_err()
    {
        eprintln!("could not launch Notepad");
        return ExitCode::FAILURE;
    }
    tokio::time::sleep(Duration::from_secs(6)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Find a Notepad window, activate it, and only then look at focus.
    // Every UIA call below is time-bounded. Three earlier attempts at this
    // measurement ran past their timeout producing no output at all, and a hang
    // with no output cannot be told apart from a slow machine. Bounding the
    // calls converts that into a reportable fact.
    println!("  enumerating Notepad windows (bounded to 20s)...");
    let started = std::time::Instant::now();
    let windows = match tokio::time::timeout(
        Duration::from_secs(20),
        desktop
            .locator("role:Window|name:Notepad")
            .within(desktop.root())
            .all(Some(Duration::from_secs(8)), Some(3)),
    )
    .await
    {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => {
            println!("  enumeration errored: {e}");
            Vec::new()
        }
        Err(_) => {
            println!("\n  INCONCLUSIVE: enumerating Notepad windows did not return within 20s.");
            println!("  This machine currently has a Notepad holding a very large document,");
            println!("  and UIA traversal through it is the suspected cause. Not a result");
            println!("  about capture.");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "  Notepad windows found: {} in {} ms",
        windows.len(),
        started.elapsed().as_millis()
    );
    for w in &windows {
        println!("    {:?}", w.name().unwrap_or_default());
    }

    let mut surface: Option<(UIElement, String, String)> = None;
    for w in &windows {
        let title = w.name().unwrap_or_default();

        // Title first, before ANY text read. `text(0)` walks the element's
        // subtree, and on a Notepad holding a large document that read does not
        // return in any usable time. Matching the document this probe just
        // created keeps the read cheap AND keeps the probe away from a window
        // holding somebody's work.
        if !title.starts_with(&doc_name) && !title.starts_with(&format!("*{doc_name}")) {
            println!("    skipping {title:?}: not the document this probe created");
            continue;
        }

        let _ = w.activate_window();
        tokio::time::sleep(Duration::from_millis(1200)).await;

        let Ok(el) = desktop.focused_element() else {
            continue;
        };
        let role = el.role();
        let win_name = el
            .window()
            .ok()
            .flatten()
            .and_then(|x| x.name())
            .unwrap_or_default();

        // Bounded: this is the read that hangs on a large buffer.
        let probe_el = el.clone();
        let body = match tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(move || probe_el.text(0).unwrap_or_default()),
        )
        .await
        {
            Ok(Ok(t)) => t,
            _ => {
                println!("    rejected: reading this surface's text did not return within 10s");
                continue;
            }
        };
        println!(
            "  after activating {title:?}: focus role={role:?} window={win_name:?} text_len={}",
            body.len()
        );

        if !win_name.contains("Notepad") && !title.contains("Notepad") {
            println!("    rejected: focus is not inside a Notepad window");
            continue;
        }
        if !text::is_text_role(&role) {
            println!("    rejected: role {role:?} is not an editable role");
            continue;
        }
        if !body.trim().is_empty() {
            println!("    rejected: surface is NOT empty -- refusing to type into real content");
            continue;
        }
        surface = Some((el, role, title));
        break;
    }

    let Some((element, role, title)) = surface else {
        println!("\n  INCONCLUSIVE: no freshly-launched, verified-empty Notepad surface could");
        println!("  be confirmed. Not typing into a window that cannot be vouched for.");
        return ExitCode::FAILURE;
    };
    println!("\n  anchored on {title:?}, role={role:?}, verified empty");
    println!("  accepted by is_text_role: {}", text::is_text_role(&role));

    // ---- capture ------------------------------------------------------------
    // In-memory trace, so the keystroke-focus path can be OBSERVED rather than
    // assumed inert here. See `capture::text`.
    text::set_trace(true);
    let session = match CaptureSession::start_session(
        "notepad-grid-regression",
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

    println!("\n-- driving: click, type a line, Enter, type a second line --");
    robust_click(&desktop, &element);
    tokio::time::sleep(Duration::from_millis(700)).await;
    for ch in "alpha line".chars() {
        let _ = element.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = element.press_key("{Enter}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    for ch in "beta line".chars() {
        let _ = element.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Phase 2: NO SETTLE, whole string at once -- the shape that reaches the
    // keystroke-focus path in a browser. If that path can misbehave in Notepad,
    // this is where it would.
    println!("-- driving: Enter, then a no-settle fast burst --");
    let _ = element.press_key("{Enter}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    robust_click(&desktop, &element);
    let _ = element.type_text("gammaburst", false);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let actual = element.text(0).unwrap_or_default();
    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- results ------------------------------------------------------------
    println!("\n================ CAPTURED ================\n");
    println!(
        "  {} action(s), {} exclusion(s), {} unmapped",
        report.actions.len(),
        report.exclusions.len(),
        report.unmapped_events
    );
    for a in &report.actions {
        println!(
            "    {:<9} role={:<10} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
    }
    for e in report.exclusions.iter().take(8) {
        println!("    excluded {:?} {:?}", e.kind.as_str(), e.reason);
    }

    let types: Vec<&paradigm_lib::capture::CapturedAction> = report
        .actions
        .iter()
        .filter(|a| a.kind.as_str() == "type")
        .collect();
    let combined: String = types
        .iter()
        .filter_map(|a| a.payload.clone())
        .collect::<Vec<_>>()
        .join("");
    let grid_actions = report
        .actions
        .iter()
        .filter(|a| a.element_role.as_deref() == Some("ComboBox"))
        .count();

    // ---- the keystroke-focus path, observed --------------------------------
    let watcher_trace = text::take_trace();
    let ks_inert = watcher_trace
        .iter()
        .filter(|l| l.contains("focus is already the watched element"))
        .count();
    let ks_refused = watcher_trace
        .iter()
        .filter(|l| l.contains("keystroke-follow REFUSED"))
        .count();
    let ks_started = watcher_trace
        .iter()
        .filter(|l| l.contains("keystroke-follow STARTED"))
        .count();

    println!("\n================ WATCHER TRACE ================\n");
    for line in watcher_trace.iter().take(60) {
        println!("  {line}");
    }
    if watcher_trace.len() > 60 {
        println!("  ... {} more lines", watcher_trace.len() - 60);
    }

    println!("\n================ VERDICT ================\n");
    println!("  type actions captured        : {}", types.len());
    println!("  concatenated payloads        : {combined:?}");
    println!("  actually in the Notepad buffer: {actual:?}");
    println!("  actions from the GRID path    : {grid_actions}  (must be 0)");
    println!("\n  keystroke path ran, focus already watched : {ks_inert}");
    println!("  keystroke path REFUSED (non-startable role): {ks_refused}");
    println!("  keystroke path STARTED a watch             : {ks_started}  (must be 0)");
    if ks_inert + ks_refused == 0 {
        println!("  !! the keystroke path left no trace at all -- it may not have run,");
        println!("     so this run does NOT establish that it is inert in Notepad.");
    }

    let norm = |s: &str| s.replace("\r\n", "\n").replace('\r', "\n");
    let text_ok = !types.is_empty() && norm(&combined) == norm(&actual);
    let grid_ok = grid_actions == 0;
    let ks_ok = ks_started == 0 && (ks_inert + ks_refused) > 0;
    if text_ok && grid_ok && ks_ok {
        println!("\n  PASS: Document-role capture is unchanged, the grid path stayed inert,");
        println!("  and the keystroke-focus path ran but never started a watch on Document.");
    } else if text_ok && grid_ok {
        println!("\n  PARTIAL: capture is correct, but the keystroke path was not observed");
        println!("  behaving as required -- see the counts above.");
    } else if !text_ok {
        println!("\n  REGRESSION: Notepad text capture no longer reproduces the buffer.");
    } else {
        println!("\n  REGRESSION: the grid path produced actions inside Notepad.");
    }

    println!("\n--- cleanup ---");
    println!("  Notepad left open with unsaved text; close it without saving.");
    ExitCode::SUCCESS
}

// ---------------------------------------------------- notepadclose mode ----
// Close the Notepad windows this investigation's probes left behind.
//
// Deliberately not a blanket close. Every probe run left an instance open, but
// the user's own Notepad windows are in the same list and are indistinguishable
// by process. So the title is the discriminator, and anything that is not
// recognisably probe-created is left alone and reported.
//
// Recognisably probe-created:
//   "Untitled - Notepad"          a fresh buffer -- nothing to lose either way
//   "*alpha line - Notepad"       the exact text `notepadgrid` types
//   "*paradigm-probe-… - Notepad" a file an earlier probe created
//
// Anything else -- a real filename -- is somebody's work and is not touched.
// A `*` means unsaved changes, so closing raises a save prompt; that is answered
// with "Don't save", which is only ever reached for a window already classified
// as probe-created.
fn is_probe_notepad(title: &str) -> bool {
    let t = title.trim();
    t.starts_with("Untitled - Notepad")
        || t.starts_with("*alpha line - Notepad")
        || t.starts_with("*paradigm-probe-")
        || t.starts_with("paradigm-probe-")
}

async fn notepadclose_mode() -> ExitCode {
    println!("== close probe-created Notepad windows ==\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    let windows = desktop
        .locator("role:Window|name:Notepad")
        .within(desktop.root())
        .all(Some(Duration::from_secs(10)), Some(3))
        .await
        .unwrap_or_default();

    println!("  {} Notepad window(s) open:\n", windows.len());
    let mut safe = Vec::new();
    let mut keep = Vec::new();
    for w in &windows {
        let title = w.name().unwrap_or_default();
        if is_probe_notepad(&title) {
            println!("    PROBE-CREATED  {title:?}");
            safe.push(w.clone());
        } else {
            println!("    LEAVING ALONE  {title:?}");
            keep.push(title);
        }
    }

    if !keep.is_empty() {
        println!(
            "\n  {} window(s) are not recognisably probe-created and will NOT be touched.",
            keep.len()
        );
    }
    if safe.is_empty() {
        println!("\n  nothing to close.");
        return ExitCode::SUCCESS;
    }

    println!("\n  closing {} probe window(s)...", safe.len());
    let mut closed = 0usize;
    for w in &safe {
        let title = w.name().unwrap_or_default();
        if w.close().is_err() {
            println!("    could not close {title:?}");
            continue;
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;

        // An unsaved buffer raises a save prompt. Answer it with "Don't save".
        for label in ["Don't save", "Do not save", "Don’t save"] {
            if let Ok(btns) = desktop
                .locator(format!("role:Button|name:{label}").as_str())
                .within(desktop.root())
                .all(Some(Duration::from_secs(3)), Some(6))
                .await
            {
                if let Some(b) = btns.first() {
                    println!("    answering save prompt for {title:?} with {label:?}");
                    robust_click(&desktop, b);
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    break;
                }
            }
        }
        closed += 1;
    }

    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = desktop
        .locator("role:Window|name:Notepad")
        .within(desktop.root())
        .all(Some(Duration::from_secs(10)), Some(3))
        .await
        .unwrap_or_default();
    println!("\n  closed {closed}; {} Notepad window(s) remain:", after.len());
    for w in &after {
        println!("    {:?}", w.name().unwrap_or_default());
    }

    ExitCode::SUCCESS
}

// ----------------------------------------------------- sheetsentry mode ----
// Part 1: how does anything GET INTO a cell, when no cell element exists?
//
// Replay's normal shape is resolve-a-selector then act on what it finds. A grid
// cell has nothing to resolve: the ComboBox editor is created BY typing, so it
// cannot be the thing that receives the typing.
//
// The obvious answer is coordinate clicking, and it is the fragile one -- it
// breaks on scroll, zoom, window resize and frozen rows. But there is a
// persistent, element-based candidate that was never fairly tested: the Name
// Box. It is a real element (`Edit` child of the "Name box (Ctrl + J)" group),
// and its text was measured tracking the cursor within 0-1 ms. Typing a
// reference into it and pressing Enter is how a keyboard user reaches a cell.
//
// It was tried once in `sheetswatch` and appeared to fail -- values landed in
// A1. That run committed with `type_text("\n")`, which does not submit anything,
// so the navigation never happened and the mechanism was never actually on
// trial. Note also that `press_key("{Enter}")`'s injected {LEFT}{END} is
// harmless HERE: inside a text field those are caret moves, not grid navigation.
//
// This mode tests entry by Name Box, verifying after each step that the cursor
// really moved, and checks the result against the CSV export.
async fn sheetsentry_mode() -> ExitCode {
    println!("== can the Name Box drive cell entry for replay? ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    let mut doc_id = String::new();
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                doc_id = rest.split('/').next().unwrap_or("").to_string();
            }
        }
    }
    println!("  DOCUMENT ID: {doc_id}");

    // The Name Box input: the Edit child of the group named "Name box".
    let name_box = desktop
        .locator("name:Name box")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
        .and_then(|g| {
            g.children()
                .ok()
                .and_then(|c| c.into_iter().find(|e| e.role() == "Edit"))
        });
    let Some(name_box) = name_box else {
        println!("  Name Box input not found -- entry by Name Box is not available.");
        return ExitCode::FAILURE;
    };
    println!("  Name Box input found: {}", snap(&name_box));

    let plan = [("B2", "apple"), ("D5", "banana"), ("C9", "cherry")];
    let mut steps: Vec<(String, bool, String)> = Vec::new();

    for (cell, value) in plan {
        println!("\n---- {cell} = {value:?} ----");

        // ENTRY: replace the Name Box contents, then submit.
        //
        // `type_text` APPENDS -- measured: the box read "A1", then "A1B2", then
        // "A1B2D5", never a valid reference, so Enter did nothing. It has to be
        // cleared first. Two ways to do that, tried in order so the run says
        // which actually works rather than assuming.
        let mut moved = false;
        let mut landed = String::new();
        let mut how = "none";
        for strategy in ["set_value", "ctrl+a then type"] {
            match strategy {
                "set_value" => {
                    let _ = name_box.set_value(cell);
                }
                _ => {
                    let _ = name_box.press_key("{ctrl}a");
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let _ = name_box.type_text(cell, true);
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            // {LEFT}{END} injected before Enter are caret moves inside this text
            // field, so the grid defect does not apply here.
            let _ = name_box.press_key("{Enter}");
            tokio::time::sleep(Duration::from_millis(1400)).await;

            landed = name_box.text(0).unwrap_or_default();
            println!("  via {strategy:<18} box reads {landed:?}");
            if landed.trim() == cell {
                moved = true;
                how = strategy;
                break;
            }
        }
        println!("  cursor on {cell}: {moved} (via {how})");
        if !moved {
            println!("  NOT typing: the cursor is not demonstrably on {cell}");
            steps.push((cell.to_string(), false, String::new()));
            continue;
        }

        // TYPE into whatever now has focus (the grid's hidden input).
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text(value, false);
        }
        tokio::time::sleep(Duration::from_millis(700)).await;

        // Observe the editor, which is the only per-cell element that exists.
        let seen = desktop
            .focused_element()
            .ok()
            .map(|el| (el.role(), el.name().unwrap_or_default()))
            .unwrap_or_default();
        println!("  editor during typing: role={:?} name={:?}", seen.0, seen.1);

        // Commit with Tab -- no injected keystrokes.
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key("{Tab}");
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
        steps.push((cell.to_string(), true, seen.1));
    }

    // ---- ground truth -------------------------------------------------------
    println!("\n================ GROUND TRUTH (CSV) ================\n");
    let before_csv = newest_csv().map(|(p, _)| p);
    let export = format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, &export])
        .spawn()
    {
        let _ = c.wait();
    }
    let mut saved = String::new();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before_csv.as_ref() {
                saved = std::fs::read_to_string(&p).unwrap_or_default();
                break;
            }
        }
    }
    for (i, line) in saved.lines().enumerate().take(12) {
        println!("    row {:<3} {line:?}", i + 1);
    }

    println!("\n================ VERDICT ================\n");
    println!("  {:<6} {:<9} {:<8} {:<12} {}", "cell", "value", "entered", "editor said", "csv at cell");
    let mut ok = 0usize;
    for (i, (cell, value)) in plan.iter().enumerate() {
        let (_, entered, editor) = steps
            .get(i)
            .cloned()
            .unwrap_or_else(|| (cell.to_string(), false, String::new()));
        let at = parse_cell_ref(cell)
            .and_then(|(c, r)| csv_at(&saved, c, r))
            .unwrap_or_else(|| "<none>".into());
        if at == *value {
            ok += 1;
        }
        println!("  {cell:<6} {value:<9} {entered:<8} {editor:<12} {at}");
    }
    println!("\n  values landing in the INTENDED cell: {ok}/{}", plan.len());
    if ok == plan.len() {
        println!("\n  Name Box entry works. Replay can reach a cell through a persistent");
        println!("  element, with no coordinates involved.");
    } else {
        println!("\n  Name Box entry did NOT place every value correctly.");
    }
    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ----------------------------------------------------- sheetsmulti mode ----
//
// The wrong-sheet gap: a captured grid edit carries a bare cell reference, so
// replay writes into whichever tab happens to be active. A playbook recorded on
// Sheet2 and replayed with Sheet1 in front writes to the wrong sheet, and every
// step reports success.
// See docs/known-issues/complex-web-grid-capture-unreliable.md.
//
// Two assumptions have to hold before that is a small fix, and BOTH are measured
// here rather than assumed:
//
//   1. Can capture read the ACTIVE sheet's name? A list of sheet names is not
//      enough -- the tree has to say which one is current, or capture cannot
//      record what it was. So this switches sheets and checks the marker MOVES.
//   2. Does the Name Box accept a qualified `Sheet2!B2` and cross tabs?
//
// Ground truth is the per-sheet CSV export (`export?format=csv&gid=<n>`), never
// the UI that produced the edit. Sheet1 must be EMPTY and Sheet2 must hold the
// value -- checking only that Sheet2 has it would pass if the write landed in
// both, and checking only Sheet1 would pass if nothing was written at all.

/// Everything the tree offers that could mark a tab as the current one.
fn describe_tab(el: &UIElement) -> String {
    let a = el.attributes();
    format!(
        "role={:<13} name={:?} selected={:?} toggled={:?} focused={:?} desc={:?}",
        a.role,
        a.name.unwrap_or_default(),
        a.is_selected,
        a.is_toggled,
        a.is_focused,
        a.description.unwrap_or_default(),
    )
}

/// The browser address bar's text, which carries `#gid=<n>` for the open sheet.
async fn address_of(desktop: &Desktop, window: &UIElement) -> String {
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

fn gid_in(addr: &str) -> Option<String> {
    let rest = addr.split("gid=").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!digits.is_empty()).then_some(digits)
}

/// Every sheet tab, found the way the rest of this file finds elements.
///
/// Deliberately the locator and not a hand-rolled `children()` walk. The first
/// version of this probe walked the tree itself with a node budget, reported
/// "no sheet tabs exist", and was WRONG -- the budget ran out before reaching
/// the tab bar while `find_named` located a "Sheet1" tab in the same run. The
/// locator searches to depth 50 and does not silently truncate.
async fn sheet_tabs(desktop: &Desktop, window: &UIElement) -> Vec<UIElement> {
    let mut out: Vec<UIElement> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for role in ["role:Tab", "role:TabItem", "role:Button", "role:ListItem"] {
        if let Ok(all) = desktop
            .locator(role)
            .within(window.clone())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            for el in all {
                let n = el.name().unwrap_or_default();
                let t = n.trim();
                let looks_like_tab = t.len() > 5
                    && t.starts_with("Sheet")
                    && t[5..].chars().all(|c| c.is_ascii_digit());
                if looks_like_tab {
                    let key = format!("{}/{}", el.role(), t);
                    if !seen.contains(&key) {
                        seen.push(key);
                        out.push(el);
                    }
                }
            }
        }
    }
    out
}

/// Download one sheet's CSV export and return its body.
async fn download_csv(browser: &str, doc_id: &str, gid: &str) -> Option<String> {
    let before = newest_csv().map(|(p, _)| p);
    let export =
        format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv&gid={gid}");
    // NOT `cmd /C start`. A `gid=` export URL contains `&`, which cmd treats as
    // a command separator -- measured twice: unquoted it tried to run `gid=0`
    // as a program, and quoted it silently dropped the parameter, so BOTH
    // exports came back as the default sheet named "…- Sheet1.csv". Either way
    // the ground-truth check was measuring nothing while reporting a verdict.
    //
    // `Start-Process` takes the URL as one argument with no shell re-parsing.
    // Verified: gid=23428486 downloaded "…- Sheet2.csv" carrying the marker,
    // gid=0 downloaded an empty "…- Sheet1.csv".
    if let Ok(mut c) = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Start-Process",
            browser,
            "-ArgumentList",
            &format!("'{export}'"),
        ])
        .spawn()
    {
        let _ = c.wait();
    }
    // 45s, not 30: the first corrected run reported "did not download" for two
    // exports that were sitting in Downloads seconds later.
    for _ in 0..45 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before.as_ref() {
                return std::fs::read_to_string(&p).ok();
            }
        }
    }
    None
}

async fn sheetsmulti_mode() -> ExitCode {
    println!("== the wrong-sheet gap: can it even be fixed? ==\n");
    println!("Creates a blank 'Untitled spreadsheet' in the signed-in Drive account");
    println!("and adds a second sheet. The DOCUMENT ID is printed at the end for");
    println!("cleanup via `sheetstrash <id>`.\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 30s for Sheets to load...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("\n  INCONCLUSIVE: no 'Untitled spreadsheet' window found. Not signed in,");
        println!("  or the document did not load. Nothing below would be measuring Sheets.");
        return ExitCode::FAILURE;
    };
    println!("  window: {:?}", window.name().unwrap_or_default());

    let addr0 = address_of(&desktop, &window).await;
    let doc_id = addr0
        .split("/d/")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("")
        .to_string();
    println!("  DOCUMENT ID: {doc_id}");
    println!("  address: {addr0:?}");
    let gid_sheet1 = gid_in(&addr0).unwrap_or_else(|| "0".to_string());
    println!("  gid of the first sheet: {gid_sheet1}");

    // ---- Q1a: what does the tree show for ONE sheet? -----------------------
    println!("\n================ Q1a. sheet tabs, one sheet ================\n");
    let found = sheet_tabs(&desktop, &window).await;
    if found.is_empty() {
        println!("  no sheet-tab element found.");
    }
    for el in found.iter().take(15) {
        println!("  {}", describe_tab(el));
    }

    // ---- add a second sheet -------------------------------------------------
    println!("\n================ adding a second sheet ================\n");
    let add = find_named(&desktop, &window, &["role:Button"], |n| {
        n.trim().eq_ignore_ascii_case("Add Sheet")
    })
    .await;
    match &add {
        Some(b) => {
            println!("  clicking {:?}", b.name().unwrap_or_default());
            robust_click(&desktop, b);
        }
        None => {
            println!("  no 'Add Sheet' button found; falling back to {{shift}}{{f11}}");
            if let Ok(el) = desktop.focused_element() {
                let _ = el.press_key("{shift}{f11}");
            }
        }
    }
    tokio::time::sleep(Duration::from_secs(4)).await;

    let addr_after_add = address_of(&desktop, &window).await;
    let gid_sheet2 = gid_in(&addr_after_add).unwrap_or_default();
    println!("  address now: {addr_after_add:?}");
    println!("  gid of the new sheet: {gid_sheet2:?}");

    // ---- Q1b: does the tree say WHICH sheet is active? ---------------------
    //
    // The whole question. Two names in a list are useless to capture; the
    // marker has to move when the active sheet changes.
    println!("\n================ Q1b. with TWO sheets, second active ================\n");
    let found2 = sheet_tabs(&desktop, &window).await;
    for el in found2.iter().take(15) {
        println!("  {}", describe_tab(el));
    }
    // Everything under the tab bar, including unnamed nodes, in case the
    // selected-state lives on a wrapper rather than on the tab itself.
    if let Some(bar) = find_named(&desktop, &window, &["role:Group"], |n| {
        n.trim() == "Sheet tab bar"
    })
    .await
    {
        println!("\n  full 'Sheet tab bar' subtree:");
        let mut b = 120usize;
        dump_tree(&bar, 0, 8, &mut b);
    }
    let marked_when_sheet2: Vec<String> = found2
        .iter()
        .filter(|e| {
            let a = e.attributes();
            a.is_selected == Some(true) || a.is_toggled == Some(true)
        })
        .filter_map(|e| e.name())
        .collect();
    println!("\n  marked selected/toggled: {marked_when_sheet2:?}");

    // Switch back to the first sheet and re-read. If the marker does not move,
    // it is not an active-sheet signal.
    println!("\n================ Q1c. switching back to the first sheet ================\n");
    let tab1 = find_named(&desktop, &window, &["role:Tab", "role:Button", "role:ListItem"], |n| {
        n.trim() == "Sheet1"
    })
    .await;
    match &tab1 {
        Some(t) => {
            println!("  clicking tab {:?}", t.name().unwrap_or_default());
            robust_click(&desktop, t);
        }
        None => println!("  could not find a 'Sheet1' tab element to click"),
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    let addr_back = address_of(&desktop, &window).await;
    println!("  address now: {addr_back:?}");
    println!("  gid now: {:?}", gid_in(&addr_back));

    let found3 = sheet_tabs(&desktop, &window).await;
    for el in found3.iter().take(15) {
        println!("  {}", describe_tab(el));
    }
    let marked_when_sheet1: Vec<String> = found3
        .iter()
        .filter(|e| {
            let a = e.attributes();
            a.is_selected == Some(true) || a.is_toggled == Some(true)
        })
        .filter_map(|e| e.name())
        .collect();
    println!("\n  marked selected/toggled: {marked_when_sheet1:?}");

    let q1_names_present = found3.iter().filter_map(|e| e.name()).any(|n| n.trim() == "Sheet2");
    let q1_marker_moves = !marked_when_sheet1.is_empty()
        && !marked_when_sheet2.is_empty()
        && marked_when_sheet1 != marked_when_sheet2;
    let q1_gid_moves = gid_in(&addr_back) != gid_in(&addr_after_add)
        && gid_in(&addr_after_add).is_some();

    // ---- Q2: does the Name Box take a qualified reference? -----------------
    println!("\n================ Q2. Name Box with 'Sheet2!B2' ================\n");
    println!("  (the first sheet is active, so a cross-sheet jump is required)");

    let name_box = desktop
        .locator("name:Name box")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
        .and_then(|g| {
            g.children()
                .ok()
                .and_then(|c| c.into_iter().find(|e| e.role() == "Edit"))
        });
    let Some(name_box) = name_box else {
        println!("  Name Box input not found -- Q2 cannot be answered.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    };

    const MARKER: &str = "crosssheetmarker";
    let _ = name_box.set_value("Sheet2!B2");
    tokio::time::sleep(Duration::from_millis(600)).await;
    let _ = name_box.press_key("{Enter}");
    tokio::time::sleep(Duration::from_millis(1800)).await;

    let box_reads = name_box.text(0).unwrap_or_default();
    let addr_after_jump = address_of(&desktop, &window).await;
    println!("  Name Box reads afterwards : {box_reads:?}");
    println!("  address afterwards        : {addr_after_jump:?}");
    println!("  gid afterwards            : {:?}", gid_in(&addr_after_jump));

    let jumped_by_gid = gid_in(&addr_after_jump).is_some()
        && !gid_sheet2.is_empty()
        && gid_in(&addr_after_jump) == Some(gid_sheet2.clone());

    // Type into wherever the cursor landed, and commit with Tab.
    // NOT Enter: press_key injects {LEFT}{END} and {END} relocates the cursor in
    // a grid -- see docs/known-issues/press-key-enter-injects-end-keystroke.md.
    if let Ok(target) = desktop.focused_element() {
        let _ = target.type_text(MARKER, false);
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    let committer = desktop.focused_element().ok();
    if let Some(c) = &committer {
        let _ = c.press_key("{Tab}");
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    // ---- ground truth: which sheet actually holds it? ----------------------
    println!("\n================ ground truth: per-sheet CSV ================\n");
    let csv1 = download_csv(browser, &doc_id, &gid_sheet1).await;
    match &csv1 {
        Some(b) => {
            println!("  sheet 1 (gid={gid_sheet1}) contents:");
            for line in b.lines().take(6) {
                println!("    {line:?}");
            }
        }
        None => println!("  sheet 1 export did not download"),
    }
    let csv2 = if gid_sheet2.is_empty() {
        println!("  no gid captured for the second sheet -- cannot export it");
        None
    } else {
        let c = download_csv(browser, &doc_id, &gid_sheet2).await;
        match &c {
            Some(b) => {
                println!("  sheet 2 (gid={gid_sheet2}) contents:");
                for line in b.lines().take(6) {
                    println!("    {line:?}");
                }
            }
            None => println!("  sheet 2 export did not download"),
        }
        c
    };

    let on_sheet1 = csv1.as_deref().map(|b| b.contains(MARKER)).unwrap_or(false);
    let on_sheet2 = csv2.as_deref().map(|b| b.contains(MARKER)).unwrap_or(false);

    // ---- verdict ------------------------------------------------------------
    println!("\n================ VERDICT ================\n");
    println!("  Q1  can capture read the ACTIVE sheet?");
    println!("      sheet names present in the tree      : {q1_names_present}");
    println!("      a selected/toggled marker MOVES      : {q1_marker_moves}");
    println!("        with sheet2 active: {marked_when_sheet2:?}");
    println!("        with sheet1 active: {marked_when_sheet1:?}");
    println!("      address-bar gid moves (independent)  : {q1_gid_moves}");
    println!();
    println!("  Q2  does the Name Box take 'Sheet2!B2'?");
    println!("      box read back                        : {box_reads:?}");
    println!("      gid changed to the second sheet      : {jumped_by_gid}");
    println!("      marker text landed on sheet 1        : {on_sheet1}  (must be false)");
    println!("      marker text landed on sheet 2        : {on_sheet2}  (must be true)");

    let q1 = q1_names_present && (q1_marker_moves || q1_gid_moves);
    let q2 = on_sheet2 && !on_sheet1;
    println!();
    match (q1, q2) {
        (true, true) => {
            println!("  BOTH HOLD -- the fix is small: record the active sheet name beside");
            println!("  the cell reference, and have grid_type navigate with 'Sheet!Cell'.");
        }
        (true, false) => {
            println!("  Q1 holds, Q2 does NOT. The sheet is knowable but the Name Box will");
            println!("  not cross tabs, so replay needs a real sheet-selection step first.");
        }
        (false, true) => {
            println!("  Q2 holds, Q1 does NOT. Replay could target a sheet, but capture");
            println!("  cannot tell which sheet the edit happened on -- so there is nothing");
            println!("  to record. This is the harder half.");
        }
        (false, false) => {
            println!("  NEITHER holds. The fix is not small and needs a different approach.");
        }
    }

    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    println!("  clean up with: cargo run --example text_capture_probe -- sheetstrash {doc_id}");
    ExitCode::SUCCESS
}

// ---------------------------------------------------- sheetsformula mode ----
//
// `sheetsread` established that a cell's value IS readable live, from an
// element with role `Edit` and an empty name. That is a finding, not yet a
// locator: a Sheets window holds several `Edit`s, and one of them is the Name
// Box -- which reports the cell REFERENCE. A reader that grabbed the wrong one
// would read "B2" where it meant to read the customer's name, and would look
// like it was working.
//
// So this enumerates every `Edit` in the window with enough context to tell them
// apart -- ancestry, bounds, and what each currently reports -- against a cell
// whose value is known. The output is what the spreadsheet SourceReader needs to
// target the formula bar deliberately rather than by position or luck.
async fn sheetsformula_mode() -> ExitCode {
    const MARKER: &str = "readprobe7391";
    const CELL: &str = "B2";

    println!("== which Edit is the formula bar? ==\n");

    let doc_arg = std::env::args().find(|a| {
        a.len() >= 40
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    });
    let browser = browser_order()[0];
    let url = match &doc_arg {
        Some(id) => format!("https://docs.google.com/spreadsheets/d/{id}/edit"),
        None => "https://sheets.new".to_string(),
    };
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", &url])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((_w, doc_id)) = sheets_window(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    println!("  document: {doc_id}");

    // Make sure the cell holds the marker, whether or not this document is a
    // reused one that already had it.
    goto_sheet_via_namebox(&desktop, CELL).await;
    if let Ok(t) = desktop.focused_element() {
        let _ = t.type_text(MARKER, false);
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    if let Ok(c) = desktop.focused_element() {
        let _ = c.press_key("{Tab}");
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    goto_sheet_via_namebox(&desktop, "A1").await;
    goto_sheet_via_namebox(&desktop, CELL).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let Some((window, _)) = sheets_window(&desktop).await else {
        println!("  lost the window.");
        return ExitCode::FAILURE;
    };

    /// Ancestor names, nearest first -- the cheapest stable way to tell two
    /// same-role, same-name elements apart.
    fn ancestry(el: &UIElement) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = el.clone();
        for _ in 0..6 {
            match cur.parent() {
                Ok(Some(p)) => {
                    out.push(format!(
                        "{}{}",
                        p.role(),
                        p.name()
                            .filter(|n| !n.trim().is_empty())
                            .map(|n| format!("({n})"))
                            .unwrap_or_default()
                    ));
                    cur = p;
                }
                _ => break,
            }
        }
        out
    }

    println!("\n================ EVERY Edit IN THE WINDOW ================\n");
    let edits = desktop
        .locator("role:Edit")
        .within(window.clone())
        .all(Some(Duration::from_secs(6)), None)
        .await
        .unwrap_or_default();
    println!("  {} Edit element(s)\n", edits.len());

    for (i, el) in edits.iter().enumerate().take(20) {
        let a = el.attributes();
        let t0 = el.text(0).unwrap_or_default();
        let carries = t0.contains(MARKER);
        let bounds = el
            .bounds()
            .ok()
            .map(|(x, y, w, h)| format!("({x:.0},{y:.0},{w:.0},{h:.0})"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  [{i}] name={:?} value={:?}",
            a.name.clone().unwrap_or_default(),
            a.value.clone().unwrap_or_default()
        );
        println!("      text(0)={t0:?}");
        println!("      bounds={bounds}  carries the cell value: {carries}");
        println!("      ancestry={:?}", ancestry(el));
        println!();
    }

    // The Name Box, resolved the way replay already resolves it, so the two can
    // be compared directly rather than guessed at.
    println!("================ THE NAME BOX, FOR CONTRAST ================\n");
    let name_box = desktop
        .locator("name:Name box")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
        .and_then(|g| {
            g.children()
                .ok()
                .and_then(|c| c.into_iter().find(|e| e.role() == "Edit"))
        });
    match &name_box {
        Some(nb) => {
            println!("  text(0)={:?}", nb.text(0).unwrap_or_default());
            println!("  bounds={:?}", nb.bounds().ok());
            println!("  ancestry={:?}", ancestry(nb));
            println!("\n  (this one reports the REFERENCE -- a reader must not use it)");
        }
        None => println!("  not found"),
    }

    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ------------------------------------------------------- sheetsread mode ----
//
// Can a cell's value be read back NON-DESTRUCTIVELY -- navigate to it, ask the
// tree what is there, without entering edit mode?
//
// This is the one unmeasured primitive the spreadsheet SourceReader needs. Every
// read this codebase performs against Sheets today is either the Name Box (a
// cell REFERENCE, not a value) or a CSV export (the whole sheet, rendered
// server-side). Neither is "read cell B2 live".
//
// There is prior evidence it may not be possible: text_capture_probe.rs:4769
// records that after typing, no readable element reported the typed text, and
// says plainly that "typing did not happen" and "typing happened and is
// invisible to UIA" were indistinguishable from that run.
//
// So: write a distinctive marker, move away, come back, and scan every element
// in the window for it -- checking name, value, description and text() at
// several depths, since which accessor carries a value is exactly what is
// unknown. Nothing is typed after the marker is written, so any hit is a real
// non-destructive read.
async fn sheetsread_mode() -> ExitCode {
    const MARKER: &str = "readprobe7391";
    const CELL: &str = "B2";

    println!("== can a cell's value be read back without editing it? ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((_window, doc_id)) = sheets_window(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    println!("  document: {doc_id}");

    // ---- 1. put a known value in a known cell -------------------------------
    println!("\n-- writing {MARKER:?} into {CELL} --");
    if goto_sheet_via_namebox(&desktop, CELL).await.is_none() {
        println!("  could not reach {CELL} via the Name Box.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }
    if let Ok(target) = desktop.focused_element() {
        let _ = target.type_text(MARKER, false);
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    // Tab, not Enter -- press_key prefixes Enter with {LEFT}{END}, and {END} in
    // a grid relocates the cursor. See press-key-enter-injects-end-keystroke.md.
    if let Ok(c) = desktop.focused_element() {
        let _ = c.press_key("{Tab}");
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // ---- 2. move away, then come back ---------------------------------------
    println!("-- moving away to A1, then back to {CELL} --");
    goto_sheet_via_namebox(&desktop, "A1").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    goto_sheet_via_namebox(&desktop, CELL).await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Nothing below types anything. Any hit is a non-destructive read.

    // ---- 3. the focused element, every accessor -----------------------------
    println!("\n================ THE FOCUSED ELEMENT ================\n");
    match desktop.focused_element() {
        Ok(el) => {
            let a = el.attributes();
            println!("  role        : {:?}", a.role);
            println!("  name        : {:?}", a.name);
            println!("  value       : {:?}", a.value);
            println!("  description : {:?}", a.description);
            for depth in [0usize, 1, 2, 5] {
                println!("  text({depth})     : {:?}", el.text(depth).ok());
            }
        }
        Err(e) => println!("  no focused element: {e}"),
    }

    // ---- 4. the whole window, hunting for the marker ------------------------
    println!("\n================ WHOLE-WINDOW SCAN ================\n");

    /// Every element whose name, value, description or text carries `needle`.
    fn hunt(
        el: &UIElement,
        needle: &str,
        depth: usize,
        budget: &mut usize,
        hits: &mut Vec<String>,
    ) {
        if *budget == 0 || depth > 14 {
            return;
        }
        *budget -= 1;
        let a = el.attributes();
        let mut where_found = Vec::new();
        if a.name.as_deref().unwrap_or_default().contains(needle) {
            where_found.push("name");
        }
        if a.value.as_deref().unwrap_or_default().contains(needle) {
            where_found.push("value");
        }
        if a.description.as_deref().unwrap_or_default().contains(needle) {
            where_found.push("description");
        }
        if el.text(0).unwrap_or_default().contains(needle) {
            where_found.push("text(0)");
        }
        if el.text(2).unwrap_or_default().contains(needle) {
            where_found.push("text(2)");
        }
        if !where_found.is_empty() {
            hits.push(format!(
                "role={:<12} name={:?} via {:?}",
                a.role,
                a.name.unwrap_or_default(),
                where_found
            ));
        }
        if let Ok(kids) = el.children() {
            for k in kids {
                hunt(&k, needle, depth + 1, budget, hits);
            }
        }
    }

    let Some((window, _)) = sheets_window(&desktop).await else {
        println!("  lost the window.");
        return ExitCode::FAILURE;
    };
    let mut hits = Vec::new();
    let mut budget = 9000usize;
    hunt(&window, MARKER, 0, &mut budget, &mut hits);
    println!("  elements carrying {MARKER:?}: {}", hits.len());
    for h in hits.iter().take(15) {
        println!("    {h}");
    }
    if budget == 0 {
        println!("  !! node budget exhausted -- the scan is a sample, not a total,");
        println!("     so an empty result here would NOT be conclusive.");
    }

    // ---- 5. cross-check: is the value even in the document? -----------------
    //
    // Without this, "nothing reports it" cannot be told apart from "it was never
    // written" -- the exact ambiguity that made the earlier attempt at this
    // inconclusive.
    println!("\n================ CROSS-CHECK (CSV) ================\n");
    tokio::time::sleep(Duration::from_secs(6)).await;
    let csv = download_csv(browser, &doc_id, "0").await.unwrap_or_default();
    let in_document = csv.contains(MARKER);
    for l in csv.lines().take(5) {
        println!("    {l:?}");
    }
    println!("\n  the marker IS in the saved document: {in_document}");

    println!("\n================ VERDICT ================\n");
    if !in_document {
        println!("  INCONCLUSIVE. The marker never reached the document, so this run");
        println!("  cannot distinguish 'UIA does not expose cell values' from 'the");
        println!("  write did not happen'. Not evidence either way.");
    } else if hits.is_empty() {
        println!("  NO LIVE READ. The value is definitely in the document, and nothing");
        println!("  in the window's accessibility tree reports it. A live per-cell read");
        println!("  is not available, so the SourceReader's read path has to come from");
        println!("  somewhere else -- CSV export being the mechanism already proven.");
    } else {
        println!("  LIVE READ AVAILABLE. The elements above report the cell's value");
        println!("  without entering edit mode; the reader can use that accessor.");
    }

    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// -------------------------------------------------- sheetsqualified mode ----
//
// The replay half, and the regression check, in one document so they verify
// each other.
//
//   Leg A -- a QUALIFIED step (`Sheet2!B2`) replayed while the document shows
//   Sheet1. It must land on Sheet2. This is the fix.
//
//   Leg B -- a BARE step (`C3`) replayed while the document shows Sheet1. It
//   must land on Sheet1, exactly as before sheet tracking existed. This is the
//   regression check, and it is the one that matters most: the common case is a
//   recording that never switches sheets, and it must be untouched.
//
// Ground truth is the per-sheet CSV export, and BOTH sheets are checked for BOTH
// markers. Checking only that each marker is where it belongs would pass if a
// marker landed on both sheets.
async fn sheetsqualified_mode() -> ExitCode {
    use paradigm_lib::capture::{ActionCandidate, ActionKind, CapturedStream, ExclusionList};
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    const QUALIFIED: &str = "qualifiedmarker";
    const BARE: &str = "baremarker";

    println!("== qualified replay lands on the right sheet; bare is unchanged ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((_w, doc_id)) = sheets_window(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    println!("  document: {doc_id}");
    if !ensure_second_sheet(&desktop).await {
        println!("\n  INCONCLUSIVE: could not get a second sheet.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }
    let gid_sheet2 = current_gid(&desktop).await.unwrap_or_default();
    println!("  Sheet2 gid = {gid_sheet2}");

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

    // One grid step, built through the real gate and compiler.
    let build = |name: &str, text: &str, title: &str| {
        let mut s = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
        s.admit(ActionCandidate {
            kind: ActionKind::Type,
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            element_role: Some("ComboBox".into()),
            element_name: Some(name.to_string()),
            payload: Some(text.to_string()),
            detail: None,
            timestamp_ms: 0,
        });
        compile(
            s.actions(),
            title,
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
    };

    let run_leg = |label: &str, pb: &paradigm_lib::compile::CompiledPlaybook| {
        println!("\n  -- {label} --");
        for s in &pb.steps {
            let payload: serde_json::Value =
                serde_json::from_str(&s.action_payload_json).unwrap_or_default();
            println!("     target name = {:?}", payload["target"]["name"].as_str());
        }
    };

    // ---- leg A: qualified, from Sheet1 --------------------------------------
    println!("\n================ LEG A: qualified step ================");
    let parked = goto_sheet_via_namebox(&desktop, "Sheet1!A1").await;
    println!("  parked on gid {parked:?} (must be 0, so a switch is required)");
    if parked.as_deref() != Some("0") {
        println!("\n  INCONCLUSIVE: could not park on Sheet1.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }
    let pb_a = build("Sheet2!B2", QUALIFIED, "Qualified");
    run_leg("replaying Sheet2!B2 while showing Sheet1", &pb_a);
    if let Err(e) = store::store(&mut conn, &pb_a) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }
    match paradigm_lib::replay::replay(&mut conn, &desktop, &pb_a.id).await {
        Ok(r) => {
            println!("     status: {}", r.status);
            for o in &r.outcomes {
                println!("     [{}] {}", o.step_order, o.result.label());
                for l in o.detail.lines() {
                    println!("        {l}");
                }
            }
        }
        Err(e) => {
            eprintln!("replay failed to run: {e}");
            return ExitCode::FAILURE;
        }
    }

    // ---- leg B: bare, from Sheet1 -------------------------------------------
    println!("\n================ LEG B: bare step (regression) ================");
    let parked = goto_sheet_via_namebox(&desktop, "Sheet1!A1").await;
    println!("  parked back on gid {parked:?}");
    if parked.as_deref() != Some("0") {
        println!("\n  INCONCLUSIVE: could not park back on Sheet1 for the regression leg.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }
    let pb_b = build("C3", BARE, "Bare");
    run_leg("replaying bare C3 while showing Sheet1", &pb_b);
    if let Err(e) = store::store(&mut conn, &pb_b) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }
    match paradigm_lib::replay::replay(&mut conn, &desktop, &pb_b.id).await {
        Ok(r) => {
            println!("     status: {}", r.status);
            for o in &r.outcomes {
                println!("     [{}] {}", o.step_order, o.result.label());
                for l in o.detail.lines() {
                    println!("        {l}");
                }
            }
        }
        Err(e) => {
            eprintln!("replay failed to run: {e}");
            return ExitCode::FAILURE;
        }
    }

    // ---- ground truth --------------------------------------------------------
    println!("\n  waiting 10s for Sheets to sync before exporting...");
    tokio::time::sleep(Duration::from_secs(10)).await;
    println!("\n================ GROUND TRUTH (per sheet) ================\n");
    let csv1 = download_csv(browser, &doc_id, "0").await.unwrap_or_default();
    println!("  Sheet1 (gid=0):");
    for l in csv1.lines().take(6) {
        println!("    {l:?}");
    }
    let csv2 = download_csv(browser, &doc_id, &gid_sheet2)
        .await
        .unwrap_or_default();
    println!("  Sheet2 (gid={gid_sheet2}):");
    for l in csv2.lines().take(6) {
        println!("    {l:?}");
    }

    let q_on1 = csv1.contains(QUALIFIED);
    let q_on2 = csv2.contains(QUALIFIED);
    let b_on1 = csv1.contains(BARE);
    let b_on2 = csv2.contains(BARE);

    println!("\n================ VERDICT ================\n");
    println!("  qualified marker on Sheet1 : {q_on1}   (must be false)");
    println!("  qualified marker on Sheet2 : {q_on2}   (must be TRUE)");
    println!("  bare marker on Sheet1      : {b_on1}   (must be TRUE)");
    println!("  bare marker on Sheet2      : {b_on2}   (must be false)");
    println!(
        "  bare landed at C3          : {:?}",
        csv_at(&csv1, 3, 3)
    );

    let fix_ok = q_on2 && !q_on1;
    let regression_ok = b_on1 && !b_on2;
    println!();
    match (fix_ok, regression_ok) {
        (true, true) => {
            println!("  BOTH PASS. A qualified step crosses to the recorded sheet, and a bare");
            println!("  step still writes to whatever sheet is showing -- unchanged.");
        }
        (false, true) => {
            println!("  FIX FAILED, regression fine. The qualified step did not land on the");
            println!("  recorded sheet.");
        }
        (true, false) => {
            println!("  FIX WORKS but BARE BEHAVIOUR CHANGED. A recording that never switches");
            println!("  sheets no longer behaves as before -- that is a regression.");
        }
        (false, false) => println!("  BOTH FAILED."),
    }

    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ------------------------------------------------------ sheetsstamp mode ----
//
// Verifies the CAPTURE half of sheet tracking, which needs a human: no probe in
// this investigation has ever made a synthetic click land on a sheet tab, and
// the tracking is driven entirely by that click.
//
// Drives nothing. Starts a capture session, waits while a person switches sheets
// by clicking a tab and then edits a cell, and reports whether the resulting
// grid action was stamped `Sheet<N>!<cell>` rather than a bare cell.
//
// Same guards as `sheetsmanual`: the gid is read either side, so a recording
// with no actual sheet switch in it is reported as inconclusive rather than
// being read as a tracking failure.
async fn sheetsstamp_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};
    use std::io::Write;

    fn say(line: &str) {
        println!("{line}");
        let _ = std::io::stdout().flush();
    }

    let wait_secs: u64 = std::env::args()
        .filter_map(|a| a.parse::<u64>().ok())
        .find(|n| (10..=600).contains(n))
        .unwrap_or(60);

    say("== does capture stamp a cell edit with its sheet? ==\n");
    say("This probe clicks nothing. You perform the gesture; it only watches.\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((window, doc_id)) = sheets_window(&desktop).await else {
        say("  INCONCLUSIVE: no Sheets window open. Open a spreadsheet first.");
        return ExitCode::FAILURE;
    };
    say(&format!("  document: {doc_id}"));

    if !ensure_second_sheet(&desktop).await {
        say("\n  INCONCLUSIVE: could not get a second sheet to switch to.");
        say(&format!("\n  DOCUMENT ID for cleanup: {doc_id}"));
        return ExitCode::FAILURE;
    }
    // Park on Sheet1 so the switch under test is a real change.
    goto_sheet_via_namebox(&desktop, "Sheet1!A1").await;
    let gid_before = current_gid(&desktop).await;
    let tabs = tab_bar_buttons(&desktop, &window).await;
    say(&format!("  showing gid {gid_before:?}, tabs {tabs:?}"));

    say("\n================ WHAT TO DO ================\n");
    say("  When recording starts below:");
    say("    1. CLICK the 'Sheet2' tab at the bottom");
    say("    2. Type a short value into a cell");
    say("    3. Press Tab to commit it");
    say("  Then stop touching the machine.\n");
    say("  Use the MOUSE for step 1. The Name Box or a keyboard shortcut would");
    say("  switch the sheet without producing the click the tracking needs.\n");

    for n in (1..=5).rev() {
        say(&format!("  starting in {n}..."));
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let session = match CaptureSession::start_session(
        "sheets-stamp",
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
    say(&format!("\n  >>> RECORDING NOW -- go ahead. {wait_secs}s <<<\n"));
    let mut left = wait_secs;
    while left > 0 {
        let step = left.min(5);
        tokio::time::sleep(Duration::from_secs(step)).await;
        left -= step;
        if left > 0 {
            say(&format!("      {left}s left..."));
        }
    }
    say("\n  >>> STOPPED <<<\n");

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    let gid_after = current_gid(&desktop).await;

    say("================ DID THE SWITCH HAPPEN? ================\n");
    say(&format!("  gid before: {gid_before:?}"));
    say(&format!("  gid after : {gid_after:?}"));
    let switched = gid_before.is_some() && gid_after.is_some() && gid_before != gid_after;
    if !switched {
        say("  !! the gid did NOT change, so no sheet switch happened. Nothing below");
        say("     says anything about whether tracking works.");
    }

    say("\n================ WHAT CAPTURE PRODUCED ================\n");
    say(&format!(
        "  {} action(s), {} exclusion(s), {} unmapped event(s)\n",
        report.actions.len(),
        report.exclusions.len(),
        report.unmapped_events
    ));
    for (i, a) in report.actions.iter().enumerate() {
        say(&format!(
            "  [{}] {:<9} role={:?} name={:?} payload={:?}",
            i + 1,
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        ));
    }

    say("\n================ VERDICT ================\n");
    let grid_edits: Vec<_> = report
        .actions
        .iter()
        .filter(|a| a.kind.as_str() == "type" && a.element_role.as_deref() == Some("ComboBox"))
        .collect();
    if grid_edits.is_empty() {
        say("  no grid cell edit was captured, so there is nothing to have stamped.");
        say("  (Was the value typed into a CELL, and committed with Tab?)");
    }
    let stamped: Vec<&str> = grid_edits
        .iter()
        .filter_map(|a| a.element_name.as_deref())
        .filter(|n| n.contains('!'))
        .collect();
    for a in &grid_edits {
        let name = a.element_name.as_deref().unwrap_or("-");
        let (sheet, cell) = paradigm_lib::capture::grid::split_sheet_ref(name);
        say(&format!(
            "  cell edit {name:?}  ->  sheet={sheet:?} cell={cell:?}"
        ));
    }
    say("");
    if switched && !grid_edits.is_empty() && !stamped.is_empty() {
        say("  STAMPED. Capture recorded which sheet the edit happened on, which is");
        say("  the half that had no data source until the tab click supplied it.");
    } else if switched && !grid_edits.is_empty() {
        say("  NOT STAMPED. A switch happened and an edit was captured, but the edit");
        say("  carries a bare cell -- the tab click was not recognised. Check the");
        say("  captured click's role and name above against looks_like_sheet_tab.");
    } else {
        say("  INCONCLUSIVE -- see above. Not evidence either way.");
    }

    say(&format!("\n  DOCUMENT ID for cleanup: {doc_id}"));
    ExitCode::SUCCESS
}

// ------------------------------------------------------ sheetsactive mode ----
//
// The last capture-side question, asked of the WHOLE window rather than the tab
// bar: is the active sheet's identity anywhere in the accessibility tree?
//
// Everything so far looked at the sheet tabs and asked "which one is marked
// selected" -- answered no, twice, the second time properly via
// `is_selected()`/SelectionItemPattern. But a sheet name could be exposed
// somewhere else entirely: a status line, a heading, an accessible description,
// a hidden label.
//
// So this takes the opposite approach. Snapshot every element in the window on
// one sheet, snapshot again on another, and diff. Anything that appears,
// disappears, or changes value across the switch is a candidate signal. If the
// diff is empty of anything naming a sheet, the tree genuinely does not carry it
// and capture has no source -- concluded from the whole window, not one corner.
async fn sheetsactive_mode() -> ExitCode {
    println!("== is the ACTIVE sheet's name anywhere in the tree? ==\n");
    println!("Diffs the whole window across a sheet switch, rather than assuming");
    println!("the signal would live on the tabs.\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((_w, doc_id)) = sheets_window(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window open.");
        return ExitCode::FAILURE;
    };
    println!("  document: {doc_id}");
    if !ensure_second_sheet(&desktop).await {
        println!("\n  INCONCLUSIVE: could not get a second sheet.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }

    /// Every (role, name, value) triple in the window, budgeted.
    fn census(el: &UIElement, depth: usize, out: &mut Vec<String>, budget: &mut usize) {
        if *budget == 0 || depth > 14 {
            return;
        }
        *budget -= 1;
        let a = el.attributes();
        let name = a.name.unwrap_or_default();
        let value = a.value.unwrap_or_default();
        let desc = a.description.unwrap_or_default();
        if !name.is_empty() || !value.is_empty() || !desc.is_empty() {
            out.push(format!("{}|{}|{}|{}", a.role, name, value, desc));
        }
        if let Ok(kids) = el.children() {
            for k in kids {
                census(&k, depth + 1, out, budget);
            }
        }
    }

    async fn snap(desktop: &Desktop, label: &str) -> Vec<String> {
        let Some((w, _)) = sheets_window(desktop).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut budget = 8000usize;
        census(&w, 0, &mut out, &mut budget);
        println!("  {label}: {} elements with text ({} budget left)", out.len(), budget);
        out
    }

    goto_sheet_via_namebox(&desktop, "Sheet1!A1").await;
    let gid1 = current_gid(&desktop).await;
    let a = snap(&desktop, &format!("on Sheet1 (gid={gid1:?})")).await;

    goto_sheet_via_namebox(&desktop, "Sheet2!A1").await;
    let gid2 = current_gid(&desktop).await;
    let b = snap(&desktop, &format!("on Sheet2 (gid={gid2:?})")).await;

    if gid1 == gid2 || gid1.is_none() || gid2.is_none() {
        println!("\n  INCONCLUSIVE: the sheet did not actually change between snapshots.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }

    let only_a: Vec<&String> = a.iter().filter(|x| !b.contains(x)).collect();
    let only_b: Vec<&String> = b.iter().filter(|x| !a.contains(x)).collect();

    println!("\n================ WHAT CHANGED ================\n");
    println!("  present only while Sheet1 was active: {}", only_a.len());
    for x in only_a.iter().take(20) {
        println!("    {x}");
    }
    println!("\n  present only while Sheet2 was active: {}", only_b.len());
    for x in only_b.iter().take(20) {
        println!("    {x}");
    }

    let names_a_sheet = only_a.iter().any(|x| x.contains("Sheet1"));
    let names_b_sheet = only_b.iter().any(|x| x.contains("Sheet2"));

    println!("\n================ VERDICT ================\n");
    println!("  a Sheet1-naming element appears only when Sheet1 is active : {names_a_sheet}");
    println!("  a Sheet2-naming element appears only when Sheet2 is active : {names_b_sheet}");
    if names_a_sheet && names_b_sheet {
        println!("\n  A SIGNAL EXISTS. Capture can read the active sheet from it, and the");
        println!("  wrong-sheet fix becomes buildable end to end.");
    } else {
        println!("\n  NO SIGNAL. Nothing that names the active sheet appears or disappears");
        println!("  with the switch, across the WHOLE window -- not just the tab bar.");
        println!("  Capture has no source for which sheet an edit happened on.");
    }

    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ------------------------------------------------------ sheetsrename mode ----
//
// Rename an open spreadsheet, by `set_value` on the title field -- the same
// mechanism the Name Box uses, and the one measured reliable where clicks are
// not.
//
// Exists to undo a rename this investigation made. `sheetstrash` deliberately
// refuses any document whose title is not "Untitled spreadsheet", so that it can
// never delete real work; renaming a throwaway put it out of reach of its own
// cleanup. Renaming it back is the right repair -- loosening that guard to cover
// a name the probe itself invented would trade a real safety property for
// convenience.
//
// Usage: sheetsrename <doc-id> <new name...>
async fn sheetsrename_mode() -> ExitCode {
    let Some(id) = std::env::args().find(|a| {
        a.len() >= 40
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }) else {
        eprintln!("usage: sheetsrename <doc-id> <new name...>");
        return ExitCode::FAILURE;
    };
    let new_name: String = std::env::args()
        .skip_while(|a| a != "sheetsrename")
        .skip(1)
        .filter(|a| *a != id)
        .collect::<Vec<_>>()
        .join(" ");
    if new_name.trim().is_empty() {
        eprintln!("usage: sheetsrename <doc-id> <new name...>");
        return ExitCode::FAILURE;
    }
    println!("== rename {id} to {new_name:?} ==\n");

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            browser,
            "--new-window",
            &format!("https://docs.google.com/spreadsheets/d/{id}/edit"),
        ])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Google Sheets")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  no Sheets window found.");
        return ExitCode::FAILURE;
    };
    println!("  window: {:?}", window.name().unwrap_or_default());

    // The title field is the Edit whose text is the current document name.
    let current = window
        .name()
        .unwrap_or_default()
        .split(" - Google Sheets")
        .next()
        .unwrap_or("")
        .to_string();
    let Some(edit) = desktop
        .locator("role:Edit")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| {
            all.into_iter()
                .find(|e| e.text(0).unwrap_or_default().trim() == current.trim())
        })
    else {
        println!("  could not find the title field (looking for {current:?}).");
        return ExitCode::FAILURE;
    };

    let _ = edit.set_value(&new_name);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let _ = edit.press_key("{Enter}");
    tokio::time::sleep(Duration::from_secs(4)).await;

    let after = desktop
        .locator("role:Window|name:Google Sheets")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
        .and_then(|w| w.name())
        .unwrap_or_default();
    println!("  window title now: {after:?}");
    if after.contains(new_name.trim()) {
        println!("\n  RENAMED.");
        ExitCode::SUCCESS
    } else {
        println!("\n  the rename did not take.");
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------- sheetsselected mode ----
//
// Re-opens a question this investigation answered WRONG.
//
// "Q1: can capture read the active sheet?" was answered no, on the strength of
// `attributes().is_selected` being `None` on every tab in both states. That
// field is hardcoded `None` on Windows -- `platforms/windows/element.rs:553` --
// so it is `None` for every element ever, and the measurement established
// nothing. Absence of evidence, read as evidence of absence.
//
// `UIElement::is_selected()` (`element.rs:1261`) is a different thing entirely:
// it queries `UISelectionItemPattern` live. This asks that method instead, and
// checks the answer MOVES when the sheet changes -- a marker that does not move
// is not an active-sheet signal, whatever it reports.
async fn sheetsselected_mode() -> ExitCode {
    println!("== does is_selected() identify the active sheet? ==\n");
    println!("The earlier 'no' came from attributes().is_selected, which is hardcoded");
    println!("None on Windows. This asks the real accessor.\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((_window, doc_id)) = sheets_window(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window open.");
        return ExitCode::FAILURE;
    };
    println!("  document: {doc_id}");

    if !ensure_second_sheet(&desktop).await {
        println!("\n  INCONCLUSIVE: could not get a second sheet.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }

    /// Every candidate element in the tab bar, with what it is.
    async fn tab_elements(desktop: &Desktop, window: &UIElement) -> Vec<(String, UIElement)> {
        let mut out = Vec::new();
        if let Some(bar) = find_named(desktop, window, &["role:Group"], |n| {
            n.trim() == "Sheet tab bar"
        })
        .await
        {
            fn walk(el: &UIElement, depth: usize, out: &mut Vec<(String, UIElement)>) {
                if depth > 6 {
                    return;
                }
                let name = el.name().unwrap_or_default();
                let t = name.trim();
                if t.len() > 5 && t.starts_with("Sheet") && t[5..].chars().all(|c| c.is_ascii_digit())
                {
                    out.push((format!("{} {:?}", el.role(), t), el.clone()));
                }
                if let Ok(kids) = el.children() {
                    for k in kids {
                        walk(&k, depth + 1, out);
                    }
                }
            }
            walk(&bar, 0, &mut out);
        }
        out
    }

    async fn snapshot(desktop: &Desktop, label: &str) -> Vec<(String, String)> {
        let Some((window, _)) = sheets_window(desktop).await else {
            return Vec::new();
        };
        let els = tab_elements(desktop, &window).await;
        println!("\n  -- {label} --");
        let mut out = Vec::new();
        for (what, el) in els {
            let sel = match el.is_selected() {
                Ok(true) => "Ok(true)".to_string(),
                Ok(false) => "Ok(false)".to_string(),
                Err(e) => format!("Err({e})"),
            };
            println!("    {what:<24} is_selected() = {sel}");
            out.push((what, sel));
        }
        out
    }

    // Park on Sheet1, snapshot; move to Sheet2, snapshot. The Name Box is used
    // to switch because it is the mechanism measured working -- clicks are not.
    goto_sheet_via_namebox(&desktop, "Sheet1!A1").await;
    let gid1 = current_gid(&desktop).await;
    let on_sheet1 = snapshot(&desktop, &format!("showing Sheet1 (gid={gid1:?})")).await;

    goto_sheet_via_namebox(&desktop, "Sheet2!A1").await;
    let gid2 = current_gid(&desktop).await;
    let on_sheet2 = snapshot(&desktop, &format!("showing Sheet2 (gid={gid2:?})")).await;

    println!("\n================ VERDICT ================\n");
    if gid1 == gid2 || gid1.is_none() || gid2.is_none() {
        println!("  INCONCLUSIVE: the document did not actually change sheets between the");
        println!("  two snapshots, so nothing below distinguishes anything.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }
    let any_true = on_sheet1.iter().chain(on_sheet2.iter()).any(|(_, s)| s == "Ok(true)");
    let moved = on_sheet1 != on_sheet2;
    println!("  any element reported Ok(true) : {any_true}");
    println!("  the answers MOVED with the sheet: {moved}");
    if any_true && moved {
        println!("\n  USABLE. is_selected() identifies the active sheet, so capture CAN");
        println!("  record which sheet a cell edit happened on -- the blocker the whole");
        println!("  wrong-sheet investigation has been stuck behind.");
    } else if !any_true {
        println!("\n  NOT USABLE. Nothing reports Ok(true) in either state -- Sheets' tabs");
        println!("  do not implement SelectionItemPattern. The earlier conclusion stands,");
        println!("  now for a reason that was actually measured.");
    } else {
        println!("\n  NOT USABLE. Something reports Ok(true) but the answer does not change");
        println!("  with the sheet, so it is not an active-sheet signal.");
    }

    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ---------------------------------------------------- sheetsscopefix mode ----
//
// End-to-end check of window-scoped resolution: with TWO spreadsheets open,
// each owning a `Sheet1` tab, does a replay that navigates to one of them
// resolve the tab inside that window instead of refusing as ambiguous?
//
// The two documents need DISTINCT window titles, or the navigate step itself is
// ambiguous and the run never establishes a scope -- a different bug, and one
// this file already documents. So one document is renamed first, via `set_value`
// on the title field: the same mechanism the Name Box uses, chosen because
// clicks on Sheets chrome are measured unreliable.
async fn sheetsscopefix_mode() -> ExitCode {
    use paradigm_lib::capture::{ActionCandidate, ActionKind, CapturedStream, ExclusionList};
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    const RENAMED: &str = "ScopeProbeAlpha";
    println!("== window-scoped resolution, two spreadsheets, one Sheet1 each ==\n");

    let browser = browser_order()[0];
    let ids: Vec<String> = std::env::args()
        .filter(|a| {
            a.len() >= 40
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .collect();
    if ids.len() < 2 {
        eprintln!("pass TWO document ids: the one to rename, then the decoy");
        return ExitCode::FAILURE;
    }

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- document A, renamed so navigate can target it -----------------------
    println!("-- opening document A ({}) --", ids[0]);
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            browser,
            "--new-window",
            &format!("https://docs.google.com/spreadsheets/d/{}/edit", ids[0]),
        ])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(30)).await;

    // Idempotent: a previous run may already have renamed this document, in
    // which case the title field no longer reads "Untitled spreadsheet" and
    // there is nothing to do.
    let already = desktop
        .locator(format!("role:Window|name:{RENAMED}").as_str())
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), Some(3))
        .await
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    if already {
        println!("  document A is already named {RENAMED:?}; skipping the rename");
    } else {
        let Some((win_a, _)) = sheets_window(&desktop).await else {
            println!("  INCONCLUSIVE: no Sheets window.");
            return ExitCode::FAILURE;
        };
        // The title field is an Edit whose text is the current document name.
        let title_edit = desktop
            .locator("role:Edit")
            .within(win_a.clone())
            .all(Some(Duration::from_secs(5)), None)
            .await
            .ok()
            .and_then(|all| {
                all.into_iter().find(|e| {
                    let t = e.text(0).unwrap_or_default();
                    t.trim() == "Untitled spreadsheet"
                })
            });
        match &title_edit {
            Some(e) => {
                println!("  renaming document A to {RENAMED:?}");
                let _ = e.set_value(RENAMED);
                tokio::time::sleep(Duration::from_millis(600)).await;
                let _ = e.press_key("{Enter}");
                tokio::time::sleep(Duration::from_secs(4)).await;
            }
            None => {
                println!("  INCONCLUSIVE: could not find the title field to rename.");
                println!("  Without distinct titles the navigate step is itself ambiguous and");
                println!("  no scope is ever established, so this would test nothing.");
                return ExitCode::FAILURE;
            }
        }
    }

    // Look it up by its NEW title. `sheets_window` matches "Untitled
    // spreadsheet", so a successful rename makes it invisible to that helper --
    // which is what happened on the first attempt and read as "lost the window".
    let Some(win_a) = desktop
        .locator(format!("role:Window|name:{RENAMED}").as_str())
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  could not find a window titled {RENAMED:?} after renaming, so the");
        println!("  rename did not take. Without distinct titles this tests nothing.");
        return ExitCode::FAILURE;
    };
    let title_a = win_a.name().unwrap_or_default();
    println!("  document A window title now: {title_a:?}");
    if !title_a.contains(RENAMED) {
        println!("\n  INCONCLUSIVE: the rename did not take, so both windows still share a");
        println!("  title and the navigate step could not pick one.");
        return ExitCode::FAILURE;
    }

    // ---- document B, the decoy, left as "Untitled spreadsheet" --------------
    println!("\n-- opening document B, the decoy ({}) --", ids[1]);
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            browser,
            "--new-window",
            &format!("https://docs.google.com/spreadsheets/d/{}/edit", ids[1]),
        ])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(30)).await;

    // ---- prove the ambiguity exists before claiming to have fixed it --------
    let wide = desktop
        .locator("role:text|name:Sheet1")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .unwrap_or_default();
    let wide_exact = wide
        .iter()
        .filter(|e| {
            paradigm_lib::replay::resolved_is_recorded_target(
                "Sheet1",
                &e.name().unwrap_or_default(),
            )
        })
        .count();
    println!("\n  desktop-wide exact matches for Sheet1: {wide_exact}");
    if wide_exact < 2 {
        println!("\n  INCONCLUSIVE: fewer than two Sheet1 tabs on the desktop, so there is");
        println!("  no ambiguity to resolve and a pass would prove nothing.");
        return ExitCode::FAILURE;
    }

    // ---- the playbook: navigate to A, then click ITS Sheet1 tab -------------
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
    stream.admit(ActionCandidate {
        kind: ActionKind::Navigate,
        identifiers: vec![title_a.clone()],
        process_name: Some("msedge.exe".into()),
        element_role: Some("Window".into()),
        element_name: Some(title_a.clone()),
        payload: None,
        detail: None,
        timestamp_ms: 0,
    });
    stream.admit(ActionCandidate {
        kind: ActionKind::Click,
        identifiers: vec![title_a.clone()],
        process_name: Some("msedge.exe".into()),
        element_role: Some("text".into()),
        element_name: Some("Sheet1".into()),
        payload: None,
        detail: None,
        timestamp_ms: 1,
    });
    let playbook = compile(
        &stream.actions().to_vec(),
        "Scope Fix",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    println!("\n  playbook steps:");
    for s in &playbook.steps {
        let sel = serde_json::from_str::<serde_json::Value>(&s.action_payload_json)
            .ok()
            .and_then(|v| v["target"]["selector"].as_str().map(str::to_string))
            .unwrap_or_else(|| "<none>".into());
        println!("    [{}] {:<9} selector={sel:?}", s.step_order, s.action_type);
    }

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
    if let Err(e) = store::store(&mut conn, &playbook) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }

    println!("\n================ REPLAY ================\n");
    let run = match paradigm_lib::replay::replay(&mut conn, &desktop, &playbook.id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay failed to run: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("  status: {}", run.status);
    for o in &run.outcomes {
        println!("  [{}] {:<9} {}", o.step_order, o.action_type, o.result.label());
        for line in o.detail.lines() {
            println!("       {line}");
        }
    }

    println!("\n================ VERDICT ================\n");
    let nav_ok = run
        .outcomes
        .iter()
        .find(|o| o.action_type == "navigate")
        .map(|o| !o.result.is_failure())
        .unwrap_or(false);
    let click = run.outcomes.iter().find(|o| o.action_type == "click");
    let click_label = click.map(|o| o.result.label()).unwrap_or("<none>");
    let click_ambiguous = click_label == "failed_ambiguous";

    println!("  {wide_exact} Sheet1 tabs on the desktop -- desktop-wide this IS ambiguous");
    println!("  navigate established a scope : {nav_ok}");
    println!("  click outcome                : {click_label}");
    if nav_ok && !click_ambiguous {
        println!("\n  FIXED. The click resolved inside the navigated window instead of");
        println!("  refusing across the desktop. (Whether the click then activates the");
        println!("  tab is a separate, already-measured question.)");
    } else if !nav_ok {
        println!("\n  INCONCLUSIVE: navigate did not succeed, so no scope was set and the");
        println!("  click was never scoped. This does not test the fix.");
    } else {
        println!("\n  NOT FIXED: the click is still ambiguous with a scope in place.");
    }

    println!("\n  DOCUMENT IDs for cleanup: {} {}", ids[0], ids[1]);
    ExitCode::SUCCESS
}

// ------------------------------------------------------- sheetsscope mode ----
//
// Would `StepPayload::scoped_selector` fix the ambiguity that made a tab-click
// replay refuse? It builds `process:<name>|<selector>`, and the ambiguity was
// two Sheets documents each owning a `Sheet1` tab.
//
// The premise is worth testing before implementing, because both documents live
// in the SAME browser process. If that is so, a process prefix cannot separate
// them and wiring it in would change nothing while looking like a fix.
//
// Measured directly: the desktop-wide count replay actually uses, against the
// process-scoped count the proposed fix would use.
async fn sheetsscope_mode() -> ExitCode {
    println!("== can a process: prefix separate two spreadsheets? ==\n");
    println!("Needs TWO Sheets documents open, each with a Sheet1 tab.\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Which windows are open, and what process owns each.
    let windows = desktop
        .locator("role:Window|name:Google Sheets")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .unwrap_or_default();
    println!("  Sheets windows open: {}", windows.len());
    for w in &windows {
        let app = w
            .application()
            .ok()
            .flatten()
            .and_then(|a| a.name())
            .unwrap_or_else(|| "-".into());
        println!("    {:?}  application={app:?}", w.name().unwrap_or_default());
    }
    if windows.len() < 2 {
        println!("\n  INCONCLUSIVE: need two Sheets documents open to test this.");
        return ExitCode::FAILURE;
    }

    // 1. The desktop-wide resolution replay actually performs today.
    println!("\n================ desktop-wide (what replay does) ================\n");
    let wide = desktop
        .locator("role:text|name:Sheet1")
        .within(desktop.root())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .unwrap_or_default();
    let wide_exact: Vec<_> = wide
        .iter()
        .filter(|e| {
            paradigm_lib::replay::resolved_is_recorded_target(
                "Sheet1",
                &e.name().unwrap_or_default(),
            )
        })
        .collect();
    println!("  role:text|name:Sheet1");
    println!("    raw matches          : {}", wide.len());
    println!("    exact-name matches   : {}", wide_exact.len());
    for e in wide_exact.iter().take(6) {
        let win = e
            .window()
            .ok()
            .flatten()
            .and_then(|w| w.name())
            .unwrap_or_else(|| "-".into());
        println!("      in window {win:?}");
    }

    // 2. What the proposed fix would resolve instead.
    println!("\n================ process-scoped (the proposed fix) ================\n");
    for proc_name in ["msedge.exe", "chrome.exe"] {
        let sel = format!("process:{proc_name}|role:text|name:Sheet1");
        match desktop
            .locator(sel.as_str())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            Ok(found) => {
                let exact = found
                    .iter()
                    .filter(|e| {
                        paradigm_lib::replay::resolved_is_recorded_target(
                            "Sheet1",
                            &e.name().unwrap_or_default(),
                        )
                    })
                    .count();
                println!("  {sel}");
                println!("    raw matches        : {}", found.len());
                println!("    exact-name matches : {exact}");
                for e in found.iter().take(6) {
                    println!(
                        "      role={:<10} name={:?}",
                        e.role(),
                        e.name().unwrap_or_default()
                    );
                }
            }
            Err(e) => println!("  {sel}\n    error: {e}"),
        }
    }

    println!("\n================ VERDICT ================\n");
    println!("  If both documents are in the same process, a process: prefix cannot");
    println!("  separate them, and the exact-name count above will be unchanged.");
    ExitCode::SUCCESS
}

// --------------------------------------------------- sheetsreplaytab mode ----
//
// One narrow question, and nothing else: does a SYNTHETIC click on the element
// the recorder actually captured -- `role:text|name:Sheet1`, the Text node
// inside the tab button -- switch sheets?
//
// It is worth asking separately because the automated runs only ever clicked at
// the `Button` level, and those failed. The recorder attributes the click one
// level deeper. See "Route 1, answered by a human click" in
// docs/known-issues/complex-web-grid-capture-unreliable.md.
//
// The playbook is built from the real captured shape, through the real
// exclusion gate and the real compiler, so the selector under test is the one
// replay would genuinely resolve -- not a hand-written string.
//
// Success is the gid moving to 0. That is the direct observable for "the sheet
// changed", needs no typing, and cannot be faked by a step reporting Ok.
async fn sheetsreplaytab_mode() -> ExitCode {
    use paradigm_lib::capture::{ActionCandidate, ActionKind, CapturedStream, ExclusionList};
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    println!("== does replay's click on role:text|name:Sheet1 switch sheets? ==\n");

    let browser = browser_order()[0];
    let doc_arg = std::env::args().find(|a| {
        a.len() >= 40
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    });
    let url = match &doc_arg {
        Some(id) => format!("https://docs.google.com/spreadsheets/d/{id}/edit"),
        None => "https://sheets.new".to_string(),
    };
    println!("  opening {url}");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", &url])
        .spawn()
    {
        let _ = c.wait();
    }
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((window, doc_id)) = sheets_window(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    println!("  document: {doc_id}");

    // The target must exist, and the document must NOT already be on it.
    if !ensure_second_sheet(&desktop).await {
        println!("\n  INCONCLUSIVE: could not get a second sheet.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }
    let mut gid_before = current_gid(&desktop).await;
    if gid_before.as_deref() == Some("0") {
        println!("  on Sheet1 already; moving to Sheet2 so a switch is required");
        gid_before = goto_sheet_via_namebox(&desktop, "Sheet2!A1").await;
    }
    println!("  showing gid before replay: {gid_before:?}");
    if gid_before.as_deref() == Some("0") || gid_before.is_none() {
        println!("\n  INCONCLUSIVE: could not park the document off Sheet1, so a replay");
        println!("  that changed nothing would be indistinguishable from one that worked.");
        println!("\n  DOCUMENT ID for cleanup: {doc_id}");
        return ExitCode::FAILURE;
    }

    // Is the captured element even resolvable here?
    let matches = desktop
        .locator("role:text|name:Sheet1")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    println!("  elements matching role:text|name:Sheet1 : {matches}");

    // ---- the playbook, built from the REAL captured shape -------------------
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
    stream.admit(ActionCandidate {
        kind: ActionKind::Click,
        identifiers: vec!["msedge.exe".into()],
        process_name: Some("msedge.exe".into()),
        // Exactly what sheetsmanual recorded from the human click.
        element_role: Some("text".into()),
        element_name: Some("Sheet1".into()),
        payload: None,
        detail: None,
        timestamp_ms: 0,
    });
    let actions = stream.actions().to_vec();
    if actions.is_empty() {
        println!("  the action did not survive the exclusion gate; cannot test.");
        return ExitCode::FAILURE;
    }
    let playbook = compile(
        &actions,
        "Replay Tab Click",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    for s in &playbook.steps {
        let sel = serde_json::from_str::<serde_json::Value>(&s.action_payload_json)
            .ok()
            .and_then(|v| v["target"]["selector"].as_str().map(str::to_string))
            .unwrap_or_else(|| "<none>".into());
        println!("  step [{}] {} selector={sel:?}", s.step_order, s.action_type);
    }

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
    if let Err(e) = store::store(&mut conn, &playbook) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }

    // ---- replay -------------------------------------------------------------
    println!("\n================ REPLAY ================\n");
    let run = match paradigm_lib::replay::replay(&mut conn, &desktop, &playbook.id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay failed to run: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("  status: {}", run.status);
    for o in &run.outcomes {
        println!("  [{}] {:<9} {}", o.step_order, o.action_type, o.result.label());
        for line in o.detail.lines() {
            println!("       {line}");
        }
    }

    tokio::time::sleep(Duration::from_secs(3)).await;
    let gid_after = current_gid(&desktop).await;

    println!("\n================ VERDICT ================\n");
    println!("  gid before : {gid_before:?}");
    println!("  gid after  : {gid_after:?}");

    // "The sheet did not change" has two completely different causes and the
    // first version of this collapsed them, reporting "replay clicked and the
    // document stayed put" for a run where replay REFUSED and never clicked.
    // Whether a click was actually attempted is the thing that decides which
    // question the run answered.
    let clicked = run.outcomes.iter().any(|o| !o.result.is_failure());
    let switched = gid_after.as_deref() == Some("0");

    match (clicked, switched) {
        (true, true) => {
            println!("\n  SWITCHED. A synthetic click on the captured Text node DOES change");
            println!("  sheets, even though Button-level clicks did not. Route 1 works end");
            println!("  to end for the explicit-switch case.");
        }
        (true, false) => {
            println!("\n  CLICKED, BUT DID NOT SWITCH. Replay acted on the recorded element");
            println!("  and the document stayed put -- the same failure Button-level clicks");
            println!("  had. Capture is not the blocker; the click is.");
        }
        (false, _) => {
            println!("\n  UNANSWERED -- replay never clicked. Every step failed before acting,");
            println!("  so nothing was learned about whether the click would have worked.");
            println!("  See the step detail above for why it refused. This is NOT evidence");
            println!("  that the click fails.");
        }
    }
    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

// ---------------------------------------------------- sheetsmanual mode ----
//
// The one gesture five automated runs could not produce: a HUMAN clicking a
// sheet tab. `sheetstabclick` could never make a synthetic click land on the
// tab bar, so what capture does with a real tab click is still unmeasured --
// see "Route 1 attempted" in
// docs/known-issues/complex-web-grid-capture-unreliable.md.
//
// This probe drives NOTHING. It finds the open Sheets window, starts a real
// capture session, waits while a person switches sheets and types, then prints
// exactly what capture produced. The probe not touching the mouse is the entire
// point: any click it made would be the thing already known not to work.
//
// It also reads the gid before and after. If the sheet did not actually change,
// the recording is of some other gesture and the run says so rather than
// letting a conclusion be drawn from it.

/// Names of the buttons in the sheet tab bar, excluding its controls.
///
/// Not `sheet_tabs`, which only matches `Sheet<digits>`: a real user's document
/// has tabs called things like "Data" or "Q3", and this probe is meant to be
/// pointed at whatever is already open.
async fn tab_bar_buttons(desktop: &Desktop, window: &UIElement) -> Vec<String> {
    let Some(bar) = find_named(desktop, window, &["role:Group"], |n| {
        n.trim() == "Sheet tab bar"
    })
    .await
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Ok(toolbars) = bar.children() {
        for tb in toolbars {
            if let Ok(kids) = tb.children() {
                for k in kids {
                    if k.role() == "Button" {
                        if let Some(n) = k.name() {
                            let t = n.trim().to_string();
                            if !t.is_empty()
                                && t != "Add Sheet"
                                && t != "All Sheets"
                                && !out.contains(&t)
                            {
                                out.push(t);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

async fn sheetsmanual_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};
    use paradigm_lib::compile::{compile, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;
    use std::io::Write;

    fn say(line: &str) {
        println!("{line}");
        let _ = std::io::stdout().flush();
    }

    // Seconds to leave the recorder running. Override by passing a number.
    let wait_secs: u64 = std::env::args()
        .filter_map(|a| a.parse::<u64>().ok())
        .find(|n| (10..=600).contains(n))
        .unwrap_or(45);

    say("== capture a HUMAN sheet-tab click ==\n");
    say("This probe clicks nothing. You perform the gesture; it only watches.");
    say("Point it at a Sheets document that ALREADY has two sheets.\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Any Sheets window, not just "Untitled spreadsheet" -- this is meant to be
    // pointed at a real document.
    let Some(window) = desktop
        .locator("role:Window|name:Google Sheets")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        say("  INCONCLUSIVE: no window titled '… Google Sheets' is open.");
        say("  Open a Sheets document with two sheets, then run this again.");
        return ExitCode::FAILURE;
    };
    say(&format!("  window : {:?}", window.name().unwrap_or_default()));

    let addr_before = address_of(&desktop, &window).await;
    let doc_id = addr_before
        .split("/d/")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("")
        .to_string();
    let gid_before = gid_in(&addr_before);
    say(&format!("  document: {doc_id}"));
    say(&format!("  showing gid: {gid_before:?}"));

    let tabs = tab_bar_buttons(&desktop, &window).await;
    say(&format!("  sheet tabs visible: {tabs:?}"));
    if tabs.len() < 2 {
        say("\n  WARNING: fewer than two sheet tabs were found. If the document only");
        say("  has one sheet there is nothing to switch to, and the recording will");
        say("  not answer the question. Add a second sheet first (Shift+F11).");
    }

    // ---- what to do ---------------------------------------------------------
    say("\n================ WHAT TO DO ================\n");
    say("  When recording starts below:");
    say("    1. CLICK the other sheet's tab at the bottom (the gesture under test)");
    say("    2. Type a short value into a cell");
    say("    3. Press Tab to commit it");
    say("  Then stop touching the machine and wait for the report.\n");
    say("  Do NOT use the Name Box or a keyboard shortcut to switch sheets --");
    say("  a click on the tab is the specific thing being measured.\n");

    for n in (1..=5).rev() {
        say(&format!("  starting in {n}..."));
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let session = match CaptureSession::start_session(
        "sheets-manual-tab-click",
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

    say(&format!(
        "\n  >>> RECORDING NOW -- go ahead. {wait_secs}s <<<\n"
    ));
    let mut left = wait_secs;
    while left > 0 {
        let step = left.min(5);
        tokio::time::sleep(Duration::from_secs(step)).await;
        left -= step;
        if left > 0 {
            say(&format!("      {left}s left..."));
        }
    }
    say("\n  >>> STOPPED <<<\n");

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- did the sheet actually change? -------------------------------------
    let addr_after = address_of(&desktop, &window).await;
    let gid_after = gid_in(&addr_after);
    say("================ DID THE SWITCH HAPPEN? ================\n");
    say(&format!("  gid before: {gid_before:?}"));
    say(&format!("  gid after : {gid_after:?}"));
    let switched = gid_before.is_some() && gid_after.is_some() && gid_before != gid_after;
    if switched {
        say("  the document changed sheets, so the recording contains a real switch.");
    } else {
        say("  !! the gid did NOT change, so no sheet switch happened during the");
        say("     recording. Whatever is below is a capture of some other gesture,");
        say("     and nothing about tab clicks should be concluded from it.");
    }

    // ---- what capture produced ---------------------------------------------
    say("\n================ WHAT CAPTURE PRODUCED ================\n");
    say(&format!(
        "  {} action(s), {} exclusion(s), {} unmapped event(s)",
        report.actions.len(),
        report.exclusions.len(),
        report.unmapped_events
    ));
    if report.actions.is_empty() && report.unmapped_events == 0 {
        say("  !! the recorder observed NO events at all -- this run is broken, not");
        say("     evidence about what capture does with a tab click.");
    }
    say("");
    for (i, a) in report.actions.iter().enumerate() {
        say(&format!(
            "  [{}] {:<9} role={:?} name={:?} payload={:?}",
            i + 1,
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        ));
    }
    for e in report.exclusions.iter().take(12) {
        say(&format!("      excluded {:?} {:?}", e.kind.as_str(), e.reason));
    }

    // ---- the selectors replay would actually resolve ------------------------
    let replayable: Vec<_> = report
        .actions
        .iter()
        .filter(|a| a.kind.as_str() == "click" || a.kind.as_str() == "type")
        .cloned()
        .collect();
    say("\n================ COMPILED SELECTORS ================\n");
    if replayable.is_empty() {
        say("  no click or type actions to compile.");
    } else {
        let pb = compile(
            &replayable,
            "Manual Tab Click",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        for s in &pb.steps {
            let sel = serde_json::from_str::<serde_json::Value>(&s.action_payload_json)
                .ok()
                .and_then(|v| v["target"]["selector"].as_str().map(str::to_string))
                .unwrap_or_else(|| "<none>".into());
            say(&format!(
                "  [{}] {:<9} selector={sel:?}",
                s.step_order, s.action_type
            ));
        }
    }

    // ---- the actual question ------------------------------------------------
    say("\n================ THE TAB CLICK ================\n");
    let named_tab: Vec<_> = report
        .actions
        .iter()
        .filter(|a| a.kind.as_str() == "click")
        .filter(|a| {
            a.element_name
                .as_deref()
                .map(|n| tabs.iter().any(|t| t == n.trim()))
                .unwrap_or(false)
        })
        .collect();

    if let Some(a) = named_tab.first() {
        say("  CAPTURED, and it names the tab:");
        say(&format!(
            "    role={:?} name={:?}",
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-")
        ));
        say("\n  That is a replayable identification of a specific sheet, which is");
        say("  what route 1 needs. Next step is replaying it into a fresh document.");
    } else {
        let clicks: Vec<_> = report
            .actions
            .iter()
            .filter(|a| a.kind.as_str() == "click")
            .collect();
        if clicks.is_empty() {
            say("  no click action was captured at all.");
        } else {
            say("  NO captured click names a sheet tab. The clicks that were captured:");
            for a in clicks {
                say(&format!(
                    "    role={:?} name={:?}",
                    a.element_role.as_deref().unwrap_or("-"),
                    a.element_name.as_deref().unwrap_or("-")
                ));
            }
            say("\n  If the gid DID change above, then a real tab click happened and");
            say("  capture did not attribute it to the tab -- which settles route 1");
            say("  negatively, and this time on a genuine human gesture.");
        }
    }

    say(&format!("\n  document: {doc_id}"));
    ExitCode::SUCCESS
}

// ---------------------------------------------------- sheetstabclick mode ----
//
// Route 1 from "The sheet-name gap, measured": Sheets publishes no signal for
// WHICH sheet is active, but a sheet tab is an ordinary `Button "Sheet2"`. So a
// user who switches sheets during a recording may already produce a replayable
// click -- sidestepping the unreadable active-sheet state entirely.
//
// Three things to establish, in order, and the third is a limitation rather
// than a feature:
//
//   1. Does capture actually ADMIT the tab click, and with what selector?
//   2. Does REPLAYING it land the edit on the right sheet in a fresh document?
//      Ground truth is the per-sheet CSV export, never the UI.
//   3. Does it cover a recording that merely STARTS on a non-default sheet?
//      Measured directly rather than reasoned about, because this is the half
//      that decides whether "fixed" would be an overclaim.

/// The open Sheets window and its document id.
async fn sheets_window(desktop: &Desktop) -> Option<(UIElement, String)> {
    let w = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .ok()?
        .into_iter()
        .next()?;
    let addr = address_of(desktop, &w).await;
    let id = addr
        .split("/d/")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("")
        .to_string();
    Some((w, id))
}

/// Click a sheet tab by name. Returns the gid afterwards.
///
/// Re-resolves the window rather than taking one from the caller. The first
/// version held a handle across an "Add Sheet" click and every later read came
/// back empty -- `gid now None` -- which looked like a failed click but was a
/// stale element. Anything read after a navigation must be re-resolved.
async fn click_sheet_tab(desktop: &Desktop, name: &str) -> Option<String> {
    let (window, _) = sheets_window(desktop).await?;
    let tab = find_named(desktop, &window, &["role:Button"], |n| n.trim() == name).await?;
    robust_click(desktop, &tab);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (fresh, _) = sheets_window(desktop).await?;
    gid_in(&address_of(desktop, &fresh).await)
}

/// Switch sheets via the Name Box, using a qualified reference.
///
/// Setup only -- never for the step under test. Clicking a sheet tab was
/// measured failing repeatedly in this desktop state (the "Add Sheet" button
/// and the "Sheet1" tab both accepted a click that the page never saw), while
/// keyboard and Name Box entry kept working. Since a qualified reference was
/// already proven to cross sheets in "The sheet-name gap, measured", setup uses
/// the mechanism known to work and leaves the click to the hypothesis.
async fn goto_sheet_via_namebox(desktop: &Desktop, reference: &str) -> Option<String> {
    let (window, _) = sheets_window(desktop).await?;
    let _ = window.activate_window();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let name_box = desktop
        .locator("name:Name box")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()?
        .into_iter()
        .next()?
        .children()
        .ok()?
        .into_iter()
        .find(|e| e.role() == "Edit")?;
    let _ = name_box.set_value(reference);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let _ = name_box.press_key("{Enter}");
    tokio::time::sleep(Duration::from_millis(1800)).await;
    current_gid(desktop).await
}

/// The gid the open document is currently showing, freshly resolved.
async fn current_gid(desktop: &Desktop) -> Option<String> {
    let (w, _) = sheets_window(desktop).await?;
    gid_in(&address_of(desktop, &w).await)
}

/// The names of every sheet tab currently present, freshly resolved.
async fn sheet_tab_names(desktop: &Desktop) -> Vec<String> {
    let Some((window, _)) = sheets_window(desktop).await else {
        return Vec::new();
    };
    let mut names: Vec<String> = sheet_tabs(desktop, &window)
        .await
        .iter()
        .filter_map(|e| e.name())
        .map(|n| n.trim().to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Add a second sheet, and confirm it exists before returning.
///
/// Retries because a single click 30s after load is not reliable: the first
/// attempt at this reported "second sheet created, gid=0" against a document
/// that still had only `["Sheet1"]`. The `sheetsmulti` probe got away with one
/// click only because ~20s of unrelated tab scanning sat between load and
/// click. Waiting on the observable outcome beats guessing a delay.
/// Two strategies, alternated, because a click alone was measured failing four
/// times in a row against a document where `sheetsmulti` had succeeded: the
/// button click only reaches the web app when its window is foreground, and
/// `{shift}{f11}` is Sheets' own "insert sheet" shortcut and does not depend on
/// hit-testing a button at all.
async fn ensure_second_sheet(desktop: &Desktop) -> bool {
    for attempt in 1..=6 {
        if sheet_tab_names(desktop).await.iter().any(|n| n == "Sheet2") {
            return true;
        }
        if let Some((window, _)) = sheets_window(desktop).await {
            // Foreground first. A background window accepts the UIA call and
            // the page never sees it.
            let _ = window.activate_window();
            tokio::time::sleep(Duration::from_millis(700)).await;

            if attempt % 2 == 1 {
                if let Some(add) = find_named(desktop, &window, &["role:Button"], |n| {
                    n.trim().eq_ignore_ascii_case("Add Sheet")
                })
                .await
                {
                    robust_click(desktop, &add);
                }
            } else if let Ok(el) = desktop.focused_element() {
                let _ = el.press_key("{shift}{f11}");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        let tabs = sheet_tab_names(desktop).await;
        let how = if attempt % 2 == 1 { "click" } else { "shift+f11" };
        println!("    add-sheet attempt {attempt} ({how}): tabs now {tabs:?}");
        if tabs.iter().any(|n| n == "Sheet2") {
            return true;
        }
    }
    false
}

/// Put the caret in the grid, so typing reaches a cell rather than whatever
/// the last click left focused.
async fn focus_grid(desktop: &Desktop) {
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{home}");
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
}

async fn sheetstabclick_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    println!("== does a captured sheet-tab click replay onto the right sheet? ==\n");
    println!("Creates TWO throwaway spreadsheets. Both IDs are printed at the end.\n");

    let browser = browser_order()[0];
    let open_new_sheet = || async {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
            .spawn()
        {
            let _ = c.wait();
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    };

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    const MARKER: &str = "tabclickmarker";
    const MARKER2: &str = "nostartswitch";

    // ---- set up the RECORDING document --------------------------------------
    println!("-- opening the RECORDING document --");
    open_new_sheet().await;
    let Some((_rec_win, rec_id)) = sheets_window(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window. Not signed in, or it did not load.");
        return ExitCode::FAILURE;
    };
    println!("  recording document: {rec_id}");

    // A second sheet, created BEFORE capture starts so that creating it is not
    // itself part of the recording -- the question is about switching, not
    // about adding. Verified, not assumed.
    if !ensure_second_sheet(&desktop).await {
        println!("\n  INCONCLUSIVE: could not add a Sheet2, so nothing below would be");
        println!("  testing a sheet switch. Not reporting a result from it.");
        println!("\n  DOCUMENT ID for cleanup: {rec_id}");
        return ExitCode::FAILURE;
    }
    let rec_gid2 = current_gid(&desktop).await.unwrap_or_default();
    println!("  second sheet present, gid={rec_gid2}");

    // Back to the first sheet, still outside capture, so the recording has to
    // perform the switch itself.
    let back = goto_sheet_via_namebox(&desktop, "Sheet1!A1").await;
    println!("  switched back to Sheet1 before recording, gid={back:?}");
    if back.as_deref() != Some("0") {
        println!("\n  INCONCLUSIVE: could not return to the first sheet, so the recording");
        println!("  would not have to switch at all.");
        println!("\n  DOCUMENT ID for cleanup: {rec_id}");
        return ExitCode::FAILURE;
    }

    // ---- 1. record: click the Sheet2 tab, then edit a cell ------------------
    println!("\n================ 1. RECORD (tab click + cell edit) ================\n");
    let session = match CaptureSession::start_session(
        "sheets-tab-click",
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

    let switched = click_sheet_tab(&desktop, "Sheet2").await;
    println!("  clicked the Sheet2 tab, gid now {switched:?}");
    // Focus lands on the tab button after clicking it, so typing would go
    // there rather than into a cell. The previous run typed into the button and
    // captured nothing, which was then misread as "the tab click is not
    // captured".
    focus_grid(&desktop).await;
    if let Ok(el) = desktop.focused_element() {
        let _ = el.type_text(MARKER, false);
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{Tab}");
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    // `unmapped` is the load-bearing diagnostic. Zero actions AND zero unmapped
    // events means the recorder never saw anything, which is a broken run, not
    // a finding about what capture admits. Without this the two are
    // indistinguishable -- the exact mistake the first run made.
    println!(
        "\n  captured {} action(s), {} exclusion(s), {} unmapped event(s):",
        report.actions.len(),
        report.exclusions.len(),
        report.unmapped_events
    );
    if report.actions.is_empty() && report.unmapped_events == 0 {
        println!("  !! the recorder observed NO events at all -- this run is broken,");
        println!("     not evidence about whether tab clicks are captured.");
    }
    for a in &report.actions {
        println!(
            "    {:<9} role={:<12} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
    }
    for e in report.exclusions.iter().take(10) {
        println!("    excluded {:?} {:?}", e.kind.as_str(), e.reason);
    }

    let tab_click = report.actions.iter().find(|a| {
        a.kind.as_str() == "click" && a.element_name.as_deref().map(|n| n.trim()) == Some("Sheet2")
    });
    println!(
        "\n  a click action naming the Sheet2 tab: {}",
        if tab_click.is_some() { "YES" } else { "NO" }
    );

    // What selector would compile actually build for it? That is what replay
    // resolves, so printing the action alone is not enough.
    let replayable: Vec<_> = report
        .actions
        .iter()
        .filter(|a| a.kind.as_str() == "click" || a.kind.as_str() == "type")
        .cloned()
        .collect();
    let recorded_pb = compile(
        &replayable,
        "Sheets Tab Click",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    println!("\n  compiled steps and the selectors replay would resolve:");
    for s in &recorded_pb.steps {
        let sel = serde_json::from_str::<serde_json::Value>(&s.action_payload_json)
            .ok()
            .and_then(|v| v["target"]["selector"].as_str().map(str::to_string))
            .unwrap_or_else(|| "<none>".into());
        println!("    [{}] {:<9} selector={sel:?}", s.step_order, s.action_type);
    }

    if tab_click.is_none() {
        println!("\n  STOPPING: the tab click was not captured, so there is nothing to");
        println!("  replay. Route 1 does not work, and no fix follows from it.");
        println!("\n  DOCUMENT ID for cleanup: {rec_id}");
        return ExitCode::SUCCESS;
    }

    // ---- 3. the limitation, measured on the same document -------------------
    //
    // Deliberately BEFORE the replay leg: it is cheap, it uses the document
    // already open on Sheet2, and it is the result most likely to be assumed
    // rather than checked.
    println!("\n================ 3. LIMITATION: recording that STARTS on Sheet2 ================\n");
    println!("  already on Sheet2; starting capture without touching a tab.");
    let session2 = match CaptureSession::start_session(
        "sheets-tab-click-nostart",
        ExclusionList::from_patterns(["!never-matches!"]),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not start second capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    tokio::time::sleep(Duration::from_secs(2)).await;
    focus_grid(&desktop).await;
    if let Ok(el) = desktop.focused_element() {
        let _ = el.type_text(MARKER2, false);
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{Tab}");
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let report2 = match session2.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop second capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "  captured {} action(s), {} unmapped event(s):",
        report2.actions.len(),
        report2.unmapped_events
    );
    for a in &report2.actions {
        println!(
            "    {:<9} role={:<12} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
    }
    let any_sheet_ref = report2.actions.iter().any(|a| {
        a.element_name
            .as_deref()
            .map(|n| n.trim().starts_with("Sheet") && n.trim().len() > 5)
            .unwrap_or(false)
    });
    println!(
        "\n  anything naming a sheet in this recording: {}",
        if any_sheet_ref { "YES" } else { "NO" }
    );

    // ---- 2. replay into a FRESH multi-sheet document ------------------------
    println!("\n================ 2. REPLAY into a fresh document ================\n");

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
    if let Err(e) = store::store(&mut conn, &recorded_pb) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }
    println!("  stored {} step(s)", recorded_pb.steps.len());

    println!("\n-- opening a FRESH document to replay into --");
    open_new_sheet().await;
    let Some((_play_win, play_id)) = sheets_window(&desktop).await else {
        println!("  could not open a fresh document.");
        return ExitCode::FAILURE;
    };
    if play_id == rec_id {
        println!("  the fresh document IS the recorded one; aborting -- replaying into it");
        println!("  would pass without doing anything.");
        return ExitCode::FAILURE;
    }
    println!("  replay document: {play_id}");

    // It needs a Sheet2 to switch to, and must be sitting on Sheet1 so the
    // switch is actually required.
    if !ensure_second_sheet(&desktop).await {
        println!("  could not add a Sheet2 to the replay document.");
        println!("\n  DOCUMENT IDs for cleanup: {rec_id} {play_id}");
        return ExitCode::FAILURE;
    }
    let play_gid2 = current_gid(&desktop).await.unwrap_or_default();
    let play_back = goto_sheet_via_namebox(&desktop, "Sheet1!A1").await;
    println!("  replay doc has a second sheet gid={play_gid2}, now showing gid={play_back:?}");
    if play_back.as_deref() != Some("0") {
        println!("  could not put the replay document on Sheet1, so replay would not have");
        println!("  to switch. Not reporting a result from it.");
        println!("\n  DOCUMENT IDs for cleanup: {rec_id} {play_id}");
        return ExitCode::FAILURE;
    }
    println!("  (so replay MUST switch sheets for the edit to land correctly)");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let run = match paradigm_lib::replay::replay(&mut conn, &desktop, &recorded_pb.id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay failed to run: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("\n  replay status: {}", run.status);
    for o in &run.outcomes {
        println!("    [{}] {:<9} {}", o.step_order, o.action_type, o.result.label());
        for line in o.detail.lines() {
            println!("         {line}");
        }
    }

    // ---- ground truth --------------------------------------------------------
    println!("\n  waiting 10s for Sheets to sync before exporting...");
    tokio::time::sleep(Duration::from_secs(10)).await;
    println!("\n================ GROUND TRUTH (replay doc, per sheet) ================\n");
    let csv1 = download_csv(browser, &play_id, "0").await;
    match &csv1 {
        Some(b) => {
            println!("  Sheet1 (gid=0), {} bytes:", b.len());
            for line in b.lines().take(6) {
                println!("    {line:?}");
            }
        }
        None => println!("  Sheet1 export did not download"),
    }
    let csv2 = if play_gid2.is_empty() {
        None
    } else {
        let c = download_csv(browser, &play_id, &play_gid2).await;
        match &c {
            Some(b) => {
                println!("  Sheet2 (gid={play_gid2}), {} bytes:", b.len());
                for line in b.lines().take(6) {
                    println!("    {line:?}");
                }
            }
            None => println!("  Sheet2 export did not download"),
        }
        c
    };

    let on1 = csv1.as_deref().map(|b| b.contains(MARKER)).unwrap_or(false);
    let on2 = csv2.as_deref().map(|b| b.contains(MARKER)).unwrap_or(false);

    println!("\n================ VERDICT ================\n");
    println!("  1. tab click captured                : {}", tab_click.is_some());
    println!("  2. replay put the edit on Sheet2     : {on2}  (must be true)");
    println!("     replay put the edit on Sheet1     : {on1}  (must be false)");
    println!("  3. a recording that STARTS on Sheet2");
    println!("     carries any sheet identity        : {any_sheet_ref}  (expected false)");
    println!();
    if tab_click.is_some() && on2 && !on1 {
        println!("  ROUTE 1 WORKS for the explicit-switch case, with NO new code --");
        println!("  the tab click is an ordinary click action and replays as one.");
        println!("  It does NOT cover a recording that starts on a non-default sheet.");
    } else if tab_click.is_some() {
        println!("  The click is captured but the replay did not land on the right sheet.");
        println!("  Route 1 does not work as it stands; see the step outcomes above.");
    }

    println!("\n  DOCUMENT IDs for cleanup: {rec_id} {play_id}");
    println!("  clean up with: cargo run --example text_capture_probe -- sheetstrash {rec_id} {play_id}");
    ExitCode::SUCCESS
}

// ---------------------------------------------------- sheetsroundtrip mode ----
// Record real Sheets cell edits, then replay them into a FRESH document and
// check the result against that document's CSV export.
//
// A fresh document matters: replaying into the recorded one would pass even if
// replay did nothing at all, because the values are already there.
async fn sheetsroundtrip_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    println!("== record Sheets cell edits, replay them into a fresh document ==\n");

    let browser = browser_order()[0];
    let open_new_sheet = || async {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
            .spawn()
        {
            let _ = c.wait();
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    };

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    async fn current_sheet(desktop: &Desktop) -> Option<(UIElement, String)> {
        let w = desktop
            .locator("role:Window|name:Untitled spreadsheet")
            .within(desktop.root())
            .all(Some(Duration::from_secs(8)), Some(3))
            .await
            .ok()?
            .into_iter()
            .next()?;
        let mut id = String::new();
        if let Ok(bars) = desktop
            .locator("role:Edit|name:Address and search bar")
            .within(w.clone())
            .all(Some(Duration::from_secs(5)), None)
            .await
        {
            for b in &bars {
                let t = b.text(0).unwrap_or_default();
                if let Some(rest) = t.split("/d/").nth(1) {
                    id = rest.split('/').next().unwrap_or("").to_string();
                }
            }
        }
        Some((w, id))
    }

    // ---- record -------------------------------------------------------------
    println!("-- opening the RECORDING document --");
    open_new_sheet().await;
    let Some((_, rec_id)) = current_sheet(&desktop).await else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    println!("  recording document: {rec_id}");

    let session = match CaptureSession::start_session(
        "sheets-roundtrip",
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

    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{home}");
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
    let values = ["alpha", "bravo", "charlie"];
    for v in values {
        if let Ok(el) = desktop.focused_element() {
            let _ = el.type_text(v, false);
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        if let Ok(el) = desktop.focused_element() {
            let _ = el.press_key("{Tab}");
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("\n  captured {} action(s):", report.actions.len());
    for a in &report.actions {
        println!(
            "    {:<9} role={:<10} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
    }
    let recorded_cells: Vec<(String, String)> = report
        .actions
        .iter()
        .filter(|a| a.kind.as_str() == "type")
        .filter_map(|a| Some((a.element_name.clone()?, a.payload.clone()?)))
        .collect();
    if recorded_cells.is_empty() {
        println!("\n  nothing to replay -- capture produced no cell edits.");
        return ExitCode::FAILURE;
    }

    // ---- compile + store ----------------------------------------------------
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
    // Only the cell edits: the recorded navigate points at the OLD document.
    let cell_actions: Vec<_> = report
        .actions
        .iter()
        .filter(|a| a.kind.as_str() == "type")
        .cloned()
        .collect();
    let playbook = compile(
        &cell_actions,
        "Sheets Roundtrip",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    if let Err(e) = store::store(&mut conn, &playbook) {
        eprintln!("store failed: {e}");
        return ExitCode::FAILURE;
    }
    println!("  stored {} step(s)", playbook.steps.len());

    // ---- replay into a FRESH document ---------------------------------------
    println!("\n-- opening a FRESH document to replay into --");
    open_new_sheet().await;
    let Some((_, play_id)) = current_sheet(&desktop).await else {
        println!("  could not open a fresh document.");
        return ExitCode::FAILURE;
    };
    if play_id == rec_id {
        println!("  the fresh document is the same as the recorded one; aborting, since");
        println!("  replaying into it would pass without doing anything.");
        return ExitCode::FAILURE;
    }
    println!("  replay document: {play_id}");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let run = match paradigm_lib::replay::replay(&mut conn, &desktop, &playbook.id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay failed to run: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("\n  replay status: {}", run.status);
    for o in &run.outcomes {
        println!("    [{}] {:<9} {}", o.step_order, o.action_type, o.result.label());
        if o.result.is_failure() {
            for line in o.detail.lines() {
                println!("         {line}");
            }
        }
    }

    // ---- ground truth on the REPLAY document --------------------------------
    //
    // The export is rendered server-side, so it shows what Sheets has SAVED, not
    // what is on screen. Sheets autosaves asynchronously, and a first run of this
    // exported immediately after the last commit and came back missing only the
    // final cell -- which looks identical to "replay failed on the last step".
    // Waiting first separates the two.
    println!("\n  waiting 10s for Sheets to sync before exporting...");
    tokio::time::sleep(Duration::from_secs(10)).await;
    println!("\n================ GROUND TRUTH (replay doc CSV) ================\n");
    let before_csv = newest_csv().map(|(p, _)| p);
    let export = format!("https://docs.google.com/spreadsheets/d/{play_id}/export?format=csv");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, &export])
        .spawn()
    {
        let _ = c.wait();
    }
    let mut saved = String::new();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before_csv.as_ref() {
                saved = std::fs::read_to_string(&p).unwrap_or_default();
                break;
            }
        }
    }
    for (i, line) in saved.lines().enumerate().take(8) {
        println!("    row {:<3} {line:?}", i + 1);
    }

    println!("\n================ VERDICT ================\n");
    println!("  {:<8} {:<10} {}", "cell", "recorded", "in the REPLAY document");
    let mut ok = 0usize;
    for (cell, value) in &recorded_cells {
        let at = parse_cell_ref(cell)
            .and_then(|(c, r)| csv_at(&saved, c, r))
            .unwrap_or_else(|| "<none>".into());
        if at == *value {
            ok += 1;
        }
        println!("  {cell:<8} {value:<10} {at}");
    }
    println!("\n  cells reproduced correctly: {ok}/{}", recorded_cells.len());
    if ok == recorded_cells.len() && run.status == "completed" {
        println!("\n  PASS: a recorded Sheets edit replays into a different document and");
        println!("  lands in the right cell with the right value.");
    } else {
        println!("\n  NOT A CLEAN PASS.");
    }
    println!("\n  CLEANUP IDS: {rec_id} {play_id}");
    ExitCode::SUCCESS
}

// ------------------------------------------------------- closewins mode ----
// Close browser windows whose title starts with a given prefix.
//
// Probe runs accumulate windows, and that is not cosmetic: repeated runs leave
// several copies of the same page open, which makes a selector like
// `role:edit|name:FieldA` genuinely ambiguous and causes `replaycheck` to be
// refused by the ambiguity check -- correctly, but for an environmental reason
// rather than a real one.
//
// Prefix match, not containment, so a prefix cannot accidentally sweep up an
// unrelated window whose title merely mentions the same words.
async fn closewins_mode() -> ExitCode {
    let prefix = std::env::args()
        .skip_while(|a| a != "closewins")
        .nth(1)
        .unwrap_or_default();
    if prefix.trim().is_empty() {
        eprintln!("usage: closewins <title-prefix>");
        return ExitCode::FAILURE;
    }
    println!("== closing windows whose title starts with {prefix:?} ==\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let all = desktop
        .locator(format!("role:Window|name:{prefix}").as_str())
        .within(desktop.root())
        .all(Some(Duration::from_secs(10)), Some(3))
        .await
        .unwrap_or_default();

    let mut closed = 0usize;
    for w in &all {
        let name = w.name().unwrap_or_default();
        if !name.starts_with(&prefix) {
            println!("  leaving {name:?} (prefix does not match)");
            continue;
        }
        match w.close() {
            Ok(()) => {
                println!("  closed {name:?}");
                closed += 1;
            }
            Err(e) => println!("  could not close {name:?}: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    println!("\n  {closed} window(s) closed");
    ExitCode::SUCCESS
}

// ------------------------------------------------------- deleteui modes ----
// End-to-end verification of the delete-playbook control through the REAL app.
//
// Pointed at a scratch store via `PARADIGM_DATA_DIR`, never the user's real
// database. The whole point of the feature is removing recordings, so a test
// that practised on real ones would be a poor trade.
//
//   seedplaybooks <dir>   put two known playbooks in a scratch store
//   deleteui              drive the running app: delete one, keep the other
//
// The control playbook is the point. "The row disappeared" is also what a
// delete-everything bug looks like.

// -------------------------------------------------------- multimon mode ----
// Does the secondary-monitor click refusal still reproduce on 0.23.35, and does
// the workaround replay actually ships still carry the click?
//
// The doc records this from 2026-08-03 and 2026-08-04. Re-measured rather than
// assumed: the library version is pinned, but "the bug is still there" and "our
// fallback still covers it" are separate claims and both decide what to do next.
//
// Nothing here clicks arbitrary UI. It opens the probe's own page positioned on
// the secondary monitor and clicks a field in it, then proves the click landed
// by checking what has focus afterwards.
// -------------------------------------------------- clipboardcheck mode ----
// What does capture ACTUALLY do when the user pastes?
//
// The known-issues doc says clipboard operations "produce no captured action at
// all", from a session recorded before capture stopped trusting the recorder's
// TextInputCompleted and started reading field values directly. That change may
// have altered the answer for ordinary text fields without anyone re-checking,
// so this measures it instead of designing around the old finding.
//
// Copy from FieldA, paste into FieldB, and see what the pipeline emits.
// Paste into a Google Sheets CELL -- the context Finding 1 was actually
// observed in. A cell paste does not open the editor overlay, so the grid
// watcher's per-keystroke sampling has nothing to sample.
async fn sheetspaste_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};

    println!("== what capture does with a paste into a Sheets cell ==\n");

    const SECRET: &str = "pastedvalue7";
    // Clipboard set from outside Sheets, so nothing about Sheets' own copy path
    // is involved -- this is the user's real shape: copy elsewhere, paste here.
    let set = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("Set-Clipboard -Value '{SECRET}'")])
        .status();
    println!("  clipboard set: {:?}", set.map(|s| s.success()));

    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", "https://sheets.new"])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("  waiting 30s for Sheets...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(window) = desktop
        .locator("role:Window|name:Untitled spreadsheet")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    else {
        println!("  INCONCLUSIVE: no Sheets window.");
        return ExitCode::FAILURE;
    };
    let mut doc_id = String::new();
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if let Some(rest) = t.split("/d/").nth(1) {
                doc_id = rest.split('/').next().unwrap_or("").to_string();
            }
        }
    }
    println!("  DOCUMENT ID: {doc_id}");

    let session = match CaptureSession::start_session(
        "sheets-paste",
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

    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}{home}");
    }
    tokio::time::sleep(Duration::from_millis(900)).await;
    println!("-- pasting into the current cell --");
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{ctrl}v");
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    // Move off the cell so anything pending would flush.
    if let Ok(el) = desktop.focused_element() {
        let _ = el.press_key("{Tab}");
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n  {} action(s), {} exclusion(s), {} unmapped, {} paste(s) observed",
        report.actions.len(), report.exclusions.len(), report.unmapped_events,
        report.pastes_observed);
    for a in &report.actions {
        println!(
            "    {:<9} role={:<9} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
    }

    // Ground truth: did the paste reach the document at all?
    println!("\n  waiting 10s for Sheets to sync, then exporting...");
    tokio::time::sleep(Duration::from_secs(10)).await;
    let before_csv = newest_csv().map(|(p, _)| p);
    let export = format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv");
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, &export])
        .spawn()
    {
        let _ = c.wait();
    }
    let mut saved = String::new();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some((p, _)) = newest_csv() {
            if Some(&p) != before_csv.as_ref() {
                saved = std::fs::read_to_string(&p).unwrap_or_default();
                break;
            }
        }
    }
    let landed = saved.contains(SECRET);
    println!("  saved document contains the pasted value: {landed}");
    for line in saved.lines().take(3) {
        println!("    {line:?}");
    }

    let captured = report
        .actions
        .iter()
        .any(|a| a.payload.as_deref().map(|p| p.contains(SECRET)).unwrap_or(false));

    println!("\n================ VERDICT ================\n");
    println!("  paste reached the spreadsheet   : {landed}");
    println!("  capture recorded its content    : {captured}");
    if !landed {
        println!("\n  INCONCLUSIVE: the paste never happened, so this says nothing.");
    } else if captured {
        println!("\n  Sheets cell pastes ARE captured.");
    } else {
        println!("\n  CONFIRMED GAP: the value reached the spreadsheet and capture holds");
        println!("  no record of it. This is Finding 1, still real, in the context it was");
        println!("  originally observed in.");
    }
    println!("\n  DOCUMENT ID for cleanup: {doc_id}");
    ExitCode::SUCCESS
}

async fn clipboardcheck_mode() -> ExitCode {
    use paradigm_lib::capture::{CaptureSession, ExclusionList};

    println!("== what capture does with a real copy/paste ==\n");
    println!("WARNING: performs real clicks, typing and clipboard use. Hands off.\n");

    let page = std::env::temp_dir().join("paradigm-text-capture-probe.html");
    if std::fs::write(&page, PAGE).is_err() {
        eprintln!("could not write probe page");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    let browser = browser_order()[0];
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args(["/C", "start", "", browser, "--new-window", &url])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("waiting 12s for the browser...");
    tokio::time::sleep(Duration::from_secs(12)).await;

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let find = |name: &str| {
        let d = &desktop;
        let sel = format!("role:Edit|name:{name}");
        async move { d.locator(sel.as_str()).first(Some(Duration::from_secs(10))).await.ok() }
    };
    let (Some(field_a), Some(field_b)) = (find("FieldA").await, find("FieldB").await) else {
        println!("  INCONCLUSIVE: probe fields not found");
        return ExitCode::FAILURE;
    };

    let session = match CaptureSession::start_session(
        "clipboard-check",
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

    const SECRET: &str = "clipsource42";
    println!("-- typing {SECRET:?} into FieldA --");
    robust_click(&desktop, &field_a);
    tokio::time::sleep(Duration::from_millis(600)).await;
    for ch in SECRET.chars() {
        let _ = field_a.type_text(&ch.to_string(), false);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;

    println!("-- Ctrl+A, Ctrl+C on FieldA --");
    let _ = field_a.press_key("{ctrl}a");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let _ = field_a.press_key("{ctrl}c");
    tokio::time::sleep(Duration::from_millis(600)).await;

    println!("-- clicking FieldB and pasting --");
    robust_click(&desktop, &field_b);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let _ = field_b.press_key("{ctrl}v");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Click away so the watcher flushes FieldB.
    if let Ok(done) = desktop
        .locator("role:Button|name:Done")
        .first(Some(Duration::from_secs(8)))
        .await
    {
        robust_click(&desktop, &done);
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let pasted = field_b.text(0).unwrap_or_default();
    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop capture: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n================ RESULTS ================\n");
    println!("  FieldB actually contains: {pasted:?}");
    println!("  paste really happened   : {}", pasted.contains(SECRET));
    println!(
        "\n  {} action(s), {} exclusion(s), {} unmapped, {} paste(s) observed",
        report.actions.len(),
        report.exclusions.len(),
        report.unmapped_events,
        report.pastes_observed
    );
    for a in &report.actions {
        println!(
            "    {:<9} role={:<9} name={:?} payload={:?}",
            a.kind.as_str(),
            a.element_role.as_deref().unwrap_or("-"),
            a.element_name.as_deref().unwrap_or("-"),
            a.payload.as_deref().unwrap_or("-")
        );
    }

    let captured_for_b = report.actions.iter().any(|a| {
        a.kind.as_str() == "type"
            && a.element_name.as_deref() == Some("FieldB")
            && a.payload.as_deref().map(|p| p.contains(SECRET)).unwrap_or(false)
    });

    println!("\n================ VERDICT ================\n");
    println!("  a `type` action carries the pasted text for FieldB: {captured_for_b}");
    if !pasted.contains(SECRET) {
        println!("\n  INCONCLUSIVE: the paste itself did not land, so this says nothing");
        println!("  about what capture does with one.");
    } else if captured_for_b {
        println!("\n  Pasting into an ORDINARY FIELD is already captured -- not as a");
        println!("  clipboard action, but the destination's new value is read directly, so");
        println!("  the data movement is recorded and replay can reproduce it by typing.");
    } else {
        println!("\n  The paste landed and capture recorded nothing for it. The doc's");
        println!("  Finding 1 still holds for this case.");
    }
    ExitCode::SUCCESS
}

async fn multimon_mode() -> ExitCode {
    println!("== secondary-monitor click refusal, re-measured ==\n");

    println!(
        "  per-monitor DPI aware BEFORE ensure_dpi_aware(): {}",
        paradigm_lib::replay::is_per_monitor_dpi_aware()
    );
    paradigm_lib::replay::ensure_dpi_aware();
    let dpi_ok = paradigm_lib::replay::is_per_monitor_dpi_aware();
    println!("  per-monitor DPI aware AFTER:  {dpi_ok}");
    if !dpi_ok {
        println!("  !! the coordinate fallback needs this; results below are not trustworthy");
    }

    let page = std::env::temp_dir().join("paradigm-text-capture-probe.html");
    if std::fs::write(&page, PAGE).is_err() {
        eprintln!("could not write probe page");
        return ExitCode::FAILURE;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    let browser = browser_order()[0];
    // Positioned on the secondary display. x=2000 is inside DISPLAY4, which
    // starts at 1920 -- past the primary's width, which is the whole condition
    // the defect turns on.
    if let Ok(mut c) = std::process::Command::new("cmd")
        .args([
            "/C",
            "start",
            "",
            browser,
            "--new-window",
            "--window-position=2000,120",
            "--window-size=900,600",
            &url,
        ])
        .spawn()
    {
        let _ = c.wait();
    }
    println!("\n  opened the probe page at x=2000 (secondary display); waiting 12s...");
    tokio::time::sleep(Duration::from_secs(12)).await;

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
            println!("  INCONCLUSIVE: FieldA not found: {e}");
            return ExitCode::FAILURE;
        }
    };

    let bounds = field.bounds().ok();
    println!("\n  FieldA bounds: {bounds:?}");
    let on_secondary = bounds.map(|(x, _, _, _)| x >= 1920.0).unwrap_or(false);
    println!("  on the secondary display (x >= 1920): {on_secondary}");
    if !on_secondary {
        println!("\n  INCONCLUSIVE: the window did not land on the secondary display, so this");
        println!("  run does not exercise the defect at all.");
        return ExitCode::FAILURE;
    }

    // The two halves of the reported defect.
    let visible = field.is_visible();
    println!("\n  is_visible() : {visible:?}   (UIA says on-screen; the check disagrees)");
    let clicked = field.click();
    let refused = matches!(
        clicked,
        Err(terminator::AutomationError::ElementNotVisible(_))
    );
    println!("  click()      : {}", match &clicked {
        Ok(_) => "Ok -- NOT refused".to_string(),
        Err(e) => format!("Err {e}"),
    });

    // The workaround the product actually ships, exercised the same way
    // replay::click does it.
    let mut fallback_ok = false;
    if let Some((x, y, w, h)) = bounds {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        println!("\n  falling back to a real click at ({cx}, {cy})...");
        match desktop.click_at_coordinates(cx, cy) {
            Ok(_) => {
                tokio::time::sleep(Duration::from_millis(900)).await;
                // Proof the click LANDED, not merely that the call returned Ok.
                let focused = desktop
                    .focused_element()
                    .ok()
                    .and_then(|el| el.name())
                    .unwrap_or_default();
                println!("  focused element afterwards: {focused:?}");
                fallback_ok = focused == "FieldA";
            }
            Err(e) => println!("  coordinate click failed: {e}"),
        }
    }

    println!("\n================ VERDICT ================\n");
    println!("  defect still reproduces (click refused on secondary): {refused}");
    println!("  is_visible() wrongly false                          : {:?}", visible.as_ref().map(|v| !v));
    println!("  shipped workaround lands the click                  : {fallback_ok}");
    println!("  process per-monitor DPI aware                       : {dpi_ok}");
    if refused && fallback_ok {
        println!("\n  Both halves confirmed: the library still refuses, and the coordinate");
        println!("  fallback still covers it. Replay is mitigated, not fixed.");
    } else if !refused {
        println!("\n  The refusal did NOT reproduce. Either the library changed or the");
        println!("  element was not where this run assumed.");
    } else {
        println!("\n  The refusal reproduces and the workaround did NOT land the click.");
    }
    ExitCode::SUCCESS
}

async fn seedplaybooks_mode() -> ExitCode {
    use paradigm_lib::capture::stream::{ActionCandidate, CapturedStream};
    use paradigm_lib::capture::{ActionKind, ExclusionList};
    use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
    use paradigm_lib::labeling::RedactionPolicy;

    let dir = match std::env::args().skip_while(|a| a != "seedplaybooks").nth(1) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            eprintln!("usage: seedplaybooks <data-dir>");
            return ExitCode::FAILURE;
        }
    };
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("could not create {}", dir.display());
        return ExitCode::FAILURE;
    }

    let (db_path, key_path) = paradigm_lib::db::paths_in(&dir);
    let mut conn = match paradigm_lib::db::open(&db_path, &key_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not open scratch db: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
    stream.admit(ActionCandidate {
        kind: ActionKind::Click,
        identifiers: vec!["probe.exe".into()],
        process_name: Some("probe.exe".into()),
        element_role: Some("Button".into()),
        element_name: Some("Go".into()),
        payload: None,
        detail: None,
        timestamp_ms: 0,
    });
    let actions = stream.actions().to_vec();

    for name in ["DeleteMe Probe", "KeepMe Probe"] {
        let pb = compile(
            &actions,
            name,
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        if let Err(e) = store::store(&mut conn, &pb) {
            eprintln!("store failed: {e}");
            return ExitCode::FAILURE;
        }
        println!("  seeded {name:?} id={}", pb.id);
    }
    match store::list(&conn) {
        Ok(rows) => println!("  scratch store now holds {} playbook(s)", rows.len()),
        Err(e) => println!("  could not list: {e}"),
    }
    ExitCode::SUCCESS
}

/// Names of playbooks that currently have a Delete control on screen.
async fn delete_controls(desktop: &Desktop, window: &UIElement) -> Vec<(String, UIElement)> {
    // Retried, and errors are REPORTED rather than swallowed. The first version
    // used `if let Ok(all)`, and the first enumeration against a freshly
    // activated webview came back Err -- so it returned an empty list that was
    // indistinguishable from "the UI rendered nothing". It had rendered fine.
    for attempt in 1..=3 {
        match desktop
            .locator("role:Button")
            .within(window.clone())
            .all(Some(Duration::from_secs(6)), None)
            .await
        {
            Ok(all) => {
                let found: Vec<(String, UIElement)> = all
                    .iter()
                    .filter_map(|b| {
                        let n = b.name().unwrap_or_default();
                        n.strip_prefix("Delete ")
                            .filter(|rest| *rest != "permanently")
                            .map(|rest| (rest.to_string(), b.clone()))
                    })
                    .collect();
                if !found.is_empty() || attempt == 3 {
                    return found;
                }
                println!("    (attempt {attempt}: {} buttons, none a delete control)", all.len());
            }
            Err(e) => println!("    (attempt {attempt}: enumerating buttons failed: {e})"),
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
    Vec::new()
}

async fn deleteui_mode() -> ExitCode {
    println!("== delete a playbook through the real app UI ==\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("accessibility engine unavailable: {e}");
            return ExitCode::FAILURE;
        }
    };

    let window = match desktop
        .locator("role:Window|name:paradigm")
        .within(desktop.root())
        .all(Some(Duration::from_secs(10)), Some(3))
        .await
        .ok()
        .and_then(|all| all.into_iter().next())
    {
        Some(w) => w,
        None => {
            println!("  INCONCLUSIVE: the app window was not found. Is the dev app running,");
            println!("  with PARADIGM_DATA_DIR pointed at the scratch store?");
            return ExitCode::FAILURE;
        }
    };
    println!("  app window: {:?}", window.name().unwrap_or_default());
    let _ = window.activate_window();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let before = delete_controls(&desktop, &window).await;
    println!(
        "  playbooks listed: {:?}",
        before.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()
    );
    let Some((_, delete_btn)) = before.iter().find(|(n, _)| n.starts_with("DeleteMe")) else {
        // Distinguish "the UI did not render the list" from "the search did not
        // see it" -- opposite problems, and an empty result looks identical.
        println!("\n  no delete control found; dumping what the window DOES expose:");
        for sel in ["role:Text", "role:Button", "role:Document"] {
            if let Ok(all) = desktop
                .locator(sel)
                .within(window.clone())
                .all(Some(Duration::from_secs(5)), None)
                .await
            {
                println!("    {sel} -> {} element(s)", all.len());
                for el in all.iter().take(12) {
                    let n = el.name().unwrap_or_default();
                    let t = el.text(0).unwrap_or_default();
                    if !n.trim().is_empty() || !t.trim().is_empty() {
                        println!("      name={n:?} text={:?}", t.chars().take(80).collect::<String>());
                    }
                }
            } else {
                println!("    {sel} -> Err");
            }
        }
        println!("\n  INCONCLUSIVE: no 'DeleteMe Probe' row on screen.");
        return ExitCode::FAILURE;
    };
    if !before.iter().any(|(n, _)| n.starts_with("KeepMe")) {
        println!("\n  INCONCLUSIVE: the control playbook is missing, so this run could not");
        println!("  tell a correct delete from one that removed everything.");
        return ExitCode::FAILURE;
    }

    println!("\n-- clicking Delete --");
    robust_click(&desktop, delete_btn);
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The confirmation must NAME the playbook, and this has to be checked
    // against the DIALOG rather than the page: the list row also contains the
    // name, so a whole-window search reports success even if no dialog opened.
    // "This cannot be undone" appears only inside the dialog, so it is what
    // proves the dialog is what was read.
    let mut named = false;
    let mut dialog_seen = false;
    if let Ok(texts) = desktop
        .locator("role:Text")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for t in &texts {
            let n = t.name().unwrap_or_default();
            if n.contains("This cannot be undone") {
                dialog_seen = true;
            }
            if n.contains("DeleteMe Probe") && n.contains("Delete") {
                println!("  confirmation names it: {n:?}");
                named = true;
            }
        }
    }
    if !dialog_seen {
        println!("  !! the confirmation dialog was not detected on screen");
    }
    named = named && dialog_seen;
    if !named {
        println!("  !! the confirmation did not name the playbook");
    }

    // The dialog's confirm button. Matched by EXACT name against two spellings:
    // the Radix AlertDialog on this branch labels it "Delete", while the
    // short-lived PlaybookList on backend-dev said "Delete permanently". Exact
    // match matters -- every row's own Delete button is also a Button whose name
    // begins with "Delete", and containment would pick one of those instead,
    // clicking a row rather than confirming the dialog.
    let confirm = desktop
        .locator("role:Button")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
        .ok()
        .and_then(|all| {
            all.into_iter().find(|b| {
                let n = b.name().unwrap_or_default();
                n == "Delete" || n == "Delete permanently"
            })
        });
    let Some(confirm) = confirm else {
        println!("\n  INCONCLUSIVE: no confirm button ('Delete' / 'Delete permanently') appeared.");
        return ExitCode::FAILURE;
    };
    println!("-- confirming --");
    // Prefer the element's own invoke over a coordinate click: a webview button
    // is a DOM node, and hit-testing it by screen position is the part most
    // likely to miss. Fall back only if invoke errors.
    match confirm.click() {
        Ok(_) => println!("  clicked via UIA invoke"),
        Err(e) => {
            println!("  invoke failed ({e}); falling back to a coordinate click");
            robust_click(&desktop, &confirm);
        }
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Whatever the app is saying now -- an error here is the difference between
    // "the click missed" and "the delete was refused".
    if let Ok(texts) = desktop
        .locator("role:Text")
        .within(window.clone())
        .all(Some(Duration::from_secs(5)), None)
        .await
    {
        for t in &texts {
            let n = t.name().unwrap_or_default();
            if n.contains("Could not delete") || n.contains("error") {
                println!("  app reports: {n:?}");
            }
        }
    }

    let after = delete_controls(&desktop, &window).await;
    let names: Vec<String> = after.iter().map(|(n, _)| n.clone()).collect();
    println!("\n  playbooks after delete: {names:?}");

    let gone = !names.iter().any(|n| n.starts_with("DeleteMe"));
    let kept = names.iter().any(|n| n.starts_with("KeepMe"));

    println!("\n================ VERDICT ================\n");
    println!("  confirmation named the playbook : {named}");
    println!("  deleted row is gone from the UI : {gone}");
    println!("  the other playbook survived     : {kept}");
    if named && gone && kept {
        println!("\n  PASS: deleting through the real UI removed exactly the chosen");
        println!("  playbook, and the list re-read the store to prove it.");
    } else {
        println!("\n  NOT A CLEAN PASS -- see above.");
    }
    ExitCode::SUCCESS
}

#[tokio::main]
async fn main() -> ExitCode {
    paradigm_lib::replay::ensure_dpi_aware();
    init_tracing();

    if std::env::args().any(|a| a == "sheetspaste") {
        return sheetspaste_mode().await;
    }
    if std::env::args().any(|a| a == "clipboardcheck") {
        return clipboardcheck_mode().await;
    }
    if std::env::args().any(|a| a == "multimon") {
        return multimon_mode().await;
    }
    if std::env::args().any(|a| a == "seedplaybooks") {
        return seedplaybooks_mode().await;
    }
    // Ground truth for the UI test: what the scratch store actually holds.
    if std::env::args().any(|a| a == "listplaybooks") {
        let dir = std::env::args()
            .skip_while(|a| a != "listplaybooks")
            .nth(1)
            .unwrap_or_default();
        let (db, key) = paradigm_lib::db::paths_in(std::path::Path::new(&dir));
        match paradigm_lib::db::open(&db, &key)
            .and_then(|c| paradigm_lib::compile::store::list(&c).map_err(Into::into))
        {
            Ok(rows) => {
                println!("  store holds {} playbook(s):", rows.len());
                for r in &rows {
                    println!("    {:?}", r.name);
                }
            }
            Err(e) => println!("  could not read store: {e}"),
        }
        return ExitCode::SUCCESS;
    }
    if std::env::args().any(|a| a == "deleteui") {
        return deleteui_mode().await;
    }
    if std::env::args().any(|a| a == "closewins") {
        return closewins_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsformula") {
        return sheetsformula_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsread") {
        return sheetsread_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsqualified") {
        return sheetsqualified_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsstamp") {
        return sheetsstamp_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsactive") {
        return sheetsactive_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsrename") {
        return sheetsrename_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsselected") {
        return sheetsselected_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsscopefix") {
        return sheetsscopefix_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsscope") {
        return sheetsscope_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsreplaytab") {
        return sheetsreplaytab_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsmanual") {
        return sheetsmanual_mode().await;
    }
    if std::env::args().any(|a| a == "sheetstabclick") {
        return sheetstabclick_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsmulti") {
        return sheetsmulti_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsroundtrip") {
        return sheetsroundtrip_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsentry") {
        return sheetsentry_mode().await;
    }
    if std::env::args().any(|a| a == "notepadclose") {
        return notepadclose_mode().await;
    }
    if std::env::args().any(|a| a == "notepadgrid") {
        return notepadgrid_mode().await;
    }
    if std::env::args().any(|a| a == "sheetscapture") {
        return sheetscapture_mode().await;
    }
    if std::env::args().any(|a| a == "sheetskeys") {
        return sheetskeys_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsclean") {
        return sheetsclean_mode().await;
    }
    if std::env::args().any(|a| a == "sheetswatch") {
        return sheetswatch_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsedit") {
        return sheetsedit_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsstate") {
        return sheetsstate_mode().await;
    }
    if std::env::args().any(|a| a == "sheetsa11y") {
        return sheetsa11y_mode().await;
    }
    if std::env::args().any(|a| a == "sheetstrash") {
        return sheetstrash_mode().await;
    }
    if std::env::args().any(|a| a == "sheets") {
        return sheets_mode().await;
    }
    if std::env::args().any(|a| a == "resolveorder") {
        return resolveorder_mode().await;
    }
    if std::env::args().any(|a| a == "ambigreplay") {
        return ambigreplay_mode().await;
    }
    if std::env::args().any(|a| a == "decoycount") {
        return decoycount_mode().await;
    }

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

    // In-memory only -- no I/O until the run ends. Writing log lines during a
    // timing race measures the instrument: enabling the recorder's tracing was
    // recorded making this very defect vanish.
    paradigm_lib::capture::text::set_trace(true);
    paradigm_lib::capture::grid::reset_timing();

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

    // The instrumented watcher trace. This is the evidence both prior attempts
    // at the no-settle race lacked: which route started each watch, on which
    // element, and what each flush actually saw.
    // What the grid watcher cost, in a context with no grid in it at all.
    let (grid_calls, grid_micros) = paradigm_lib::capture::grid::timing();
    println!("\n================ GridCellWatcher COST ================\n");
    println!("  observe_key calls   : {grid_calls}");
    println!("  total time          : {:.1} ms", grid_micros as f64 / 1000.0);
    if grid_calls > 0 {
        println!(
            "  mean per keystroke  : {:.3} ms",
            grid_micros as f64 / grid_calls as f64 / 1000.0
        );
    }

    // Exclusions distinguish "the watcher produced nothing" from "it produced a
    // candidate the gate refused" -- opposite diagnoses, and the A-trial trace
    // shows an EMITTING line for a field that has no action in the report.
    println!("\n================ EXCLUSIONS ================\n");
    if report.exclusions.is_empty() {
        println!("  (none)");
    }
    for e in report.exclusions.iter().take(20) {
        println!("  {:?} {:?}", e.kind.as_str(), e.reason);
    }

    let watcher_trace = paradigm_lib::capture::text::take_trace();
    println!("\n================ WATCHER TRACE ================\n");
    if watcher_trace.is_empty() {
        println!("  (empty -- tracing was not enabled, or nothing was observed)");
    }
    for line in &watcher_trace {
        println!("  {line}");
    }

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
