//! Tauri commands: the frontend's entry point to the Phase 1 pipeline.
//!
//! Wiring only. Every command here is a thin wrapper over functions already
//! built and tested in Steps 3-6. No business logic lives in this file, and
//! anything that looks like a decision should be pushed down into the module
//! that owns it.
//!
//! ## One deviation from the brief, and why
//!
//! The brief specifies `compile_and_store_playbook(actions, name_hint)`, taking
//! the captured actions back from the frontend. That is not implemented, and
//! deliberately so: `CapturedAction` carries a private gate token specifically
//! so it cannot be constructed outside `capture::stream` without passing the
//! exclusion gate (Step 3). Making it `Deserialize` would hand any frontend --
//! or anything that can reach the IPC boundary -- the ability to fabricate
//! actions that never passed that gate, which is the exact invariant Step 3
//! exists to enforce.
//!
//! Instead `stop_record_session` returns a read-only *view* of what was
//! captured (for the user to review, which is the brief's stated purpose) and
//! keeps the real actions in app state. `compile_and_store_playbook` then takes
//! only `name_hint` and, optionally, `step_indices`. The round trip through the
//! frontend is display-only.
//!
//! `step_indices` lets the user drop and reorder steps during that review
//! without weakening the above: only plain numbers cross the IPC boundary, and
//! each one indexes the actions already sitting in app state. Every action a
//! compiled playbook can contain therefore still came out of `admit`, so the
//! frontend gains no way to introduce one that never passed the exclusion gate.

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::capture::{ActionKind, CaptureSession, CapturedAction, ExclusionList};
use crate::compile::{compile, store, validate, ReversibilityPolicy};
use crate::labeling::{self, calibration, clean, CalibrationSample, RedactionPolicy};
use crate::replay::{self, journal};
use crate::AppState;

// ---------------------------------------------------------------- views ----

