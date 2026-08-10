//! Replay a stored playbook back through Terminator. Happy path only.
//!
//! Happy path means no retry loop and no drift repair -- those are Phase 2. It
//! does NOT mean assuming success. Every Terminator call's `Result` is checked
//! and reported as what actually happened: found or not found, succeeded or
//! errored. Step 5's probe printed a success message for a click whose `Result`
//! it discarded; nothing here does that.
//!
//! ## Design decision: redacted steps HALT the run
//!
//! A step compiled with `"redacted": true` has `"text": "[REDACTED]"` stored.
//! The real value was never persisted -- that is the whole point of Step 4b/5's
//! redaction discipline. Replay therefore cannot reproduce it, and has three
//! options. It halts.
//!
//! * **Typing the literal `[REDACTED]`** would write a known-wrong value into a
//!   real field. In the recorded case that field was a password, so replay
//!   would submit a bad credential -- and repeated runs could trigger an
//!   account lockout. It corrupts whatever the user is filling in.
//!
//! * **Skipping the step and continuing** produces a run that silently does
//!   less than it recorded. The steps that follow generally assume the field
//!   was filled: replaying "click Sign in" after skipping the password submits
//!   an empty form. The run would report `completed` while having done
//!   something different from what was recorded, which is the worst outcome of
//!   the three because it is invisible.
//!
//! * **Halting** keeps one invariant worth having: a run marked `completed` did
//!   everything the playbook recorded. It is also the only option that leaves
//!   room for the Phase 2 answer -- prompt the user for the value, or pull it
//!   from a credential store -- because it does not bake in the assumption that
//!   the step was optional.
//!
//! ## `aborted`, not `failed`
//!
//! A halted run is recorded as `aborted` with an `aborted` log event, not
//! `failed`. The distinction is load-bearing rather than cosmetic: `failed`
//! means something went wrong -- a missing element, an action that errored --
//! and is the class of outcome Phase 2's retry and drift-repair logic will key
//! off. A redaction halt is the system correctly declining, and retrying it
//! would fail identically every time because the value still will not exist.
//! Collapsing the two would make every halt look retryable.

pub mod journal;

use std::sync::Once;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::Value;
use terminator::{AutomationError, Desktop, UIElement};

use crate::compile::store::{self, StoredPlaybook, StoredStep};
use crate::db::DbError;

use journal::{
    StepLogEntry, EVENT_ABORTED, EVENT_EXECUTE, EVENT_FAILURE, STATUS_ABORTED, STATUS_COMPLETED,
    STATUS_FAILED,
};

/// How long to wait for a top-level WINDOW. Step 2 proved 5s is enough for a
/// realized window; this allows margin without stalling a failed lookup.
///
/// Deliberately shorter than the element budget below. A window either exists
/// or it does not — waiting longer buys nothing — and `navigate` falls back to
/// `activate_application` when the selector misses, so a miss here is not fatal.
const WINDOW_LOCATE_TIMEOUT: Duration = Duration::from_secs(8);

