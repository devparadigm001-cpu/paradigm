//! Record Mode capture: observe the user's own actions during a session.
//!
//! This is the mirror image of `examples/terminator_probe.rs`. There, *we*
//! performed actions. Here the user performs them and we watch.
//!
//! ## Why this uses `terminator-workflow-recorder`
//!
//! `terminator-rs` itself has no observation API -- it is entirely about
//! performing actions. Verified against the 0.23.35 source: zero occurrences of
//! `EventHandler`, `SetWinEventHook`, `SetWindowsHookEx`, or any UI Automation
//! event-subscription type.
//!
//! Its sibling crate `terminator-workflow-recorder` (same project, same
//! version) does exactly what we need, and implements it the way we would have
//! had to ourselves: `rdev::listen` -- low-level Windows input hooks -- with
//! each event correlated against a UI Automation read to identify *what* was
//! acted upon rather than just raw coordinates and keystrokes. Using it avoids
//! maintaining a parallel hook implementation.
//!
//! ## Scope
//!
//! Capture only. No cleaning, tagging, or compiling (Step 4), and nothing is
//! written to `run_steps_log` (Step 5). The stream is held in memory for the
//! life of one session.

pub mod exclusion;
pub mod stream;
pub mod text;

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use terminator_workflow_recorder::{
    WorkflowEvent, WorkflowRecorder, WorkflowRecorderConfig,
};
use tokio::task::JoinHandle;

pub use exclusion::ExclusionList;
pub use stream::{
    ActionCandidate, ActionKind, Admission, CapturedAction, CapturedStream, ExclusionRecord,
};
pub use text::TextFieldWatcher;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("recorder failed: {0}")]
    Recorder(String),

    #[error("a capture session is already running")]
    AlreadyRunning,
}

/// What one session observed.
pub struct CaptureReport {
    pub session_name: String,
    pub actions: Vec<CapturedAction>,
    pub exclusions: Vec<ExclusionRecord>,
    /// Events the recorder emitted that this step does not map to an action
    /// (raw mouse moves, individual keystrokes, clipboard, and so on).
    pub unmapped_events: usize,
}

/// One Record Mode capture session.
pub struct CaptureSession {
    name: String,
    recorder: WorkflowRecorder,
    stream: Arc<Mutex<CapturedStream>>,
    unmapped: Arc<Mutex<usize>>,
    /// Produces the `Type` actions. Shared with the pump so `stop_session` can
    /// flush a field the user was still in when they stopped recording.
    watcher: Arc<Mutex<TextFieldWatcher>>,
    pump: Option<JoinHandle<()>>,
}

impl CaptureSession {
    /// Begin observing. Returns once hooks are installed and events are flowing.
    pub async fn start_session(
        name: impl Into<String>,
        exclusions: ExclusionList,
    ) -> Result<Self, CaptureError> {
        let name = name.into();
        let config = WorkflowRecorderConfig {
            record_mouse: true,
            record_keyboard: true,
            capture_ui_elements: true,
            record_text_input_completion: true,
            record_application_switches: true,
            // Not needed for Phase 1 and each is another source of captured
            // content we would have to gate.
            record_clipboard: false,
            record_browser_tab_navigation: false,
            ..Default::default()
        };

        let mut recorder = WorkflowRecorder::new(name.clone(), config);

        // Subscribe BEFORE start(): event_tx is a broadcast channel, so a
        // subscription created afterwards would miss everything in between.
        let mut events = Box::pin(recorder.event_stream());

        let stream = Arc::new(Mutex::new(CapturedStream::new(exclusions)));
        let unmapped = Arc::new(Mutex::new(0usize));
        let watcher = Arc::new(Mutex::new(TextFieldWatcher::new()));

        let pump_stream = Arc::clone(&stream);
        let pump_unmapped = Arc::clone(&unmapped);
        let pump_watcher = Arc::clone(&watcher);
        let pump = tokio::spawn(async move {
            while let Some(event) = events.next().await {
                // Typed text is synthesised from focus transitions rather than
                // taken from the recorder's own TextInputCompleted -- see
                // `capture::text` for the measurements behind that. Emitted
                // BEFORE this event's own candidate so the typing is ordered
                // ahead of the click that ended it.
                if let Some(typed) = observe_text(&pump_watcher, &event) {
                    if let Ok(mut s) = pump_stream.lock() {
                        s.admit(typed);
                    }
                }

                match to_candidate(&event) {
                    Some(candidate) => {
                        // The gate runs inside admit(); this task cannot bypass
                        // it, because there is no other way into the stream.
                        if let Ok(mut s) = pump_stream.lock() {
                            s.admit(candidate);
                        }
                    }
                    None => {
                        if let Ok(mut n) = pump_unmapped.lock() {
                            *n += 1;
                        }
                    }
                }
            }
        });

        recorder
            .start()
            .await
            .map_err(|e| CaptureError::Recorder(e.to_string()))?;

        Ok(Self {
            name,
            recorder,
            stream,
            unmapped,
            watcher,
            pump: Some(pump),
        })
    }