/// A captured action as shown to the user. Read-only: it cannot be turned back
/// into a `CapturedAction`.
#[derive(Debug, Serialize)]
pub struct CapturedActionView {
    pub step_order: usize,
    pub action_type: String,
    pub source_app: String,
    pub element_role: Option<String>,
    pub element_name: Option<String>,
    /// Withheld when the redaction policy considers the field sensitive, so a
    /// secret never crosses the IPC boundary even for display.
    pub payload_preview: Option<String>,
    pub would_redact: bool,
    pub timestamp_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct CaptureSummary {
    pub session_name: String,
    pub action_count: usize,
    pub excluded_count: usize,
    pub unmapped_events: usize,
    pub actions: Vec<CapturedActionView>,
}

#[derive(Debug, Serialize)]
pub struct StoredPlaybookInfo {
    pub playbook_id: String,
    pub label: String,
    pub step_count: usize,
    pub irreversible_count: usize,
    pub redacted_count: usize,
    /// True when the label came from the local model rather than the caller.
    pub label_generated: bool,
}

#[derive(Debug, Serialize)]
pub struct PlaybookSummaryView {
    pub id: String,
    pub name: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub step_count: i64,
    /// How many of those steps are irreversible, so the list can warn before
    /// the user replays one. Purely additive: existing fields are untouched.
    pub irreversible_count: i64,
}

#[derive(Debug, Serialize)]
pub struct StepOutcomeView {
    pub step_order: i64,
    pub action_type: String,
    pub selector: Option<String>,
    pub result: String,
    pub detail: String,
    pub is_failure: bool,
}

#[derive(Debug, Serialize)]
pub struct ReplayReportView {
    pub run_id: String,
    pub playbook_id: String,
    pub playbook_name: String,
    pub status: String,
    pub steps_total: usize,
    pub steps_attempted: usize,
    pub outcomes: Vec<StepOutcomeView>,
}

#[derive(Debug, Serialize)]
pub struct StepLogView {
    pub step_order: i64,
    pub action_type: String,
    pub event_type: String,
    pub is_sensitive: bool,
    pub data_payload: Option<String>,
    pub model_source: Option<String>,
    pub cost: f64,
    pub timestamp: String,
}

#[derive(Debug, Serialize)]
pub struct RunHistoryEntry {
    pub run_id: String,
    pub feature: String,
    pub status: String,
    pub billable: bool,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub steps: Vec<StepLogView>,
}

fn view_of(actions: &[CapturedAction], policy: &RedactionPolicy) -> Vec<CapturedActionView> {
    actions
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let would_redact = policy.evaluate(a).is_some();
            CapturedActionView {
                step_order: i + 1,
                action_type: a.kind.as_str().to_string(),
                source_app: a.source_app.clone(),
                element_role: a.element_role.clone(),
                element_name: a.element_name.clone(),
                payload_preview: if would_redact {
                    None
                } else {
                    a.payload.as_ref().map(|p| truncate(p, 80))
                },
                would_redact,
                timestamp_ms: a.timestamp_ms,
            }
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "..."
}

fn model_path(state: &AppState) -> PathBuf {
    state.model_path.clone()
}

/// The one message for "there is nothing to compile", used by both the
/// nothing-was-captured and the nothing-was-selected paths so a caller can
/// match on it without caring which produced it.
const NO_ACTIONS: &str = "the captured session contains no actions to compile";

/// Resolve the caller's step selection into positions in `pending_actions`.
///
/// `None` means every captured action in its original order -- the behaviour
/// before `step_indices` existed, and the one existing callers rely on.
/// `Some(indices)` is the subset and/or reordering the user chose while
/// reviewing the capture, given as positions in the desired final order.
///
/// Duplicates are deliberately *not* rejected: repeating a step is a coherent
/// thing to ask for, it produces a playbook that still validates (each step is
/// compiled with its own id and a dense step_order), and refusing it would be a
/// restriction nothing asked for. Out-of-range indices are a different matter --
/// there is no sensible action to compile for one, so it is an error rather
/// than something to silently skip.
fn resolve_selection(total: usize, requested: Option<&[usize]>) -> Result<Vec<usize>, String> {
    // Checked here as well as by the caller so this function is correct on its
    // own terms: every path below assumes there is at least one action, and
    // `total - 1` in the range message would underflow without it.
    if total == 0 {
        return Err(NO_ACTIONS.to_string());
    }

    let Some(indices) = requested else {
        return Ok((0..total).collect());
    };

    // Selecting nothing is not a zero-step playbook; it is the same "nothing to
    // compile" condition as an empty capture, and takes the same error.
    if indices.is_empty() {
        return Err(NO_ACTIONS.to_string());
    }

    if let Some(&bad) = indices.iter().find(|&&i| i >= total) {
        return Err(format!(
            "step index {bad} is out of range: the captured session has {total} action(s), \
             so the valid indices are 0..={}",
            total - 1
        ));
    }

    Ok(indices.to_vec())
}

// ------------------------------------------------------------- commands ----

/// Begin a Record Mode capture session.
#[tauri::command]
pub async fn start_record_session(state: State<'_, AppState>) -> Result<String, String> {
    {
        let slot = state.session.lock().map_err(|e| e.to_string())?;
        if slot.is_some() {
            return Err(
                "a recording session is already active; stop it before starting another"
                    .to_string(),
            );
        }
    }

    let name = format!("record-{}", uuid::Uuid::new_v4());
    let session = CaptureSession::start_session(name.clone(), ExclusionList::placeholder())
        .await
        .map_err(|e| format!("could not start recording: {e}"))?;

    let mut slot = state.session.lock().map_err(|e| e.to_string())?;
    // Re-check: another caller could have won the race while we were starting.
    if slot.is_some() {
        // Drop the session we just built rather than leaking its hooks.
        drop(session);
        return Err("a recording session started concurrently; this one was discarded".to_string());
    }
    *slot = Some(session);
    Ok(name)
}

/// Stop the active session and return what it captured, for review.
///
/// Does not compile or store: the user gets to see the capture before
/// committing to save it. The real actions stay in app state for
/// `compile_and_store_playbook`.
#[tauri::command]
pub async fn stop_record_session(state: State<'_, AppState>) -> Result<CaptureSummary, String> {
    let session = {
        let mut slot = state.session.lock().map_err(|e| e.to_string())?;
        slot.take()
            .ok_or_else(|| "no recording session is active".to_string())?
    };

    let report = session
        .stop_session()
        .await
        .map_err(|e| format!("could not stop recording: {e}"))?;

    let policy = RedactionPolicy::placeholder();
    let summary = CaptureSummary {
        session_name: report.session_name.clone(),
        action_count: report.actions.len(),
        excluded_count: report.exclusions.len(),
        unmapped_events: report.unmapped_events,
        actions: view_of(&report.actions, &policy),
    };

    let mut pending = state.pending_actions.lock().map_err(|e| e.to_string())?;
    *pending = Some(report.actions);

    Ok(summary)
}

/// Clean, label, compile, validate and store the last captured session.
///
/// `step_indices` is the user's edit of the capture, made while reviewing it:
/// positions in the captured action list, in the order they should replay.
/// Omit it to compile everything as captured. See the module docs for why this
/// takes indices rather than actions.
#[tauri::command]
pub async fn compile_and_store_playbook(
    state: State<'_, AppState>,
    name_hint: Option<String>,
    step_indices: Option<Vec<usize>>,
) -> Result<StoredPlaybookInfo, String> {
    let captured = {
        let pending = state.pending_actions.lock().map_err(|e| e.to_string())?;
        pending
            .clone()
            .ok_or_else(|| "no captured session to compile; record one first".to_string())?
    };
    if captured.is_empty() {
        return Err(NO_ACTIONS.to_string());
    }

    // Resolved before any work is done, so a bad selection costs nothing and --
    // more importantly -- leaves the pending capture intact for the caller to
    // retry with a corrected one.
    let selection = resolve_selection(captured.len(), step_indices.as_deref())?;
    let actions: Vec<CapturedAction> = selection.iter().map(|&i| captured[i].clone()).collect();

    let redaction = RedactionPolicy::placeholder();

    // Label: caller's hint wins, otherwise the local model names it.
    //
    // The third element is a calibration sample, and it is `None` whenever the
    // caller supplied a name. That is not an omission: no model ran, so there is
    // no confidence score to calibrate. Samples therefore only ever accumulate
    // from sessions the user chooses not to name.
    let (label, label_generated, calibration_sample) = match name_hint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(hint) => (hint.to_string(), false, None),
        None => {
            let engine = labeling::shared(&model_path(&state))
                .map_err(|e| format!("labeling model unavailable: {e}"))?;
            let cleaned = clean(&actions, &redaction);
            let outcome = engine
                .label(&cleaned.description)
                .map_err(|e| format!("could not label the session: {e}"))?;
            let sample = CalibrationSample::from_outcome(&outcome);
            (outcome.label, true, Some(sample))
        }
    };