/// How long to wait for an ELEMENT inside an application.
///
/// Longer than the window budget because elements in modern web applications
/// render lazily, and 8s turned out to be barely wider than one real widget's
/// latency. Gmail's recipient picker (`role:Group|name:"To - Select contacts"`)
/// was measured arriving **6,743ms** after Compose is clicked — a 19% margin
/// against 8s, which normal variance closes. Run 7b99fae2 is what that looks
/// like when it does: "element not found" for an element that was simply late.
/// See docs/known-issues/dynamic-contact-picker-replay-fails.md.
///
/// 15s is ~2.2x the measured value. 3x was considered and rejected: the
/// measurement came from a loaded machine (90+ browser tabs, three days
/// uptime), so 6.7s is likely near the slow end rather than the median.
///
/// The cost is real and worth naming: a genuinely wrong selector now takes 15s
/// to report an honest "not found" instead of 8s. That is accepted because the
/// opposite error — failing a step whose element was merely slow — fails the
/// whole run and forces the user to re-record, which is worse than waiting.
///
/// If real-world failures recur at 15s, the answer is not a larger number but a
/// readiness signal: wait for the element to appear rather than for a deadline
/// to expire.
const ELEMENT_LOCATE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Db(#[from] DbError),

    #[error("playbook {0} has no steps to replay")]
    EmptyPlaybook(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepResult {
    /// Performed through the element's own action.
    Executed,
    /// Performed, but only after falling back to a real mouse click at the
    /// element's coordinates. See docs/known-issues/terminator-multi-monitor-visibility.md.
    ExecutedViaCoordinateClick,
    /// `read` steps have nothing to drive in Phase 1 -- nothing consumes a read
    /// result yet -- so they are logged as executed without acting.
    NoActionNeeded,
    /// Halted because the step needs a value that was deliberately not stored.
    HaltedRedacted,
    /// The target could not be located.
    FailedNotFound,
    /// The target was located but the action itself errored.
    FailedAction,
    /// Something was located, but it is not what the recording targeted.
    ///
    /// Distinct from `FailedNotFound` on purpose: "found nothing" and "found the
    /// wrong thing" are different problems with different causes, and conflating
    /// them would hide exactly the failure this exists to surface. See
    /// docs/known-issues/selector-matching-precision.md.
    FailedWrongTarget,
}

impl StepResult {
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            StepResult::HaltedRedacted
                | StepResult::FailedNotFound
                | StepResult::FailedAction
                | StepResult::FailedWrongTarget
        )
    }

    pub fn label(&self) -> &'static str {
        match self {
            StepResult::Executed => "executed",
            StepResult::ExecutedViaCoordinateClick => "executed (coordinate-click fallback)",
            StepResult::NoActionNeeded => "no action needed",
            StepResult::HaltedRedacted => "HALTED (redacted value unavailable)",
            StepResult::FailedNotFound => "FAILED (element not found)",
            StepResult::FailedAction => "FAILED (action errored)",
            StepResult::FailedWrongTarget => "FAILED (resolved the wrong element)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StepOutcome {
    pub step_order: i64,
    pub action_type: String,
    pub selector: Option<String>,
    pub result: StepResult,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ReplayReport {
    pub run_id: String,
    pub playbook_id: String,
    pub playbook_name: String,
    pub status: String,
    pub steps_total: usize,
    pub outcomes: Vec<StepOutcome>,
}

impl ReplayReport {
    pub fn steps_attempted(&self) -> usize {
        self.outcomes.len()
    }
    pub fn failure(&self) -> Option<&StepOutcome> {
        self.outcomes.iter().find(|o| o.result.is_failure())
    }
}

/// Opt the process into per-monitor DPI awareness exactly once.
///
/// Replay mixes UI Automation bounds (physical pixels) with screen coordinates
/// for its click fallback. Without this the fallback silently clicks the wrong
/// place -- see docs/known-issues/terminator-multi-monitor-visibility.md. It
/// lives here rather than in the caller so no caller can forget it.
pub fn ensure_dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::UI::HiDpi::{
                SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            };
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    });
}

/// What a compiled step's payload JSON carries.
struct StepPayload {
    selector: Option<String>,
    text: Option<String>,
    redacted: bool,
    target_name: Option<String>,
    app: Option<String>,
    /// Owning executable, recorded from 2026-08-09. `None` for anything stored
    /// before that, and for events that reported no process name.
    ///
    /// Parsed but not yet consumed by replay: it exists so a future ambiguity
    /// check has the data it needs. Capture, compile and storage all carry it
    /// today (verified end to end), so the collection half is done and only the
    /// use of it is outstanding.
    #[allow(dead_code)]
    process: Option<String>,
}

impl StepPayload {
    /// The selector with a `process:` prefix when one can be built.
    ///
    /// This exists because `Locator::all()` refuses a desktop-wide selector
    /// outright -- "Selector must include 'process:' prefix" -- while
    /// `Locator::first()` accepts one. Counting candidates, and therefore
    /// detecting ambiguity at all, is only possible on a scoped selector.
    ///
    /// Returns `None` when there is no process name, which is the case for
    /// every playbook recorded before this field existed. Callers must then
    /// behave exactly as they did before rather than treating it as an error.
    ///
    /// Unused in the product today: the counting path it was written for turned
    /// out not to work (see the note above `navigate`). Kept, and kept tested,
    /// because building the prefix correctly -- including the empty-string and
    /// missing-selector cases -- is the part a future attempt would otherwise
    /// get wrong again.
    #[allow(dead_code)]
    fn scoped_selector(&self) -> Option<String> {
        let selector = self.selector.as_deref()?;
        let process = self.process.as_deref()?.trim();
        if process.is_empty() {
            return None;
        }
        Some(format!("process:{process}|{selector}"))
    }
}