    /// Stop observing and take the captured stream.
    pub async fn stop_session(mut self) -> Result<CaptureReport, CaptureError> {
        self.recorder
            .stop()
            .await
            .map_err(|e| CaptureError::Recorder(e.to_string()))?;

        // Let the pump drain anything already queued before we cut it off.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if let Some(pump) = self.pump.take() {
            pump.abort();
            let _ = pump.await;
        }

        // The user may have stopped recording while still inside a field, so
        // nothing has signalled that its text is final. This is the last chance
        // to record it. Done after the pump is stopped, so there is no second
        // writer, and still through admit() so the gate applies.
        if let (Ok(mut watcher), Ok(mut stream)) = (self.watcher.lock(), self.stream.lock()) {
            if let Some(candidate) = watcher.flush(now_ms()) {
                stream.admit(candidate);
            }
        }

        let (actions, exclusions) = {
            let guard = self
                .stream
                .lock()
                .map_err(|_| CaptureError::Recorder("captured stream lock poisoned".into()))?;
            (guard.actions().to_vec(), guard.exclusions().to_vec())
        };
        let unmapped_events = *self.unmapped.lock().unwrap_or_else(|e| e.into_inner());

        Ok(CaptureReport {
            session_name: self.name,
            actions,
            exclusions,
            unmapped_events,
        })
    }

    /// Actions admitted so far, for live progress display.
    pub fn admitted_so_far(&self) -> usize {
        self.stream.lock().map(|s| s.len()).unwrap_or(0)
    }

    pub fn excluded_so_far(&self) -> usize {
        self.stream.lock().map(|s| s.exclusions().len()).unwrap_or(0)
    }
}