    let playbook = compile(&actions, &label, &ReversibilityPolicy::placeholder(), &redaction);

    // Report validation errors rather than swallowing them.
    let errors = validate(&playbook);
    if !errors.is_empty() {
        return Err(format!(
            "playbook rejected and NOT stored:\n  - {}",
            errors
                .iter()
                .map(|e| e.describe())
                .collect::<Vec<_>>()
                .join("\n  - ")
        ));
    }

    let info = StoredPlaybookInfo {
        playbook_id: playbook.id.clone(),
        label: playbook.name.clone(),
        step_count: playbook.steps.len(),
        irreversible_count: playbook.irreversible_count(),
        redacted_count: playbook.redacted_count(),
        label_generated,
    };

    {
        let mut conn = state.db.lock().await;

        // Calibration is bookkeeping about the MODEL, not about this playbook,
        // and it is deliberately not part of the command's contract:
        //
        //  * recorded BEFORE the store, so a store failure does not discard an
        //    observation that is already valid -- the model ran either way;
        //  * a failure here is logged and dropped, never propagated. Refusing to
        //    save a user's recording because a statistics row could not be
        //    written would trade something they care about for something they
        //    have never heard of.
        //
        // This is the call the whole subsystem was missing: `record` previously
        // had exactly one caller, in a probe writing to a temp directory, so
        // `confidence_calibration` could never accumulate anything from real
        // use. See docs/known-issues/confidence-calibration-never-recorded.md.
        if let Some(sample) = &calibration_sample {
            if let Err(e) = calibration::record(&mut conn, sample) {
                eprintln!("[paradigm] calibration sample not recorded: {e}");
            }
        }

        store::store(&mut conn, &playbook).map_err(|e| e.to_string())?;
    }

    // Consume the pending capture so it cannot be stored twice.
    let mut pending = state.pending_actions.lock().map_err(|e| e.to_string())?;
    *pending = None;

    Ok(info)
}

/// Every stored playbook, for display.
#[tauri::command]
pub async fn list_playbooks(
    state: State<'_, AppState>,
) -> Result<Vec<PlaybookSummaryView>, String> {
    let conn = state.db.lock().await;
    let rows = store::list(&conn).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|p| PlaybookSummaryView {
            id: p.id,
            name: p.name,
            source: p.source,
            created_at: p.created_at,
            updated_at: p.updated_at,
            step_count: p.step_count,
            irreversible_count: p.irreversible_count,
        })
        .collect())
}

/// Delete a stored playbook and its steps.
///
/// Irreversible, and there is no undo: the frontend should confirm before
/// calling this. Past runs are deliberately kept — see `store::delete`.
#[tauri::command]
pub async fn delete_playbook(
    state: State<'_, AppState>,
    playbook_id: String,
) -> Result<(), String> {
    let conn = state.db.lock().await;
    store::delete(&conn, &playbook_id).map_err(|e| e.to_string())
}

