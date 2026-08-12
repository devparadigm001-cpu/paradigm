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

/// Does `press_key` send this key verbatim, or inject navigation first?
///
/// `terminator-rs` 0.23.35 sends `{LEFT}` then `{END}` before any key naming
/// Enter or Return (`platforms/windows/element.rs:1163`) as a workaround for
/// inline autocomplete in a browser address bar. In a text field those are
/// harmless caret moves. **In a grid `{END}` is navigation** -- it jumps to the
/// last column of the data region, so the sequence becomes "move somewhere else,
/// then commit". Reproduced against live Google Sheets with the misplacement
/// confirmed in the CSV export: values typed into consecutive cells landed 25
/// columns away. See docs/known-issues/press-key-enter-injects-end-keystroke.md.
///
/// There is no way to opt out through the public API -- the condition is a
/// substring test, and both spellings the underlying crate accepts for that key
/// match it.
fn press_key_injects_navigation(key: &str) -> bool {
    let key = key.to_uppercase();
    key.contains("ENTER") || key.contains("RETURN")
}

/// The key that commits a grid cell edit.
///
/// Tab, not Enter, and the difference is load-bearing rather than stylistic --
/// see `press_key_injects_navigation`. Named as a constant so the choice is
/// pinned by `the_grid_commit_key_cannot_relocate_the_cursor` instead of living
/// only in a comment that an edit can quietly step past.
const GRID_COMMIT_KEY: &str = "{Tab}";

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
    /// Several elements match the selector equally well, so which one the
    /// recording meant cannot be determined.
    ///
    /// Deliberately distinct from `FailedWrongTarget`, which they are easy to
    /// conflate. `FailedWrongTarget` is a *proven mismatch*: the element found
    /// is demonstrably not the recorded one. This is *unproven identity*: the
    /// element found may well be the right one, but an equally good candidate
    /// exists and nothing in the recording says which was meant.
    ///
    /// They need different fixes, too. A wrong target means the selector no
    /// longer describes what it did at record time. An ambiguous one means the
    /// selector was never specific enough -- often two windows of the same app
    /// with the same title -- and the remedy is closing the duplicate or
    /// re-recording, not repairing drift.
    ///
    /// Collapsing the two would make a log read "resolved the wrong element"
    /// for an element that is very likely correct. For a bug whose whole nature
    /// is plausible-but-wrong reporting, that is the wrong trade.
    /// See docs/known-issues/replay-window-selector-ambiguity.md.
    FailedAmbiguous,
}

