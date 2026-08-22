//! `detect::candidates` against a REAL capture session.
//!
//!     cargo run --example candidates_probe            # 30 second window
//!     cargo run --example candidates_probe -- 45
//!
//! The unit tests pin the rules against constructed fixtures. This runs the
//! same function over actions a real desktop produced, with real
//! `element_bounds`, which is the thing
//! `docs/planning/Filtered-Post-Hoc-Confirmation.md` named as the missing
//! evidence: *"the interesting numbers are a projection"* until a genuine
//! three-record recording exists.
//!
//! Prints the funnel stage by stage so a zero can be attributed to a stage
//! rather than guessed at.
//!
//! Captures only. Performs nothing.

use std::process::ExitCode;

use paradigm_lib::capture::{CaptureSession, ExclusionList};
use paradigm_lib::detect::candidates::candidates;

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
        .unwrap_or(30);

    println!("== detect::candidates over a real session ==\n");
    println!("Work a repeating task: touch the SAME field in three or more records.");
    println!("Recording for {seconds}s, starting now.\n");

    let session = match CaptureSession::start_session("candidates-probe", ExclusionList::placeholder()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not start: {e}");
            return ExitCode::FAILURE;
        }
    };
    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    let report = match session.stop_session().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not stop: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("\n== what was captured ==");
    for (i, a) in report.actions.iter().enumerate() {
        println!(
            "  {:>3} {:<9} {:<28} {}",
            i + 1,
            a.kind.as_str(),
            a.element_name
                .as_deref()
                .unwrap_or("(no name)")
                .chars()
                .take(28)
                .collect::<String>(),
            a.element_bounds
                .map(|(x, y, w, h)| format!("x{x:.0} y{y:.0} w{w:.0} h{h:.0}"))
                .unwrap_or_else(|| "(no bounds)".into())
        );
    }

    let set = candidates(&report.actions);
    let f = set.funnel;
    println!("\n== the funnel ==");
    println!("  stage 0  raw actions                  : {}", f.raw);
    println!(
        "  stage 1  after dropping Navigate      : {}  (-{})",
        f.after_navigate,
        f.raw - f.after_navigate
    );
    println!(
        "  stage 2  with a cell ref or a position: {}  (-{})",
        f.with_identity,
        f.after_navigate - f.with_identity
    );
    println!("  stage 3  distinct field groups        : {}", f.field_groups);
    println!("  stage 4  surviving the Rule of 3      : {}", f.surviving);

    println!("\n== what the user would be asked to confirm ==");
    if set.groups.is_empty() {
        println!("  (nothing)");
        println!("\n  Zero is a real answer, not a failure. Read the funnel above: a");
        println!("  drop at stage 2 means the actions carried no identity to group by,");
        println!("  and a drop at stage 4 means no field was touched in three records.");
    }
    for c in &set.groups {
        println!(
            "  [ ] {:<12} {:<58} {} record(s), {} action(s)  steps {:?}",
            c.id,
            c.detail,
            c.distinct_records,
            c.occurrences(),
            c.action_indices.iter().map(|i| i + 1).collect::<Vec<_>>()
        );
    }
    if f.raw > 0 {
        println!(
            "\n  tedium: {} decision(s) from {} raw actions ({:.1}%)",
            set.groups.len(),
            f.raw,
            100.0 * set.groups.len() as f64 / f.raw as f64
        );
    }
    ExitCode::SUCCESS
}