fn parse_payload(raw: &str) -> StepPayload {
    let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    StepPayload {
        selector: v["target"]["selector"].as_str().map(str::to_string),
        text: v["text"].as_str().map(str::to_string),
        redacted: v["redacted"].as_bool().unwrap_or(false),
        target_name: v["target"]["name"].as_str().map(str::to_string),
        app: v["app"].as_str().map(str::to_string),
        process: v["process"].as_str().map(str::to_string),
    }
}

// A `count_candidates` helper lived here, counting a scoped selector's matches
// with `Locator::all()` so `navigate` could refuse an ambiguous target. It was
// implemented, measured, and removed: `all()` on a `process:`-scoped selector
// returns every top-level window of that process and ignores the role and name,
// so the number answers a different question than the one asked. Deleted rather
// than kept, because three lines are cheap to rewrite and unused code that
// looks purposeful is not.
//
// `StepPayload::scoped_selector` is kept and tested for whatever counting path
// a future attempt uses.

/// Is the element we resolved the one the recording targeted?
///
/// Selectors match names by CONTAINMENT (`contains_name` in terminator), so
/// `name:Paradigm` matches any element whose name contains "Paradigm" -- a real
/// stored selector was measured resolving onto
/// "Paradigm Text Capture Probe and 63 more pages - Personal - Microsoft Edge".
/// Nothing in a successful `first()` distinguishes that from a correct match.
/// See docs/known-issues/selector-matching-precision.md.
///
/// This compares for equality instead, with exactly one tolerance: a single
/// leading `*`, the near-universal Windows marker for unsaved changes.
///
/// That tolerance is not a lenient default; it is the narrowest rule that covers
/// a measured false positive. A window recorded as
/// `"DriftProbe - Personal - Microsoft Edge"` becomes
/// `"*DriftProbe - Personal - Microsoft Edge"` the moment its content is edited,
/// and strict equality would reject that legitimate same-window match while
/// containment accepts it. The tolerance still rejects the Paradigm collision,
/// which is neither the recorded name nor the recorded name with a `*`.
///
/// Note what this can and cannot do. It only ever rejects matches that
/// containment ACCEPTED -- any other drift already fails to resolve today, so
/// this adds no new failure there. It also cannot detect two windows genuinely
/// sharing a name; that needs a candidate count, which the library does not
/// expose (see replay-window-selector-ambiguity.md).
fn resolved_is_recorded_target(recorded: &str, resolved: &str) -> bool {
    if recorded == resolved {
        return true;
    }
    resolved
        .strip_prefix('*')
        .map(|undecorated| undecorated == recorded)
        .unwrap_or(false)
}

