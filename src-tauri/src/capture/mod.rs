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
pub mod grid;
pub mod text;

use std::sync::atomic::{AtomicUsize, Ordering};
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
pub use grid::GridCellWatcher;
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
    /// Paste operations (`Ctrl+V`) seen during the session.
    ///
    /// Counted, never read. Clipboard content is deliberately not captured, so
    /// this exists to make the resulting gap **visible** rather than silent: a
    /// recording with pastes may be missing data movement that no action
    /// records. Measured — a paste into a Google Sheets cell reaches the
    /// document while capture holds no record of it at all.
    ///
    /// A paste into an ordinary text field is a different matter and IS
    /// captured, because the destination's new value is read directly. So a
    /// non-zero count here means "check whether the destinations were fields",
    /// not "data was definitely lost".
    /// See docs/known-issues/complex-web-grid-capture-unreliable.md.
    pub pastes_observed: usize,
    /// Events the recorder emitted that **capture never saw**.
    ///
    /// Zero is the expected value. A non-zero count means the recording is
    /// incomplete, and specifically that actions may be missing with nothing
    /// else to show for it.
    ///
    /// This exists because the loss is otherwise **silent**. The pump consumes
    /// a `tokio::broadcast` of capacity 1000; a receiver that falls behind gets
    /// `RecvError::Lagged(n)`, and the recorder's own `event_stream` swallows
    /// it -- `tracing::error!("⚠️ Event stream LAGGED!")` then `continue`, with
    /// the skipped events gone. Nothing installs a `tracing` subscriber in this
    /// app, so that message goes nowhere. The pump cannot detect the gap
    /// either: it only ever sees the events it was yielded.
    ///
    /// So it is measured from outside. A second subscription is taken on the
    /// same broadcast which does nothing but count -- no UI Automation reads,
    /// no locks -- and therefore cannot lag. The difference between what it saw
    /// and what the pump handled is this number.
    ///
    /// Two causes are deliberately not separated, because both mean the same
    /// thing to a recording: events dropped by broadcast lag, and events still
    /// queued when `stop_session` cut the pump off. Neither reached capture.
    ///
    /// Sampling costs ~26ms per keystroke in a live grid session (measured
    /// 2026-08-18: key-down 16785us, key-up 9261us), which is what makes
    /// falling behind a real possibility rather than a theoretical one.
    pub events_lost: usize,
    /// Copy → paste pairs seen this session: where a value came from, and where
    /// it went. Positions only, never content.
    ///
    /// A **side channel**, deliberately not part of `actions`. §3 of the
    /// templated-workflows design permits source positions only transiently --
    /// during detection and during a live run -- and never as a durable
    /// position-plus-content pair. Keeping them here means they are available
    /// to pattern detection at stop and then dropped: they are not compiled,
    /// not stored, and `CapturedAction` gains no source field, so every other
    /// consumer of capture is unaffected.
    ///
    /// Empty for recordings that never copy, which includes every workflow
    /// typed from the user reading the source with their eyes -- there is no
    /// observable source interaction in that case, and a stated limitation is
    /// better than an inferred source.
    pub source_links: Vec<grid::SourceLink>,
}

