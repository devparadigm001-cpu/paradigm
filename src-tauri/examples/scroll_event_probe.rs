//! Does a scroll reach the event stream at all, and with what units?
//!
//!     cargo run --example scroll_event_probe -- [seconds]
//!
//! Direction 1 for defect 1 (`element-bounds-are-viewport-relative-...`) is dead:
//! UIScrollPattern is a stub on Edge web content. Direction 3 -- scroll-epoch
//! partitioning -- proposed INFERRING the scroll from the captured positions.
//!
//! But `MouseEventType::Wheel` exists in the recorder, carries `scroll_delta`,
//! and paradigm's config leaves every filter that would drop it turned off. So
//! before designing an inference, the cheaper question: is the scroll already
//! sitting in the stream, unread?
//!
//! This subscribes to the RAW recorder rather than going through
//! `CaptureSession`, because paradigm's pump has no arm for `WorkflowEvent::Mouse`
//! and would discard exactly the event under test.
//!
//! Reads only. Performs nothing -- the scrolling has to come from outside.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures::StreamExt;
use terminator_workflow_recorder::{
    MouseEventType, WorkflowEvent, WorkflowRecorder, WorkflowRecorderConfig,
};

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

    // The recorder reports a failed input hook through `error!` and nothing
    // else. Without a subscriber that failure is indistinguishable from "the
    // user did not scroll".
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "terminator_workflow_recorder=debug".into()),
        )
        .init();

    let seconds = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u64>().ok())
        .unwrap_or(20);

    // Deliberately the SAME config as `CaptureSession::start_session`, so that
    // what this sees is what a real recording would have seen. If this probe
    // enabled something paradigm turns off, it would prove nothing about
    // paradigm.
    let config = WorkflowRecorderConfig {
        record_mouse: true,
        record_keyboard: true,
        capture_ui_elements: true,
        record_text_input_completion: true,
        record_application_switches: true,
        record_clipboard: true,
        max_clipboard_content_length: 0,
        record_browser_tab_navigation: false,
        ..Default::default()
    };

    println!("== scroll event probe ==\n");
    println!("filter_mouse_noise = {}   (true would drop Wheel entirely)", config.filter_mouse_noise);
    println!("record_mouse       = {}", config.record_mouse);
    println!("processing delay   = {}ms", config.effective_processing_delay_ms());
    println!("max events/sec     = {:?}\n", config.effective_max_events_per_second());

    let mut recorder = WorkflowRecorder::new("scroll-probe".to_string(), config);
    let mut events = Box::pin(recorder.event_stream());

    if let Err(e) = recorder.start().await {
        eprintln!("recorder failed to start: {e}");
        return ExitCode::FAILURE;
    }
    println!("recording for {seconds}s -- scroll something NOW\n");

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut wheel = 0usize;
    let mut other_mouse = 0usize;
    let mut keys = 0usize;
    let mut total_dy = 0i64;
    let mut total_dx = 0i64;
    let mut first_wheel: Option<Instant> = None;
    let mut last_wheel: Option<Instant> = None;

    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, events.next()).await {
            Err(_) => break,
            Ok(None) => break,
            Ok(Some(event)) => match event {
                WorkflowEvent::Mouse(e) if e.event_type == MouseEventType::Wheel => {
                    wheel += 1;
                    let now = Instant::now();
                    first_wheel.get_or_insert(now);
                    last_wheel = Some(now);
                    let (dx, dy) = e.scroll_delta.unwrap_or((0, 0));
                    total_dx += dx as i64;
                    total_dy += dy as i64;
                    // The element under the cursor is captured on this path with
                    // a 1000ms timeout -- worth seeing whether it resolves.
                    let el = e
                        .metadata
                        .ui_element
                        .as_ref()
                        .map(|u| format!("{} {:?}", u.role(), u.name().unwrap_or_default()))
                        .unwrap_or_else(|| "(none)".into());
                    println!(
                        "  wheel #{wheel:<3} delta=({dx:>5},{dy:>5})  at ({:>5},{:>5})  under: {}",
                        e.position.x, e.position.y, el.chars().take(50).collect::<String>()
                    );
                }
                WorkflowEvent::Mouse(_) => other_mouse += 1,
                WorkflowEvent::Keyboard(e) if e.is_key_down => {
                    keys += 1;
                    println!("  key   down  code={}", e.key_code);
                }
                _ => {}
            },
        }
    }

    let _ = recorder.stop().await;

    println!("\n== totals ==");
    println!("  wheel events     : {wheel}");
    println!("  scroll_delta sum : dx={total_dx}  dy={total_dy}");
    println!("  other mouse      : {other_mouse}");
    println!("  key-downs        : {keys}");
    if let (Some(a), Some(b)) = (first_wheel, last_wheel) {
        let span = b.duration_since(a).as_millis();
        println!("  wheel span       : {span}ms for {wheel} events");
        if wheel > 1 && span > 0 {
            println!("  wheel rate       : {:.1}/s", (wheel as f64 - 1.0) * 1000.0 / span as f64);
        }
    }
    ExitCode::SUCCESS
}