/// Feed one event to the text watcher, returning a `Type` candidate when the
/// event finalises a field's contents.
///
/// This is where typed text comes from. `WorkflowEvent::TextInputCompleted` is
/// deliberately not used for it -- see `capture::text` for the measurements.
fn observe_text(
    watcher: &Arc<Mutex<TextFieldWatcher>>,
    event: &WorkflowEvent,
) -> Option<ActionCandidate> {
    let mut watcher = watcher.lock().ok()?;

    match event {
        // A click is how focus moves in practice, and it carries the element,
        // its role, and the owning app all in one.
        WorkflowEvent::Click(e) => {
            let mut identifiers = Vec::new();
            if let Some(p) = &e.process_name {
                identifiers.push(p.clone());
            }
            identifiers.extend(app_identifiers(e.metadata.ui_element.as_ref()));
            if let Some(url) = &e.page_url {
                identifiers.push(url.clone());
            }

            watcher.focus_moved(
                e.metadata.ui_element.as_ref(),
                &e.element_role,
                non_empty(&e.element_text),
                identifiers,
                e.process_name.clone(),
                e.metadata.timestamp.unwrap_or_else(now_ms),
            )
        }

        WorkflowEvent::Keyboard(e) if e.is_key_down => {
            watcher.key_pressed(e.key_code, e.metadata.timestamp.unwrap_or_else(now_ms))
        }

        // Switching applications means focus has left whatever was being
        // watched, even though nothing in the old window announced it -- the
        // user typed and went straight to another app, with no Enter and no
        // click elsewhere.
        //
        // Without this the watch survives the switch and is flushed later by
        // some unrelated event, producing a `type` action that carries the OLD
        // window's element but is ordered AFTER the navigate. Replay then
        // switches windows and types into the previous one. Observed in Step 12
        // (session record-403c9787: three lines typed into Notepad, then a
        // switch to Google Docs, and the typing replayed back into Notepad
        // while the run reported 7/7 succeeded) and reproduced deterministically
        // by `examples/text_capture_probe -- windowswitch`.
        //
        // The pump admits this candidate before the event's own, so flushing
        // here also puts the typing ahead of the navigate, which is the order it
        // actually happened in.
        WorkflowEvent::ApplicationSwitch(e) => {
            watcher.flush(e.metadata.timestamp.unwrap_or_else(now_ms))
        }

        _ => None,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Map a recorder event to one of Phase 1's action types, or `None` if this
/// step does not model it.
///
/// Only the high-level events are mapped. Raw `Mouse`/`Keyboard` events are
/// deliberately ignored: an individual keystroke is not an action, and
/// admitting them would put every raw character into the stream ahead of the
/// text-input event that actually describes what was typed.
fn to_candidate(event: &WorkflowEvent) -> Option<ActionCandidate> {
    match event {
        WorkflowEvent::Click(e) => {
            let mut identifiers = Vec::new();
            if let Some(p) = &e.process_name {
                identifiers.push(p.clone());
            }
            identifiers.extend(app_identifiers(e.metadata.ui_element.as_ref()));
            if let Some(url) = &e.page_url {
                identifiers.push(url.clone());
            }

            Some(ActionCandidate {
                kind: ActionKind::Click,
                process_name: e.process_name.clone(),
                identifiers,
                element_role: Some(e.element_role.clone()),
                element_name: non_empty(&e.element_text),
                payload: None,
                detail: Some(format!("{:?}", e.interaction_type)),
                timestamp_ms: e.metadata.timestamp.unwrap_or_else(now_ms),
            })
        }

        // Deliberately NOT mapped. This event is the recorder's own attempt at
        // the same job `capture::text` now does, and it is not dependable
        // enough to use: measured 1 delivery in 20 typed actions, and the two
        // that arrived in earlier steps carried truncated text. Mapping it as
        // well would mean two `Type` actions for the same field on the ~5% of
        // occasions it does fire, so it is dropped rather than deduplicated.
        //
        // It still counts as an unmapped event, which keeps its arrival rate
        // visible in `CaptureReport::unmapped_events` instead of hiding it.
        WorkflowEvent::TextInputCompleted(_) => None,

        WorkflowEvent::ApplicationSwitch(e) => {
            // Both sides are checked: switching AWAY from a bank names the bank
            // in this event, so gating only on the destination would leak it.
            let mut identifiers = vec![e.to_window_and_application_name.clone()];
            if let Some(p) = &e.to_process_name {
                identifiers.push(p.clone());
            }
            if let Some(w) = &e.from_window_and_application_name {
                identifiers.push(w.clone());
            }
            if let Some(p) = &e.from_process_name {
                identifiers.push(p.clone());
            }

            Some(ActionCandidate {
                kind: ActionKind::Navigate,
                identifiers,
                // The destination's executable. `element_name` below is the
                // window TITLE, which changes as the user works -- a Notepad
                // window is "Untitled - Notepad" until the first keystroke.
                // This is the part that does not move.
                process_name: e.to_process_name.clone(),
                element_role: Some("Window".to_string()),
                element_name: Some(e.to_window_and_application_name.clone()),
                payload: None,
                detail: e
                    .from_window_and_application_name
                    .as_ref()
                    .map(|from| format!("from {from:?}")),
                timestamp_ms: e.metadata.timestamp.unwrap_or_else(now_ms),
            })
        }

        _ => None,
    }
}

/// Every name we can get for the application owning an element.
fn app_identifiers(element: Option<&terminator::UIElement>) -> Vec<String> {
    let Some(el) = element else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let attrs = el.attributes();
    if let Some(app) = attrs.application_name.filter(|s| !s.trim().is_empty()) {
        out.push(app);
    }
    if let Ok(Some(window)) = el.window() {
        if let Some(title) = window.name().filter(|s| !s.trim().is_empty()) {
            out.push(title);
        }
    }
    out
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}
