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
//! only `name_hint`. The round trip through the frontend is display-only.

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::capture::{ActionKind, CaptureSession, CapturedAction, ExclusionList};
use crate::compile::{compile, store, validate, ReversibilityPolicy};
use crate::labeling::{self, clean, RedactionPolicy};
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
#[tauri::command]
pub async fn compile_and_store_playbook(
    state: State<'_, AppState>,
    name_hint: Option<String>,
) -> Result<StoredPlaybookInfo, String> {
    let actions = {
        let pending = state.pending_actions.lock().map_err(|e| e.to_string())?;
        pending
            .clone()
            .ok_or_else(|| "no captured session to compile; record one first".to_string())?
    };
    if actions.is_empty() {
        return Err("the captured session contains no actions to compile".to_string());
    }

    let redaction = RedactionPolicy::placeholder();

    // Label: caller's hint wins, otherwise the local model names it.
    let (label, label_generated) = match name_hint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(hint) => (hint.to_string(), false),
        None => {
            let engine = labeling::shared(&model_path(&state))
                .map_err(|e| format!("labeling model unavailable: {e}"))?;
            let cleaned = clean(&actions, &redaction);
            let outcome = engine
                .label(&cleaned.description)
                .map_err(|e| format!("could not label the session: {e}"))?;
            (outcome.label, true)
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
        })
        .collect())
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