impl StepResult {
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            StepResult::HaltedRedacted
                | StepResult::FailedNotFound
                | StepResult::FailedAction
                | StepResult::FailedWrongTarget
                | StepResult::FailedAmbiguous
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
            StepResult::FailedAmbiguous => "FAILED (selector is ambiguous)",
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
    /// The role as captured, before compile mapped it to a control role.
    ///
    /// Needed to recognise a grid cell edit, whose target is a transient
    /// `ComboBox` editor rather than anything a selector can resolve.
    raw_role: Option<String>,
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
        raw_role: v["target"]["raw_role"].as_str().map(str::to_string),
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
/// this adds no new failure there. On its own it also cannot detect two windows
/// genuinely sharing a name: each one individually IS the recorded name, so this
/// passes them both, correctly. That case needs a candidate count, which
/// `resolve_recorded` below supplies -- using this predicate as its filter.
/// See docs/known-issues/replay-window-selector-ambiguity.md.
pub fn resolved_is_recorded_target(recorded: &str, resolved: &str) -> bool {
    if recorded == resolved {
        return true;
    }
    resolved
        .strip_prefix('*')
        .map(|undecorated| undecorated == recorded)
        .unwrap_or(false)
}

/// How long the ambiguity enumeration may spend.
///
/// It only ever runs *after* a successful `first()`, so at least one element
/// matches and the matcher returns on its first pass. This bound exists for the
/// pathological case where the tree changes in between and the enumeration finds
/// nothing, which would otherwise poll for the full default timeout.
const AMBIGUITY_TIMEOUT: Duration = Duration::from_secs(2);

/// Search depth for the ambiguity enumeration. `None` means the library's
/// default of 50.
///
/// This is the one tuning decision in the check, and it is derived rather than
/// picked. The requirement is *not* "enumerate every window on the desktop" --
/// it is "cover everything `first()` could have returned", because the whole
/// question is whether `first()` had more than one candidate to choose from.
///
/// Reading `terminator-rs` 0.23.35 `platforms/windows/engine.rs`, both calls
/// bottom out in the same `Selector::Role` matcher, from the same node:
///
/// * `find_element` (behind `first()`) with `root: None` resolves its root via
///   `get_root_element_with_retry()`, and `Desktop::root()` -- what the counter
///   passes to `within()` -- returns `get_root_element()`. The same desktop node.
/// * `find_element` picks its depth with `calculate_search_depth(role, name,
///   None, None)`, which returns **5** for a *named container* role
///   (`pane`/`window`/`application`) and **50** for anything else.
/// * `find_elements` (behind `all()`) calls `calculate_search_depth(role, name,
///   root, depth)`, and `should_use_shallow_search` returns false whenever a
///   root is supplied. So the counter's depth is exactly what we pass:
///   `depth.unwrap_or(50)`.
///
/// So 50 is `>=` the resolver's depth for *every* role, and the counter's
/// traversal is a superset of the resolver's. A shallower per-role depth --
/// the tempting optimisation, since depth 3 measured 6x faster for windows --
/// would mean auditing a search by examining *less* of the tree than the search
/// itself covered. At depth 3 the counter is below the resolver's 5 for exactly
/// the window selectors this bug is about: it could report "one candidate,
/// unambiguous" for a selector `first()` had two to choose from, which is the
/// original silent-wrong-window bug reintroduced by the check meant to prevent
/// it.
///
/// Matching the library's per-role rule instead of fixing 50 was considered and
/// rejected: it means replicating `should_use_shallow_search`'s undocumented
/// predicate, and if that drifts in a future version our depth silently drops
/// below the resolver's. The failure would be silent under-counting.
///
/// The asymmetry settles it. Under-counting is silent and writes to the wrong
/// window; over-counting is loud and refuses a step the user can see. Only one
/// of those is recoverable.
const AMBIGUITY_DEPTH: Option<usize> = None;

/// What the candidate enumeration was able to establish.
#[derive(Debug)]
enum Resolution {
    /// Exactly one candidate carries the recorded name. Act on *this* element,
    /// not on whatever `first()` happened to return -- see `resolve_recorded`.
    Unique(UIElement),
    /// Several do. Which one the recording meant is not knowable from here.
    Ambiguous(usize),
    /// No candidate carries the recorded name, or the enumeration failed. The
    /// caller falls back to checking `first()`'s own pick, which distinguishes
    /// "the target is genuinely gone" from "the enumeration missed it".
    /// Never treated as "unambiguous" -- see the swallowed-error pattern in the
    /// known-issues doc.
    Inconclusive(String),
}

/// Find the element that carries the recorded name, and establish whether it is
/// the only one.
///
/// Two filters, doing different jobs. The library's `contains_name` decides
/// which elements the selector reaches -- the same rule `first()` used, so the
/// candidate set is the one `first()` chose from. `resolved_is_recorded_target`
/// then keeps only those whose name genuinely *is* the recorded one, because
/// containment alone reports false ambiguity: a window titled
/// `"Draft X - Personal - Microsoft Edge"` contains the whole title of a window
/// titled `"X - Personal - Microsoft Edge"`, and counting it would refuse a
/// perfectly unambiguous replay. Measured; see the known-issues doc.
///
/// Returning the element, rather than just a count, fixes a real defect the
/// end-to-end test exposed. `first()` returns whichever containment match it
/// reaches first in traversal order, which is **not necessarily the recorded
/// one**: with the decoy above open, `first()` returned `"Draft X …"` and the
/// target check then refused the whole step -- a legitimate, unambiguous replay
/// rejected because the resolver guessed and the checker could only veto. Since
/// the enumeration is already paid for, the exact match is right there; using it
/// makes resolution constructive instead of merely defensive.
///
/// Runs only after a successful `first()`. Ordering matters: a miss costs the
/// full locate timeout, so enumerating *before* resolution would add that to
/// every genuinely-absent element.
async fn resolve_recorded(desktop: &Desktop, selector: &str, recorded: &str) -> Resolution {
    let candidates = match desktop
        .locator(selector)
        .within(desktop.root())
        .all(Some(AMBIGUITY_TIMEOUT), AMBIGUITY_DEPTH)
        .await
    {
        Ok(candidates) => candidates,
        Err(e) => {
            return Resolution::Inconclusive(format!("candidates could not be enumerated: {e}"))
        }
    };

    let mut named: Vec<UIElement> = candidates
        .into_iter()
        .filter(|el| resolved_is_recorded_target(recorded, &el.name().unwrap_or_default()))
        .collect();

    match named.len() {
        0 => Resolution::Inconclusive(
            "no enumerated candidate carries the recorded name".to_string(),
        ),
        1 => Resolution::Unique(named.remove(0)),
        n => Resolution::Ambiguous(n),
    }
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

    // 3b. Grid cells have nothing to resolve, so they take their own path.
    //
    // Every other step shape is "resolve a selector, act on what it finds". A
    // spreadsheet cell has no element until typing creates one, so the thing
    // that receives the typing cannot also be the thing that is located first.
    // Entry happens through the Name Box instead -- see `grid_type`.
    if step.action_type == "type" {
        if let (Some(role), Some(cell)) = (
            payload.raw_role.as_deref(),
            payload.target_name.as_deref(),
        ) {
            if crate::capture::grid::is_cell_editor(role, cell) {
                return grid_type(desktop, payload, cell, &mk).await;
            }
        }
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
    let mut element = element;
    let mut unverified_note = String::new();
    if let Some(recorded) = payload.target_name.as_deref() {
        match resolve_recorded(desktop, selector, recorded).await {
            // Act on the element that IS the recorded one, which may not be the
            // one `first()` returned.
            Resolution::Unique(exact) => element = exact,
            Resolution::Ambiguous(n) => {
                return mk(
                    StepResult::FailedAmbiguous,
                    format!(
                        "selector {selector:?} matches {n} elements that all carry the recorded \
                         name {recorded:?}, so which one the recording meant cannot be \
                         determined.\nRefusing to act: `first()` would pick one silently, and a \
                         wrong pick here is invisible in the run report."
                    ),
                );
            }
            // Nothing enumerated carries the recorded name. Either the target
            // is genuinely gone, or the enumeration did not see it. `first()`'s
            // own pick tells the two apart.
            Resolution::Inconclusive(reason) => {
                let resolved = element.name().unwrap_or_default();
                if !resolved_is_recorded_target(recorded, &resolved) {
                    return mk(
                        StepResult::FailedWrongTarget,
                        format!(
                            "selector {selector:?} resolved to the wrong element -- names match \
                             by containment, not equality.\n  recorded: {recorded:?}\n  resolved: \
                             {resolved:?}\nRefusing to act: this is how a replay writes to the \
                             wrong place while reporting success."
                        ),
                    );
                }
                unverified_note = format!("\n  note: ambiguity not verified -- {reason}");
            }
        }
    }

    let mut outcome = match step.action_type.as_str() {
        "click" => click(desktop, &element, selector, &mk),
        "type" => type_text(&element, payload, &mk),
        other => mk(
            StepResult::FailedAction,
            format!("unsupported action_type {other:?}"),
        ),
    };
    // Carried into the step detail rather than dropped: "could not check" must
    // not be indistinguishable from "checked, and it was fine".
    outcome.detail.push_str(&unverified_note);
    outcome
}

/// Reproduce a spreadsheet cell edit.
///
/// ## Why this cannot use a selector
///
/// There is no cell element. Measured against a live Google Sheets document, the
/// grid contributes nothing to the accessibility tree; the only per-cell element
/// is a `ComboBox` editor that typing *creates* and committing destroys. So the
/// element that receives the text cannot be located beforehand -- there is
/// nothing there until after the action has begun.
///
/// ## Entry through the Name Box
///
/// The Name Box -- the little reference field left of the formula bar -- is a
/// real, persistent `Edit`, and its text tracks the cursor within 0-1 ms. Giving
/// it a reference and pressing Enter moves the cursor, which is how a keyboard
/// user reaches a cell. Measured 3/3 against the document's CSV export.
///
/// This keeps replay **element-based**. Coordinate clicking was the obvious
/// alternative and is far more fragile: scroll position, zoom, frozen panes and
/// window size all move a cell's pixels while its reference stays the same.
///
/// Two details that are not incidental:
///
/// * `set_value` is used, not `type_text`. `type_text` APPENDS -- measured, the
///   box went `"A1"` -> `"A1B2"` -> `"A1B2D5"`, never a valid reference, and
///   Enter did nothing each time.
/// * The commit is `{Tab}`, not `{Enter}`. `press_key` injects `{LEFT}{END}`
///   before any Enter, and in a grid `{END}` jumps to the last column of the
///   data region. See press-key-enter-injects-end-keystroke.md. Inside the Name
///   Box those same keys are harmless caret moves, which is why Enter is fine
///   *there* and not on the grid.
async fn grid_type(
    desktop: &Desktop,
    payload: &StepPayload,
    cell: &str,
    mk: &impl Fn(StepResult, String) -> StepOutcome,
) -> StepOutcome {
    let Some(text) = payload.text.as_deref().filter(|t| !t.is_empty()) else {
        return mk(
            StepResult::FailedAction,
            format!("grid step for cell {cell:?} has no text to type"),
        );
    };

    // The Name Box input is the `Edit` child of the group named "Name box".
    let name_box = match desktop
        .locator("name:Name box")
        .first(Some(ELEMENT_LOCATE_TIMEOUT))
        .await
    {
        Ok(group) => group
            .children()
            .ok()
            .and_then(|c| c.into_iter().find(|e| e.role() == "Edit")),
        Err(e) => {
            return mk(
                StepResult::FailedNotFound,
                format!(
                    "no Name Box found, so cell {cell:?} cannot be reached: {e}\n\
                     A grid edit is replayed by navigating through the Name Box; without \
                     it there is no way into a cell."
                ),
            )
        }
    };
    let Some(name_box) = name_box else {
        return mk(
            StepResult::FailedNotFound,
            format!("the Name Box group has no editable child, so cell {cell:?} cannot be reached"),
        );
    };

    if let Err(e) = name_box.set_value(cell) {
        return mk(
            StepResult::FailedAction,
            format!("could not put {cell:?} into the Name Box: {e}"),
        );
    }
    if let Err(e) = name_box.press_key("{Enter}") {
        return mk(
            StepResult::FailedAction,
            format!("could not submit the Name Box for {cell:?}: {e}"),
        );
    }
    std::thread::sleep(Duration::from_millis(1200));

    // Confirm the cursor actually moved BEFORE typing. Nothing has been written
    // yet, so refusing here costs nothing; typing into whatever happens to be
    // selected is how a replay writes to the wrong cell while reporting success.
    let landed = name_box.text(0).unwrap_or_default();
    if landed.trim() != cell {
        return mk(
            StepResult::FailedWrongTarget,
            format!(
                "the Name Box reads {landed:?} after asking for {cell:?}, so the cursor is \
                 not demonstrably on the recorded cell.\nRefusing to type: this is how a \
                 replay fills the wrong cell while reporting success."
            ),
        );
    }

    let target = match desktop.focused_element() {
        Ok(el) => el,
        Err(e) => {
            return mk(
                StepResult::FailedNotFound,
                format!("cursor is on {cell:?} but nothing holds focus to type into: {e}"),
            )
        }
    };
    if let Err(e) = target.type_text(text, false) {
        return mk(
            StepResult::FailedAction,
            format!("typing into cell {cell:?} failed: {e}"),
        );
    }
    std::thread::sleep(Duration::from_millis(400));

    // Commit against whatever holds focus NOW, not against `target`.
    //
    // Typing creates the editor overlay and focus moves to it, so `target` is
    // the pre-edit element by this point. Sending the commit there made
    // `press_key` focus it first, which abandoned the edit instead of committing
    // it. The symptom was precise and worth recording: every cell landed EXCEPT
    // the last one, because each pending edit was being committed by the *next*
    // step's Name Box navigation rather than by its own Tab -- and the final
    // step has no next step. Measured 2/3, twice, before this line changed.
    let committer = desktop.focused_element().unwrap_or(target);
    debug_assert!(
        !press_key_injects_navigation(GRID_COMMIT_KEY),
        "the grid commit key must not be one press_key prefixes with {{LEFT}}{{END}}"
    );
    if let Err(e) = committer.press_key(GRID_COMMIT_KEY) {
        return mk(
            StepResult::FailedAction,
            format!("typed into cell {cell:?} but committing it failed: {e}"),
        );
    }
    std::thread::sleep(Duration::from_millis(600));

    mk(
        StepResult::Executed,
        format!("typed {} character(s) into cell {cell:?} via the Name Box", text.chars().count()),
    )
}

async fn navigate(
    desktop: &Desktop,
    payload: &StepPayload,
    mk: &impl Fn(StepResult, String) -> StepOutcome,
) -> StepOutcome {
    if let Some(selector) = payload.selector.as_deref() {
        // The ambiguity check runs below, after resolution -- see `ambiguity_of`.
        //
        // Two earlier counting mechanisms were built and removed before this
        // one, both for the same reason: they over-counted, and would have
        // refused legitimate replays. `Locator::all()` on a `process:`-scoped
        // selector returns every top-level window of that process and ignores
        // role and name entirely. Root-scoped counting respects both, but its
        // raw count still inflates on `contains_name`, which is why the count
        // here is filtered through `resolved_is_recorded_target`.
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
                let mut window = window;
                let mut unverified_note = String::new();
                if let Some(recorded) = payload.target_name.as_deref() {
                    // This is the site of the original defect: a recording of
                    // "Untitled - Notepad" replayed against two blank Notepad
                    // windows, and the run reported 12/12 succeeded while
                    // typing into the wrong one.
                    match resolve_recorded(desktop, selector, recorded).await {
                        Resolution::Unique(exact) => window = exact,
                        Resolution::Ambiguous(n) => {
                            return mk(
                                StepResult::FailedAmbiguous,
                                format!(
                                    "window selector {selector:?} matches {n} windows that all \
                                     carry the recorded name {recorded:?}, so which one the \
                                     recording meant cannot be determined.\nRefusing to activate \
                                     any of them: activating the wrong one sends every later step \
                                     to the wrong window while the run still reports success."
                                ),
                            );
                        }
                        Resolution::Inconclusive(reason) => {
                            let resolved = window.name().unwrap_or_default();
                            if !resolved_is_recorded_target(recorded, &resolved) {
                                return mk(
                                    StepResult::FailedWrongTarget,
                                    format!(
                                        "window selector {selector:?} resolved to the wrong \
                                         window -- names match by containment, not equality.\n  \
                                         recorded: {recorded:?}\n  resolved: {resolved:?}\n\
                                         Refusing to activate it."
                                    ),
                                );
                            }
                            unverified_note =
                                format!("\n  note: ambiguity not verified -- {reason}");
                        }
                    }
                }
                return match window.activate_window() {
                    Ok(()) => mk(
                        StepResult::Executed,
                        format!("activated window via {selector:?}{unverified_note}"),
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
    fn identical_names_on_different_windows_are_not_detectable_by_the_name_check_alone() {
        // Pins the division of labour between the two checks at each resolution
        // site. When two windows genuinely share a name, the resolved name
        // equals the recorded one, so the name check passes them both -- as it
        // should, since each one individually IS the recorded name. Detecting
        // that there are two is `ambiguity_of`'s job, not this function's.
        //
        // This used to record the absence of any such counter. It now records
        // the boundary between them, so that widening this predicate is never
        // mistaken for a way to catch ambiguity.
        assert!(resolved_is_recorded_target(
            "Untitled - Notepad",
            "Untitled - Notepad"
        ));
    }

    #[test]
    fn the_ambiguity_filter_counts_exact_names_not_containment_matches() {
        // The counting rule itself, isolated from the enumeration. These are the
        // real titles measured by `text_capture_probe -- decoycount`.
        let recorded = "DecoyCount Invoice - Personal - Microsoft Edge";
        let enumerated = [
            "Draft DecoyCount Invoice - Personal - Microsoft Edge", // containment decoy
            "DecoyCount Invoice - Personal - Microsoft Edge",       // the real target
        ];
        let counted = enumerated
            .iter()
            .filter(|n| resolved_is_recorded_target(recorded, n))
            .count();

        // Two candidates reach the selector, but only one IS the recorded
        // window. Counting raw matches here would refuse a replay that is not
        // ambiguous at all -- the failure that closed Routes 1 and 2.
        assert_eq!(counted, 1);

        // Two genuinely identical windows must still count as two.
        let twins = [
            "DecoyCount Twin - Personal - Microsoft Edge",
            "DecoyCount Twin - Personal - Microsoft Edge",
        ];
        let recorded_twin = "DecoyCount Twin - Personal - Microsoft Edge";
        assert_eq!(
            twins
                .iter()
                .filter(|n| resolved_is_recorded_target(recorded_twin, n))
                .count(),
            2
        );
    }

    #[test]
    fn the_grid_commit_key_cannot_relocate_the_cursor() {
        // The guard for a latent defect in `terminator-rs`: `press_key` prefixes
        // any Enter with `{LEFT}{END}`, and `{END}` in a grid jumps to the last
        // column of the data region. Changing `GRID_COMMIT_KEY` to Enter would
        // silently write to the wrong cell -- measured, values landed 25 columns
        // away and the CSV export proved it.
        //
        // This test exists because a comment cannot fail. If someone swaps the
        // commit key, this goes red with a name that explains why.
        assert!(!press_key_injects_navigation(GRID_COMMIT_KEY));

        // The rule itself, so the guard cannot rot into a tautology.
        assert!(press_key_injects_navigation("{Enter}"));
        assert!(press_key_injects_navigation("{ENTER}"));
        assert!(press_key_injects_navigation("{Return}"));
        assert!(!press_key_injects_navigation("{Tab}"));
        assert!(!press_key_injects_navigation("{Escape}"));
    }

    #[test]
    fn the_name_box_enter_is_deliberate_and_safe() {
        // `grid_type` DOES send Enter to the Name Box, and that is not an
        // oversight. The injected `{LEFT}{END}` are caret moves inside a text
        // field there, not grid navigation -- which is exactly why the same key
        // is correct in one place and wrong in the other, a distinction easy to
        // lose when someone later "makes the two consistent".
        assert!(press_key_injects_navigation("{Enter}"));
        assert_ne!(GRID_COMMIT_KEY, "{Enter}");
    }

    #[test]
    fn a_grid_cell_step_is_recognised_from_its_stored_payload() {
        // The exact shape `GridCellWatcher` produces and `compile` stores.
        let raw = r#"{"app":"msedge.exe","text":"apple",
                      "target":{"name":"B2","raw_role":"ComboBox",
                                "selector":"role:ComboBox|name:B2"}}"#;
        let p = parse_payload(raw);
        assert_eq!(p.raw_role.as_deref(), Some("ComboBox"));
        assert!(crate::capture::grid::is_cell_editor(
            p.raw_role.as_deref().unwrap(),
            p.target_name.as_deref().unwrap()
        ));
    }

    #[test]
    fn ordinary_steps_do_not_take_the_grid_path() {
        // The grid path must claim only real cell edits. A web text field and a
        // dropdown both have to keep going through selector resolution, or
        // replay would try to reach them through a Name Box that is not there.
        for raw in [
            r#"{"target":{"name":"FieldA","raw_role":"Edit"}}"#,
            r#"{"target":{"name":"Untitled - Notepad","raw_role":"Window"}}"#,
            r#"{"target":{"name":"Menus","raw_role":"ComboBox"}}"#,
            r#"{"target":{"name":"Text editor","raw_role":"Document"}}"#,
        ] {
            let p = parse_payload(raw);
            let claimed = match (p.raw_role.as_deref(), p.target_name.as_deref()) {
                (Some(r), Some(n)) => crate::capture::grid::is_cell_editor(r, n),
                _ => false,
            };
            assert!(!claimed, "grid path wrongly claimed {raw}");
        }
    }

    #[test]
    fn a_playbook_recorded_before_raw_role_existed_does_not_take_the_grid_path() {
        // Strictly additive: an old payload has no raw_role, so the grid check
        // cannot fire and the step resolves exactly as it always did.
        let raw = r#"{"target":{"name":"B2","selector":"role:ComboBox|name:B2"}}"#;
        let p = parse_payload(raw);
        assert_eq!(p.raw_role, None);
    }

    #[test]
    fn ambiguity_is_a_distinct_failure_from_a_wrong_target() {
        // Both are failures, and they must not collapse into one another: the
        // remedies differ, and a log saying "resolved the wrong element" for an
        // element that is probably correct is the kind of plausible-but-wrong
        // report this whole investigation is about.
        assert!(StepResult::FailedAmbiguous.is_failure());
        assert!(StepResult::FailedWrongTarget.is_failure());
        assert_ne!(
            StepResult::FailedAmbiguous.label(),
            StepResult::FailedWrongTarget.label()
        );
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