/// One Record Mode capture session.
pub struct CaptureSession {
    name: String,
    recorder: WorkflowRecorder,
    stream: Arc<Mutex<CapturedStream>>,
    unmapped: Arc<Mutex<usize>>,
    /// Ctrl+V occurrences. See `CaptureReport::pastes_observed`.
    pastes: Arc<Mutex<usize>>,
    /// Produces the `Type` actions. Shared with the pump so `stop_session` can
    /// flush a field the user was still in when they stopped recording.
    watcher: Arc<Mutex<TextFieldWatcher>>,
    grid: Arc<Mutex<GridCellWatcher>>,
    pump: Option<JoinHandle<()>>,
    /// Events the pump actually handled, and events that existed to be handled.
    /// See `CaptureReport::events_lost` for why the second number is taken from
    /// a separate subscription rather than from the pump.
    handled: Arc<AtomicUsize>,
    seen: Arc<AtomicUsize>,
    census: Option<JoinHandle<()>>,
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
            // ON, so that a copy made by any gesture marks a source -- the
            // right-click menu, an Edit menu, an application's own Copy button,
            // Ctrl+Insert. The `Ctrl+C` hook in `capture::grid` sees only the
            // keystroke, which is a keyboard-shaped hole in a mechanism that is
            // supposed to be about "the user took this value".
            record_clipboard: true,
            // **Zero, and this is the §3 enforcement.** The recorder truncates
            // clipboard content to this length before it builds the event, so
            // at 0 the event carries an empty string and the copied VALUE never
            // enters this process at all. Change detection is unaffected: the
            // recorder hashes the *full* content before truncating, so a copy is
            // still detected -- we get the trigger without the data.
            //
            // Enforcing it here rather than by remembering not to read
            // `ClipboardEvent::content` is deliberate. A rule the type system
            // cannot state should at least be stated where it cannot be
            // forgotten.
            max_clipboard_content_length: 0,
            // Not needed for Phase 1 and another source of captured content we
            // would have to gate.
            record_browser_tab_navigation: false,
            ..Default::default()
        };

        // One run's numbers are its own. `timing_split` is read at stop.
        grid::reset_timing();

        let mut recorder = WorkflowRecorder::new(name.clone(), config);

        // Subscribe BEFORE start(): event_tx is a broadcast channel, so a
        // subscription created afterwards would miss everything in between.
        let mut events = Box::pin(recorder.event_stream());

        // A second subscription on the same broadcast, for one purpose: to be
        // fast enough that it cannot lag, so its count is the true number of
        // events. The pump does UI Automation reads costing ~26ms per keystroke
        // and CAN fall behind; when it does, the recorder drops the events and
        // says so only through a `tracing` macro this app has no subscriber
        // for. Comparing the two counts is what makes that loss visible.
        //
        // Deliberately does no work at all -- no locks, no reads, no mapping.
        // Anything it did here would be a way for the measurement to acquire
        // the same problem it exists to detect.
        let mut census = Box::pin(recorder.event_stream());

        let stream = Arc::new(Mutex::new(CapturedStream::new(exclusions)));
        let unmapped = Arc::new(Mutex::new(0usize));
        let pastes = Arc::new(Mutex::new(0usize));
        let watcher = Arc::new(Mutex::new(TextFieldWatcher::new()));
        let grid = Arc::new(Mutex::new(GridCellWatcher::new()));

        let pump_stream = Arc::clone(&stream);
        let pump_unmapped = Arc::clone(&unmapped);
        let pump_pastes = Arc::clone(&pastes);
        let pump_watcher = Arc::clone(&watcher);
        let pump_grid = Arc::clone(&grid);

        let handled = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(AtomicUsize::new(0));
        let census_seen = Arc::clone(&seen);
        let census = tokio::spawn(async move {
            while census.next().await.is_some() {
                census_seen.fetch_add(1, Ordering::Relaxed);
            }
        });

        let pump_handled = Arc::clone(&handled);
        let pump = tokio::spawn(async move {
            while let Some(event) = events.next().await {
                // Counted before any work, so an event that arrives and is then
                // cut off mid-processing still counts as reaching the pump. The
                // number this feeds is about events that never arrived at all.
                pump_handled.fetch_add(1, Ordering::Relaxed);
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

                // A grid cell has no element for the watcher above to follow --
                // it is created by typing and destroyed by committing -- so it
                // gets its own path. See `capture::grid`.
                if let Some(cell) = observe_grid(&pump_grid, &event) {
                    if let Ok(mut s) = pump_stream.lock() {
                        s.admit(cell);
                    }
                }

                // Count pastes. Their CONTENT is deliberately not captured, so
                // this is the only trace that data may have moved without an
                // action to show for it.
                if let WorkflowEvent::Keyboard(e) = &event {
                    if e.is_key_down && e.ctrl_pressed && e.key_code == 0x56 {
                        if let Ok(mut n) = pump_pastes.lock() {
                            *n += 1;
                        }
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
            pastes,
            watcher,
            grid,
            pump: Some(pump),
            handled,
            seen,
            census: Some(census),
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
        // Stopped after the pump, never before: the census is the reference the
        // pump is measured against, so cutting it off first would hide exactly
        // the shortfall it exists to reveal.
        if let Some(census) = self.census.take() {
            census.abort();
            let _ = census.await;
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

        // Same for a cell edit left uncommitted. Note this emits the last
        // SAMPLED value rather than re-reading -- by now the editor may be gone,
        // and re-reading it is exactly the mistake that produced U+FEFF
        // payloads. See `capture::grid`.
        if let (Ok(mut grid), Ok(mut stream)) = (self.grid.lock(), self.stream.lock()) {
            if let Some(candidate) = grid.flush(now_ms()) {
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

        // The sampling cost, measured rather than assumed. Key-up sampling was
        // added to close the one-keystroke lag, and this is where the price of
        // it becomes a number: calls and mean microseconds for each direction,
        // printed once per session so a real recording answers the question.
        {
            let ((dc, dus), (uc, uus)) = grid::timing_split();
            let mean = |calls: u64, micros: u64| micros.checked_div(calls).unwrap_or(0);
            eprintln!(
                "[paradigm] grid sampling: key-down {dc} calls, {} us mean; key-up {uc} calls, {} us mean; total {} calls, {} us",
                mean(dc, dus),
                mean(uc, uus),
                dc + uc,
                dus + uus
            );
        }

        // Events that existed versus events the pump got to. See
        // `CaptureReport::events_lost`. Printed unconditionally, including the
        // zero, so a clean recording is positively confirmed rather than merely
        // not complained about -- the distinction that made the truncation
        // defect opaque for so long.
        let seen = self.seen.load(Ordering::Relaxed);
        let handled = self.handled.load(Ordering::Relaxed);
        let events_lost = seen.saturating_sub(handled);
        eprintln!(
            "[paradigm] event census: {seen} emitted, {handled} handled, {events_lost} LOST{}",
            if events_lost == 0 {
                ""
            } else {
                " -- this recording is incomplete"
            }
        );

        Ok(CaptureReport {
            session_name: self.name,
            actions,
            exclusions,
            unmapped_events,
            events_lost,
            pastes_observed: *self.pastes.lock().unwrap_or_else(|e| e.into_inner()),
            // Drained rather than copied: the watcher must not hand the same
            // positions to a second reader, and nothing should hold them after
            // the report they belong to.
            source_links: self
                .grid
                .lock()
                .map(|mut g| g.take_links())
                .unwrap_or_default(),
        })
    }

    /// Mark the currently focused position as a source, on the user's explicit
    /// say-so. Returns whether a position could be read.
    ///
    /// Called from the global-shortcut handler rather than through the frontend,
    /// which is a deliberate departure from how the Record Mode shortcut works.
    /// That one emits an event and lets the UI run the same flow the button
    /// runs, because *when* it happens does not matter to a millisecond. This
    /// one is a reading of "what is focused right now", so a round trip through
    /// the webview would resolve the position after the round trip rather than
    /// at the keypress.
    ///
    /// Briefly locks the same watcher the pump uses. The lock is held for one
    /// position read; the pump's own hold is per event, so the worst case is
    /// that one of them waits for the other.
    pub fn mark_source(&self, timestamp_ms: u64) -> grid::MarkOutcome {
        self.grid
            .lock()
            .map(|mut g| g.note_marked_source(timestamp_ms))
            .unwrap_or(grid::MarkOutcome::Unavailable)
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

        WorkflowEvent::Keyboard(e) if e.is_key_down => watcher.key_pressed_with_modifiers(
            e.key_code,
            e.ctrl_pressed,
            e.metadata.timestamp.unwrap_or_else(now_ms),
        ),

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

/// Feed one event to the grid watcher, returning a `Type` candidate when a cell
/// edit finishes.
///
/// Separate from `observe_text` because the two disagree about when to read.
/// `TextFieldWatcher` reads its element when the edit ends; a grid cell's editor
/// does not survive that moment, so this samples on the way through and emits
/// what it last saw. See `capture::grid` for the measurements.
fn observe_grid(
    grid: &Arc<Mutex<GridCellWatcher>>,
    event: &WorkflowEvent,
) -> Option<ActionCandidate> {
    let mut grid = grid.lock().ok()?;

    match event {
        // Keyboard events carry no `ui_element` -- measured, 0 of 15 key-downs
        // in a driven Sheets session -- so the watcher resolves focus itself.
        // The modifier state is passed for two reasons now: Ctrl+C and Ctrl+V
        // are where a value's SOURCE position can be observed, and Alt+Tab is a
        // window switch that must not be mistaken for a committing Tab. Only
        // `capture::grid` knows what a spreadsheet cell is -- the generic
        // `to_candidate` path still knows nothing about spreadsheets,
        // clipboards or modifiers.
        //
        // Key-UP events are forwarded too, and the `is_key_down` guard that
        // used to sit here is gone. A key-down samples the editor before the OS
        // has processed that key, so the character just typed is visible only on
        // the way back up; dropping key-up is what recorded a value one
        // character short whenever an edit ended without Enter or Tab. The text
        // watcher above still takes key-downs only -- it reads its element when
        // the edit ends, so it never had the lag.
        WorkflowEvent::Keyboard(e) => grid.observe_key(
            e.key_code,
            e.is_key_down,
            e.ctrl_pressed,
            e.alt_pressed,
            e.metadata.timestamp.unwrap_or_else(now_ms),
        ),

        // Clicks never name the editor, but they do say which app is in play,
        // and the exclusion gate needs that.
        WorkflowEvent::Click(e) => {
            let mut identifiers = Vec::new();
            if let Some(p) = &e.process_name {
                identifiers.push(p.clone());
            }
            identifiers.extend(app_identifiers(e.metadata.ui_element.as_ref()));
            if let Some(url) = &e.page_url {
                identifiers.push(url.clone());
            }
            // Also the seam where a sheet switch is noticed. Deliberately here
            // and not in `to_candidate`: `observe_grid` is already the
            // spreadsheet-specific path, so capture's generic mapping stays
            // app-agnostic and only `capture::grid` knows what a sheet tab is.
            grid.note_click(
                &e.element_role,
                non_empty(&e.element_text).as_deref(),
                identifiers,
                e.process_name.clone(),
            );
            None
        }

        WorkflowEvent::ApplicationSwitch(e) => {
            grid.flush(e.metadata.timestamp.unwrap_or_else(now_ms))
        }

        // A copy made by a gesture the keystroke hook cannot see. Marks a
        // source position and produces no action of its own -- a copy is not
        // something to replay, it is something that says where a value came
        // from.
        //
        // `e.content` is deliberately never read. It is also empty: capture
        // sets `max_clipboard_content_length` to 0, so the copied value never
        // reaches this process. See `CaptureSession::start_session`.
        WorkflowEvent::Clipboard(e) => {
            grid.note_clipboard_copy(e.metadata.timestamp.unwrap_or_else(now_ms));
            None
        }

        _ => None,
    }
}

pub(crate) fn now_ms() -> u64 {
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
                // One UI Automation call, on an element the event already
                // resolved. `click_position` is also on the event and is
                // cheaper still, but it records where the user clicked rather
                // than what they clicked -- two clicks on one element differ,
                // and one click on each of two identical-looking elements may
                // not. Bounds describe the element.
                element_bounds: bounds_of(e.metadata.ui_element.as_ref()),
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
                // The window's own rectangle. Weaker identity than an
                // element's -- two windows of one application often coincide --
                // but it costs the same one call and a window switch is rare.
                element_bounds: bounds_of(e.metadata.ui_element.as_ref()),
                timestamp_ms: e.metadata.timestamp.unwrap_or_else(now_ms),
            })
        }

        _ => None,
    }
}

/// An element's rectangle, when it has one.
///
/// One UI Automation call, and only on events that produce an action -- never
/// on the keystroke path, where `capture::grid` samples on every key in both
/// directions and a per-key call would cost more than the whole identity read
/// that was removed from it.
///
/// `None` is ordinary rather than exceptional: the event may carry no element,
/// and an element may refuse to report bounds. Callers treat it as "no
/// positional identity available", not as a failure.
fn bounds_of(element: Option<&terminator::UIElement>) -> Option<(f64, f64, f64, f64)> {
    element.and_then(|el| el.bounds().ok())
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