/// Best-effort name of the foreground application, for `system_state_json`.
fn foreground_app(desktop: &Desktop) -> String {
    desktop
        .focused_element()
        .ok()
        .and_then(|el| {
            el.application()
                .ok()
                .flatten()
                .and_then(|app| app.name())
                .or_else(|| el.window().ok().flatten().and_then(|w| w.name()))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Replay every step of a stored playbook.
pub async fn replay(
    conn: &mut Connection,
    desktop: &Desktop,
    playbook_id: &str,
) -> Result<ReplayReport, ReplayError> {
    ensure_dpi_aware();

    let playbook: StoredPlaybook = store::load(conn, playbook_id)?;
    if playbook.steps.is_empty() {
        return Err(ReplayError::EmptyPlaybook(playbook_id.to_string()));
    }

    let run_id = journal::start_run(conn, playbook_id)?;
    let mut outcomes = Vec::new();
    // None until a step ends the run. A redaction halt ends it as `aborted`
    // (declined), anything else that stops it ends as `failed` (went wrong).
    let mut terminal_status: Option<&str> = None;

    for step in &playbook.steps {
        let payload = parse_payload(&step.action_payload_json);
        let outcome = execute_step(desktop, step, &payload).await;

        let halted = outcome.result == StepResult::HaltedRedacted;
        let is_failure = outcome.result.is_failure();
        let event_type = match (is_failure, halted) {
            (_, true) => EVENT_ABORTED,
            (true, false) => EVENT_FAILURE,
            (false, false) => EVENT_EXECUTE,
        };

        journal::log_step(
            conn,
            &run_id,
            &StepLogEntry {
                playbook_step_id: Some(step.id.clone()),
                step_order: step.step_order,
                action_type: step.action_type.clone(),
                target_ui_context_json: serde_json::json!({
                    "selector": payload.selector,
                    "name": payload.target_name,
                    "app": payload.app,
                    "result": outcome.result.label(),
                    "detail": outcome.detail,
                })
                .to_string(),
                // A halted redacted step is exactly the case where a payload
                // must not be written: is_sensitive forces data_payload NULL.
                data_payload: if payload.redacted {
                    None
                } else {
                    payload.text.clone()
                },
                is_sensitive: payload.redacted,
                event_type: event_type.to_string(),
                // Local execution: no model involved, so no cost.
                model_source: None,
                cost: 0.0,
                foreground_app: foreground_app(desktop),
            },
        )?;

        outcomes.push(outcome);

        if is_failure {
            terminal_status = Some(if halted { STATUS_ABORTED } else { STATUS_FAILED });
            break; // No retry, no repair. That is Phase 2.
        }
    }

    let status = terminal_status.unwrap_or(STATUS_COMPLETED);
    journal::finish_run(conn, &run_id, status)?;

    Ok(ReplayReport {
        run_id,
        playbook_id: playbook_id.to_string(),
        playbook_name: playbook.name,
        status: status.to_string(),
        steps_total: playbook.steps.len(),
        outcomes,
    })
}

async fn execute_step(
    desktop: &Desktop,
    step: &StoredStep,
    payload: &StepPayload,
) -> StepOutcome {
    let mk = |result: StepResult, detail: String| StepOutcome {
        step_order: step.step_order,
        action_type: step.action_type.clone(),
        selector: payload.selector.clone(),
        result,
        detail,
    };

    // 1. Redacted steps halt. See the module docs for why.
    if payload.redacted {
        return mk(
            StepResult::HaltedRedacted,
            format!(
                "step {} requires a value that was withheld for security and was never \
                 stored, so it cannot be auto-replayed. Target: {:?}. Replay stopped rather \
                 than typing a placeholder into a real field or skipping the step silently.",
                step.step_order,
                payload.target_name.as_deref().unwrap_or("<unnamed>")
            ),
        );
    }

    // 2. read: nothing consumes a read result in Phase 1.
    if step.action_type == "read" {
        return mk(
            StepResult::NoActionNeeded,
            "read step: no side effect to reproduce in Phase 1".to_string(),
        );
    }

    // 3. navigate: activate the target window, falling back to the app name.
    if step.action_type == "navigate" {
        return navigate(desktop, payload, &mk).await;
    }

    // 4. click / type both need the element first.
    let Some(selector) = payload.selector.as_deref() else {
        return mk(
            StepResult::FailedNotFound,
            "step has no selector, so its target cannot be located".to_string(),
        );
    };

    let element = match desktop
        .locator(selector)
        .first(Some(ELEMENT_LOCATE_TIMEOUT))
        .await
    {
        Ok(el) => el,
        Err(e) => {
            return mk(
                StepResult::FailedNotFound,
                format!(
                    "selector {selector:?} matched nothing within {:?}: {e}",
                    ELEMENT_LOCATE_TIMEOUT
                ),
            )
        }
    };

    // Confirm this is the recorded target BEFORE acting on it. Nothing has
    // happened yet, so refusing here costs nothing; acting on the wrong element
    // cannot be undone.
    if let Some(recorded) = payload.target_name.as_deref() {
        let resolved = element.name().unwrap_or_default();
        if !resolved_is_recorded_target(recorded, &resolved) {
            return mk(
                StepResult::FailedWrongTarget,
                format!(
                    "selector {selector:?} resolved to the wrong element -- names match by \
                     containment, not equality.\n  recorded: {recorded:?}\n  resolved: {resolved:?}\n\
                     Refusing to act: this is how a replay writes to the wrong place while \
                     reporting success."
                ),
            );
        }
    }

    match step.action_type.as_str() {
        "click" => click(desktop, &element, selector, &mk),
        "type" => type_text(&element, payload, &mk),
        other => mk(
            StepResult::FailedAction,
            format!("unsupported action_type {other:?}"),
        ),
    }
}

async fn navigate(
    desktop: &Desktop,
    payload: &StepPayload,
    mk: &impl Fn(StepResult, String) -> StepOutcome,
) -> StepOutcome {
    if let Some(selector) = payload.selector.as_deref() {
        // An ambiguity check belongs here -- a generic selector like
        // `role:Window|name:"Untitled - Notepad"` can match several real
        // windows, and silently taking the first is how a run reported
        // "Completed, 12/12 succeeded" while typing into the wrong one.
        //
        // It was implemented and removed. Counting with `Locator::all()` on a
        // `process:`-scoped selector does not work: measured, it returns every
        // top-level window of that process and ignores the role and name
        // entirely, so a browser with three windows reports three matches for
        // any selector. Failing on that would break working playbooks.
        // See docs/known-issues/replay-window-selector-ambiguity.md.
        match desktop
            .locator(selector)
            .first(Some(WINDOW_LOCATE_TIMEOUT))
            .await
        {
            Ok(window) => {
                // Same check as for elements, and this is the site where the
                // measured collision happened: `role:Window|name:Paradigm`
                // resolving onto a browser window.
                if let Some(recorded) = payload.target_name.as_deref() {
                    let resolved = window.name().unwrap_or_default();
                    if !resolved_is_recorded_target(recorded, &resolved) {
                        return mk(
                            StepResult::FailedWrongTarget,
                            format!(
                                "window selector {selector:?} resolved to the wrong window -- \
                                 names match by containment, not equality.\n  recorded: \
                                 {recorded:?}\n  resolved: {resolved:?}\n Refusing to activate it."
                            ),
                        );
                    }
                }
                return match window.activate_window() {
                    Ok(()) => mk(
                        StepResult::Executed,
                        format!("activated window via {selector:?}"),
                    ),
                    Err(e) => mk(
                        StepResult::FailedAction,
                        format!("found window via {selector:?} but activate_window failed: {e}"),
                    ),
                }
            }
            Err(e) => {
                // Fall through to the app-name attempt, but say what happened.
                if let Some(app) = payload.app.as_deref() {
                    return match desktop.activate_application(app) {
                        Ok(()) => mk(
                            StepResult::Executed,
                            format!(
                                "window selector {selector:?} not found ({e}); \
                                 activated application {app:?} instead"
                            ),
                        ),
                        Err(e2) => mk(
                            StepResult::FailedNotFound,
                            format!(
                                "window selector {selector:?} not found ({e}) and \
                                 activate_application({app:?}) failed: {e2}"
                            ),
                        ),
                    };
                }
                return mk(
                    StepResult::FailedNotFound,
                    format!("window selector {selector:?} not found: {e}"),
                );
            }
        }
    }

    match payload.app.as_deref() {
        Some(app) => match desktop.activate_application(app) {
            Ok(()) => mk(
                StepResult::Executed,
                format!("activated application {app:?}"),
            ),
            Err(e) => mk(
                StepResult::FailedNotFound,
                format!("activate_application({app:?}) failed: {e}"),
            ),
        },
        None => mk(
            StepResult::FailedNotFound,
            "navigate step has neither a selector nor an app name".to_string(),
        ),
    }
}

fn click(
    desktop: &Desktop,
    element: &UIElement,
    selector: &str,
    mk: &impl Fn(StepResult, String) -> StepOutcome,
) -> StepOutcome {
    match element.click() {
        Ok(_) => mk(
            StepResult::Executed,
            format!("clicked {selector:?} via element.click()"),
        ),

        // The known multi-monitor defect: is_visible() tests the element rect
        // against the PRIMARY monitor's work area, so anything on a second
        // display is refused. Step 2 proved a real coordinate click works.
        Err(AutomationError::ElementNotVisible(msg)) => match element.bounds() {
            Ok((x, y, w, h)) if w > 0.0 && h > 0.0 => {
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                match desktop.click_at_coordinates(cx, cy) {
                    Ok(()) => mk(
                        StepResult::ExecutedViaCoordinateClick,
                        format!(
                            "element.click() refused ({msg}); clicked real coordinates \
                             ({cx:.0}, {cy:.0}) instead -- known multi-monitor defect"
                        ),
                    ),
                    Err(e) => mk(
                        StepResult::FailedAction,
                        format!("element.click() refused ({msg}) and coordinate click failed: {e}"),
                    ),
                }
            }
            Ok((_, _, w, h)) => mk(
                StepResult::FailedAction,
                format!("element.click() refused ({msg}) and bounds are unusable: {w}x{h}"),
            ),
            Err(e) => mk(
                StepResult::FailedAction,
                format!("element.click() refused ({msg}) and bounds() failed: {e}"),
            ),
        },

        Err(e) => mk(
            StepResult::FailedAction,
            format!("element.click() on {selector:?} failed: {e}"),
        ),
    }
}

fn type_text(
    element: &UIElement,
    payload: &StepPayload,
    mk: &impl Fn(StepResult, String) -> StepOutcome,
) -> StepOutcome {
    let Some(text) = payload.text.as_deref() else {
        return mk(
            StepResult::FailedAction,
            "type step has no text to enter".to_string(),
        );
    };

    // focus() failing is not fatal -- type_text focuses internally -- but it is
    // reported rather than swallowed.
    let focus_note = match element.focus() {
        Ok(()) => String::new(),
        Err(e) => format!(" (focus() failed first: {e})"),
    };

    match element.type_text(text, false) {
        Ok(()) => mk(
            StepResult::Executed,
            format!("typed {} character(s){focus_note}", text.len()),
        ),
        Err(e) => mk(
            StepResult::FailedAction,
            format!("type_text failed: {e}{focus_note}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_compiled_type_payload() {
        let raw = r#"{"app":"msedge.exe","redacted":true,
                      "target":{"name":"Password","raw_role":"Edit",
                                "selector":"role:Edit|name:Password"},
                      "text":"[REDACTED]"}"#;
        let p = parse_payload(raw);
        assert!(p.redacted);
        assert_eq!(p.selector.as_deref(), Some("role:Edit|name:Password"));
        assert_eq!(p.target_name.as_deref(), Some("Password"));
        assert_eq!(p.text.as_deref(), Some("[REDACTED]"));
    }

    #[test]
    fn the_real_measured_collision_is_rejected() {
        // The actual observed case: a stored selector `role:Window|name:Paradigm`
        // resolving onto a browser window, from
        // docs/known-issues/selector-matching-precision.md.
        assert!(
            !resolved_is_recorded_target(
                "Paradigm",
                "Paradigm Text Capture Probe and 63 more pages - Personal - Microsoft Edge"
            ),
            "the measured production collision must be rejected"
        );
    }

    #[test]
    fn an_exact_match_is_accepted() {
        assert!(resolved_is_recorded_target("Text editor", "Text editor"));
        assert!(resolved_is_recorded_target(
            "Untitled - Notepad",
            "Untitled - Notepad"
        ));
    }

    #[test]
    fn the_unsaved_changes_marker_is_tolerated() {
        // Measured false positive: a window recorded before editing gains a
        // leading '*' once its content changes. Strict equality would reject
        // this legitimate same-window match.
        assert!(resolved_is_recorded_target(
            "DriftProbe - Personal - Microsoft Edge",
            "*DriftProbe - Personal - Microsoft Edge"
        ));
        assert!(resolved_is_recorded_target(
            "Untitled - Notepad",
            "*Untitled - Notepad"
        ));
    }

    #[test]
    fn the_star_tolerance_does_not_open_the_containment_hole() {
        // The tolerance is exactly one leading '*' and nothing more. It must not
        // become a general prefix or substring allowance.
        assert!(!resolved_is_recorded_target("Paradigm", "*Paradigm Extra"));
        assert!(!resolved_is_recorded_target("Notepad", "Untitled - Notepad"));
        assert!(!resolved_is_recorded_target("To", "Write your prompt to Claude"));
    }

    #[test]
    fn identical_names_on_different_windows_are_not_detectable_here() {
        // Documents a known limit rather than a behaviour: when two windows
        // genuinely share a name, the resolved name equals the recorded one and
        // this check cannot tell them apart. That needs a candidate count, which
        // the library does not expose -- see replay-window-selector-ambiguity.md.
        assert!(resolved_is_recorded_target(
            "Untitled - Notepad",
            "Untitled - Notepad"
        ));
    }

    #[test]
    fn a_process_name_produces_a_scoped_selector() {
        let raw = r#"{"app":"Untitled - Notepad","process":"notepad.exe",
                      "target":{"name":"Untitled - Notepad","raw_role":"Window",
                                "selector":"role:Window|name:Untitled - Notepad"}}"#;
        let p = parse_payload(raw);

        assert_eq!(p.process.as_deref(), Some("notepad.exe"));
        assert_eq!(
            p.scoped_selector().as_deref(),
            Some("process:notepad.exe|role:Window|name:Untitled - Notepad"),
            "the scoped form is what Locator::all() will accept"
        );
    }

    #[test]
    fn an_old_playbook_without_a_process_name_cannot_be_scoped() {
        // Recorded before the field existed. This must not error and must not
        // invent a prefix -- it simply cannot be counted, and replay falls back
        // to exactly the behaviour it had before this fix.
        let raw = r#"{"app":"Untitled - Notepad",
                      "target":{"name":"Untitled - Notepad","raw_role":"Window",
                                "selector":"role:Window|name:Untitled - Notepad"}}"#;
        let p = parse_payload(raw);

        assert_eq!(p.process, None);
        assert_eq!(p.scoped_selector(), None);
        // The unscoped selector is untouched, so `first()` behaves as before.
        assert_eq!(
            p.selector.as_deref(),
            Some("role:Window|name:Untitled - Notepad")
        );
    }

    #[test]
    fn a_blank_process_name_is_treated_as_absent() {
        // An empty string would build `process:|role:Window|...`, which is not
        // a scoping at all. Treated as missing rather than passed through.
        let raw = r#"{"app":"x","process":"   ",
                      "target":{"selector":"role:Window|name:x"}}"#;
        let p = parse_payload(raw);
        assert_eq!(p.scoped_selector(), None);
    }

    #[test]
    fn a_step_with_no_selector_cannot_be_scoped_either() {
        let raw = r#"{"app":"x","process":"notepad.exe","target":{}}"#;
        let p = parse_payload(raw);
        assert_eq!(p.selector, None);
        assert_eq!(p.scoped_selector(), None);
    }

    #[test]
    fn parses_a_click_payload_without_text() {
        let raw = r#"{"app":"explorer.exe","target":{"name":"Go","raw_role":"button",
                      "selector":"role:button|name:Go"}}"#;
        let p = parse_payload(raw);
        assert!(!p.redacted);
        assert!(p.text.is_none());
        assert_eq!(p.selector.as_deref(), Some("role:button|name:Go"));
    }

    #[test]
    fn malformed_payload_does_not_panic() {
        let p = parse_payload("{not json");
        assert!(p.selector.is_none());
        assert!(!p.redacted);
    }

    #[test]
    fn null_selector_is_none_not_the_string_null() {
        let raw = r#"{"target":{"name":null,"selector":null},"app":"x"}"#;
        let p = parse_payload(raw);
        assert!(p.selector.is_none());
        assert!(p.target_name.is_none());
    }

    #[test]
    fn failure_variants_are_failures_and_successes_are_not() {
        assert!(StepResult::HaltedRedacted.is_failure());
        assert!(StepResult::FailedNotFound.is_failure());
        assert!(StepResult::FailedAction.is_failure());
        assert!(!StepResult::Executed.is_failure());
        assert!(!StepResult::ExecutedViaCoordinateClick.is_failure());
        assert!(!StepResult::NoActionNeeded.is_failure());
    }
}