/// Replay a stored playbook. Performs real input on the user's desktop.
#[tauri::command]
pub async fn replay_playbook(
    state: State<'_, AppState>,
    playbook_id: String,
) -> Result<ReplayReportView, String> {
    let desktop = terminator::Desktop::new_default()
        .map_err(|e| format!("accessibility engine unavailable: {e}"))?;

    // The lock is held across the replay because a single connection is
    // serialised anyway; Phase 1 never replays two playbooks at once.
    let mut conn = state.db.lock().await;
    let report = replay::replay(&mut conn, &desktop, &playbook_id)
        .await
        .map_err(|e| format!("replay failed to run: {e}"))?;

    Ok(ReplayReportView {
        run_id: report.run_id.clone(),
        playbook_id: report.playbook_id.clone(),
        playbook_name: report.playbook_name.clone(),
        status: report.status.clone(),
        steps_total: report.steps_total,
        steps_attempted: report.steps_attempted(),
        outcomes: report
            .outcomes
            .iter()
            .map(|o| StepOutcomeView {
                step_order: o.step_order,
                action_type: o.action_type.clone(),
                selector: o.selector.clone(),
                result: o.result.label().to_string(),
                detail: o.detail.clone(),
                is_failure: o.result.is_failure(),
            })
            .collect(),
    })
}

/// Past runs for a playbook, with their step logs.
#[tauri::command]
pub async fn get_run_history(
    state: State<'_, AppState>,
    playbook_id: String,
) -> Result<Vec<RunHistoryEntry>, String> {
    let conn = state.db.lock().await;
    let runs = journal::load_runs_for_playbook(&conn, &playbook_id).map_err(|e| e.to_string())?;

    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        let steps = journal::load_step_logs(&conn, &run.id).map_err(|e| e.to_string())?;
        out.push(RunHistoryEntry {
            run_id: run.id,
            feature: run.feature,
            status: run.status,
            billable: run.billable,
            started_at: run.started_at,
            completed_at: run.completed_at,
            steps: steps
                .into_iter()
                .map(|s| StepLogView {
                    step_order: s.step_order,
                    action_type: s.action_type,
                    event_type: s.event_type,
                    is_sensitive: s.is_sensitive,
                    data_payload: s.data_payload,
                    model_source: s.model_source,
                    cost: s.cost,
                    timestamp: s.timestamp,
                })
                .collect(),
        });
    }
    Ok(out)
}

/// Whether an action kind carries a payload at all. Used by the view layer.
#[allow(dead_code)]
fn carries_payload(kind: ActionKind) -> bool {
    kind == ActionKind::Type
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_selection_means_every_action_in_captured_order() {
        assert_eq!(resolve_selection(4, None).unwrap(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_selection_is_taken_in_the_order_given() {
        // The point of the parameter: this is a reorder, not a filter that
        // happens to preserve capture order.
        assert_eq!(
            resolve_selection(4, Some(&[3, 1, 0, 2])).unwrap(),
            vec![3, 1, 0, 2]
        );
    }

    #[test]
    fn a_selection_can_drop_actions() {
        assert_eq!(resolve_selection(4, Some(&[2, 0])).unwrap(), vec![2, 0]);
    }

    #[test]
    fn an_out_of_range_index_is_refused() {
        let err = resolve_selection(3, Some(&[0, 3])).unwrap_err();
        assert!(err.contains("out of range"), "unhelpful error: {err}");
        assert!(err.contains("0..=2"), "error should name the valid range: {err}");
    }

    #[test]
    fn an_empty_selection_is_the_nothing_to_compile_case() {
        // Not a zero-step playbook, which would validate as EmptyPlaybook only
        // after doing all the labeling and compiling work first.
        assert_eq!(resolve_selection(3, Some(&[])).unwrap_err(), NO_ACTIONS);
    }

    #[test]
    fn no_captured_actions_is_refused_whatever_was_selected() {
        assert_eq!(resolve_selection(0, None).unwrap_err(), NO_ACTIONS);
        assert_eq!(resolve_selection(0, Some(&[0])).unwrap_err(), NO_ACTIONS);
    }

    #[test]
    fn a_repeated_index_is_allowed() {
        // Documented behaviour, not an oversight -- see `resolve_selection`.
        assert_eq!(resolve_selection(2, Some(&[1, 1, 0])).unwrap(), vec![1, 1, 0]);
    }
}
