//! Phase 1 Step 3: prove we can capture the USER's own actions.
//!
//!     cargo run --example capture_probe            # 30 second window
//!     cargo run --example capture_probe -- 45      # 45 second window
//!
//! Unlike terminator_probe, this program performs nothing. It starts a Record
//! Mode session, waits while you act, stops, and prints the raw stream.
//!
//! Capture only: no cleaning, tagging or compiling, and nothing touches the
//! database.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use paradigm_lib::capture::{CaptureSession, ExclusionList};

const DEFAULT_SECONDS: u64 = 30;

/// See docs/known-issues/terminator-multi-monitor-visibility.md -- UIA reports
/// physical pixels while GetSystemMetrics is virtualized for a DPI-unaware
/// process. Any process mixing the two must opt in.
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

    let seconds = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECONDS);

    let exclusions = ExclusionList::placeholder();

    println!("== Record Mode capture probe ==\n");
    println!("This captures YOUR actions. It performs nothing itself.\n");
    println!("Exclusion patterns active (source app matching any of these is");
    println!("refused BEFORE entering the stream):");
    println!("  {:?}\n", exclusions.patterns());

    println!("WHAT TO DO once recording starts:");
    println!("  1. Switch to another application (Notepad, a browser, anything).");
    println!("  2. Click a few things -- buttons, tabs, menu items.");
    println!("  3. Type some text into a text field.");
    println!("  4. Optionally switch to a second application.");
    println!("  Do NOT type anything real or sensitive -- this prints what it captures.\n");
    println!("You have {seconds} seconds. Recording starts NOW.\n");

    let session = match CaptureSession::start_session("capture-probe", exclusions).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: could not start the capture session.");
            eprintln!("  expected: low-level input hooks to install");
            eprintln!("  found:    {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("[ok] recording -- go and do things now\n");

    // Live progress, so the person can see it is actually observing them.
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
        println!(
            "  [{remaining:>3}s left] captured {} action(s), {} excluded",
            session.admitted_so_far(),
            session.excluded_so_far()
        );
    }

    println!("\n[..] stopping session");
    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: could not stop the capture session cleanly: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n== captured raw stream: {} ==", report.session_name);
    println!("actions captured : {}", report.actions.len());
    println!("actions excluded : {}", report.exclusions.len());
    println!("events unmapped  : {} (raw mouse/keyboard/etc, not modelled in Step 3)\n", report.unmapped_events);

    if report.actions.is_empty() {
        println!("(nothing captured)");
    }

    let first_ts = report.actions.first().map(|a| a.timestamp_ms).unwrap_or(0);
    for (i, a) in report.actions.iter().enumerate() {
        let offset = a.timestamp_ms.saturating_sub(first_ts);
        println!("[{:>3}] +{:>6}ms  {:<8} app={:?}", i + 1, offset, a.kind.as_str(), a.source_app);
        println!(
            "                     element: role={:?} name={:?}",
            a.element_role.as_deref().unwrap_or("<none>"),
            a.element_name.as_deref().unwrap_or("<none>")
        );
        if let Some(p) = &a.payload {
            println!("                     payload: {p:?}");
        }
        if let Some(d) = &a.detail {
            println!("                     detail : {d}");
        }
    }

    if !report.exclusions.is_empty() {
        println!("\n-- refused by the exclusion gate (never entered the stream) --");
        for e in &report.exclusions {
            println!("  {:<8} {}", e.kind.as_str(), e.reason.describe());
        }
    }

    println!("\n== result ==");
    if report.actions.is_empty() {
        println!("INCOMPLETE: the session ran but observed no actions.");
        println!("  Either nothing was done during the window, or hooks are not receiving input.");
        ExitCode::from(2)
    } else {
        println!("PASS: captured {} real user action(s).", report.actions.len());
        ExitCode::SUCCESS
    }
}
