//! Does the FOCUSED element follow what the user selects and copies on a page?
//!
//!     cargo run --example focus_vs_selection_probe -- [seconds]
//!
//! `capture::grid::read_position` takes the source of a copy to be
//! `Desktop::focused_element()`. On a spreadsheet that is right -- the selected
//! cell IS the focused element. On an ordinary web page the user selects text
//! with the mouse, and whether focus follows that selection is the question this
//! answers, because session record-fe88fb0d resolved NINE copies of nine
//! different values to one single source reference, `el/19/`.
//!
//! Prints the focused element every 400ms. Reads only.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use terminator::Desktop;

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
        .unwrap_or(20);

    let Ok(desktop) = Desktop::new_default() else {
        eprintln!("accessibility engine unavailable");
        return ExitCode::FAILURE;
    };

    println!("watching the focused element for {seconds}s\n");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last = String::new();
    let mut samples = 0usize;
    let mut changes = 0usize;

    while Instant::now() < deadline {
        if let Ok(el) = desktop.focused_element() {
            let line = format!(
                "id={:<14} role={:<12} name={:?}",
                el.id().unwrap_or_default(),
                el.role(),
                el.name().unwrap_or_default().chars().take(40).collect::<String>()
            );
            samples += 1;
            if line != last {
                changes += 1;
                println!("  [{:>5}ms] {line}", deadline.saturating_duration_since(Instant::now()).as_millis());
                last = line;
            }
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    println!("\n{samples} samples, {changes} distinct focused element(s)");
    ExitCode::SUCCESS
}
