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
use crate::compile::{compile, store, validate, CompiledTemplate, ReversibilityPolicy};
use crate::detect::{self, Detection};
use crate::labeling::{self, calibration, clean, CalibrationSample, RedactionPolicy};
use crate::replay::{self, journal};
use crate::run;
use crate::source::SourceReader;
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
    /// Pastes seen. Non-zero means the recording may be missing data movement
    /// that no action records -- see `CaptureReport::pastes_observed`.
    pub pastes_observed: usize,
    pub actions: Vec<CapturedActionView>,
    /// The repeating pattern detection found, if it found one. §4.12's review:
    /// the user sees the mapping in full before answering.
    pub template: Option<TemplateProposal>,
    /// Why no pattern was offered, when none was. `None` when one was.
    ///
    /// Every negative case of `Detection` is distinct for a reason -- §4.1 and
    /// §4.12 prescribe different responses -- so the reason is surfaced rather
    /// than collapsed into the absence of a proposal.
    pub no_template_reason: Option<String>,
}

/// One field of a proposed mapping, in the user's terms.
#[derive(Debug, Serialize)]
pub struct MappedField {
    pub from: String,
    pub to: String,
}

/// A detected pattern, offered for confirmation. Structure only -- no values.
#[derive(Debug, Serialize)]
pub struct TemplateProposal {
    pub source: String,
    pub destination: String,
    pub fields: Vec<MappedField>,
    pub source_step: i64,
    pub destination_step: i64,
    pub examples: usize,
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

    // §4.1 runs detection when the recording stops. It is pure and takes no
    // model, so it costs nothing on a session that turns out to have no
    // pattern -- which is most of them.
    let (template, no_template_reason) = propose_template(&report.source_links);

    let policy = RedactionPolicy::placeholder();
    let summary = CaptureSummary {
        session_name: report.session_name.clone(),
        action_count: report.actions.len(),
        excluded_count: report.exclusions.len(),
        unmapped_events: report.unmapped_events,
        pastes_observed: report.pastes_observed,
        actions: view_of(&report.actions, &policy),
        template,
        no_template_reason,
    };

    {
        let mut pending = state.pending_actions.lock().map_err(|e| e.to_string())?;
        *pending = Some(report.actions);
    }
    let mut links = state.pending_links.lock().map_err(|e| e.to_string())?;
    *links = Some(report.source_links);

    Ok(summary)
}

/// Run detection over a session's source links and describe the outcome.
///
/// Returns the proposal, or the reason there is none. Every negative is worded
/// as the next thing to do rather than as a diagnosis, because that is what the
/// user needs: §4.1 asks for "keep recording" on too few examples, §4.12 asks
/// for "split the recording" on multiple patterns, and §2 is explicit that a
/// still source is inconclusive rather than a confirmed constant.
fn propose_template(
    links: &[crate::capture::grid::SourceLink],
) -> (Option<TemplateProposal>, Option<String>) {
    let Some((source, destination)) = detect::link::dominant_surfaces(links) else {
        // Not an anomaly. Nothing was copied between grids, so this is an
        // ordinary recording and there was never a pattern to look for.
        return (None, None);
    };
    let observations = detect::link::observations(links);

    match detect::detect(&observations, &source, &destination) {
        Detection::Pattern(p) => (
            Some(TemplateProposal {
                source,
                destination,
                fields: p
                    .fields
                    .iter()
                    .map(|f| MappedField {
                        from: f.source_field.clone(),
                        to: f.destination_field.clone(),
                    })
                    .collect(),
                source_step: p.source_step,
                destination_step: p.destination_step,
                examples: p.examples,
            }),
            None,
        ),
        Detection::TooFewExamples { records } => (
            None,
            Some(format!(
                "only {records} record{} were copied across; three are needed \
                 before a repeating pattern can be confirmed",
                if records == 1 { "" } else { "s" }
            )),
        ),
        Detection::SourceDidNotAdvance => (
            None,
            Some(
                "the destination moved on but the source stayed on one record, \
                 so what should change each time is undetermined"
                    .to_string(),
            ),
        ),
        Detection::InconsistentAdvance { .. } => (
            None,
            Some("the records were not a consistent distance apart".to_string()),
        ),
        Detection::InconsistentMapping { .. } => (
            None,
            Some(
                "the records disagree about which source field feeds which \
                 destination field"
                    .to_string(),
            ),
        ),
        Detection::MultiplePatterns { signatures } => (
            None,
            Some(format!(
                "{signatures} separate patterns were recorded together; \
                 record them as separate workflows"
            )),
        ),
    }
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
    confirm_template: Option<bool>,
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

    // §4.2's answer. Only when the user said yes -- `compile` itself is
    // untouched and always produces an ordinary playbook, so declining, or
    // never being asked, leaves exactly the Phase 1 behaviour.
    let playbook = if confirm_template.unwrap_or(false) {
        let template = confirmed_template(&state)?;
        playbook.with_template(template)
    } else {
        playbook
    };

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

    // Consume the pending capture so it cannot be stored twice. The links go
    // with it: they describe that same session, and leaving them behind would
    // let a later compile build a template out of a recording that is gone.
    {
        let mut pending = state.pending_actions.lock().map_err(|e| e.to_string())?;
        *pending = None;
    }
    let mut links = state.pending_links.lock().map_err(|e| e.to_string())?;
    *links = None;

    Ok(info)
}

/// Rebuild the template the user just confirmed, from the links of the session
/// being compiled.
///
/// Detection is re-run here rather than the proposal being carried over from
/// `stop_record_session`. That is deliberate: the proposal that crossed the IPC
/// boundary is a display view, and trusting a value that has been outside the
/// backend to decide what gets written to the database would undo the same
/// invariant the module docs describe for `CapturedAction`. The links never
/// left app state, so re-deriving from them is both cheap and trustworthy.
///
/// ## What is NOT done here, and why
///
/// `detect::verify` -- item 4's Qwen check -- is not called. It needs a
/// human-readable label per field locator, which comes from the source's header
/// row, and by this point the recording has ended and nothing holds a live
/// reader on the source. Calling it with the locators as their own labels would
/// be worse than not calling it: `verify` explicitly returns `Unsure` for
/// "C -> B" because that carries no meaning to judge, so it would produce a
/// guaranteed non-answer wearing the appearance of a check. The verification
/// belongs where a reader is open on the source -- §4.3's first-record preview.
fn confirmed_template(state: &State<'_, AppState>) -> Result<CompiledTemplate, String> {
    let links = state.pending_links.lock().map_err(|e| e.to_string())?;
    let links = links
        .as_deref()
        .ok_or_else(|| "no captured session to build a template from".to_string())?;

    let (source, destination) = detect::link::dominant_surfaces(links)
        .ok_or_else(|| "this recording copied nothing between documents".to_string())?;
    let observations = detect::link::observations(links);

    match detect::detect(&observations, &source, &destination) {
        Detection::Pattern(p) => Ok(CompiledTemplate::from_pattern(&p, source, destination)),
        // The confirmation and the recording disagree. Refusing is the only
        // safe answer: storing a template detection does not stand behind would
        // put a workflow into the product that nothing has justified.
        _ => Err(
            "this recording no longer shows a repeating pattern, so it was stored as an \
             ordinary playbook instead"
                .to_string(),
        ),
    }
}

// ------------------------------------- §4.3 first-record safety check ----

/// One mapped field of the record about to be written.
#[derive(Debug, Serialize)]
pub struct PreviewFieldView {
    pub source_field: String,
    pub source_label: Option<String>,
    pub destination_field: String,
    pub destination_label: Option<String>,
    /// The real value, which is the whole point -- §4.3 asks for "real values,
    /// in the real destination". Shown, never stored.
    pub value: String,
}

/// What the user answers confirm/cancel about.
#[derive(Debug, Serialize)]
pub struct PreviewView {
    pub playbook_id: String,
    pub source_row: String,
    pub destination_row: u64,
    pub fields: Vec<PreviewFieldView>,
    /// The model's read on the mapping. **Advisory** -- see `run::preview`:
    /// the measured confidence band does not separate sensible mappings from
    /// nonsense, so this informs the user rather than deciding for them.
    pub verdict: String,
    pub verdict_is_reassuring: bool,
}

/// Nothing to preview, and why. §4.8's "nothing new" is an answer, not a fault.
#[derive(Debug, Serialize)]
pub struct NothingToPreview {
    pub reason: String,
}

/// The answer to "what would this workflow do next?".
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PreviewOutcome {
    Ready(PreviewView),
    NothingToDo(NothingToPreview),
    NeedsAttention(NothingToPreview),
}

/// Show the very next record this workflow would write, without writing it.
///
/// §4.3. This is the only way to obtain the authorization `start_workflow_run`
/// needs, so it is not merely the recommended first step -- it is the only
/// first step there is.
#[tauri::command]
pub async fn preview_workflow_run(
    state: State<'_, AppState>,
    playbook_id: String,
    source_row: Option<u64>,
    header_row: Option<u64>,
    destination_row: Option<u64>,
) -> Result<PreviewOutcome, String> {
    let template = {
        let conn = state.db.lock().await;
        store::load_template(&conn, &playbook_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "that playbook is not a templated workflow".to_string())?
    };

    let source_row = source_row.unwrap_or(2);
    let header_row = header_row.unwrap_or(1);
    let destination_row = destination_row.unwrap_or(2);

    let desktop = terminator::Desktop::new(false, false)
        .map_err(|e| format!("accessibility engine unavailable: {e}"))?;
    let (mut reader, _writer) =
        run::surfaces::open_for(&desktop, &template, source_row, header_row, destination_row)
            .await
            .map_err(|e| e)?;

    // Header rows first: the verdict needs words, not column letters, and the
    // preview shows the user which column is which.
    let source_shape = reader.shape().map_err(|e| e.to_string())?;
    let destination_shape = {
        // A second reader on the destination, purely to read its header row.
        // Reading is non-destructive, and `SourceShape` is the only thing that
        // can turn a destination column letter into a word.
        let (destination_doc, destination_sheet) =
            run::surfaces::split_surface_id(&template.destination_id);
        match run::surfaces::window_for(&desktop, destination_doc).await {
            Some(w) => {
                let columns: Vec<String> = template
                    .fields
                    .iter()
                    .map(|f| f.destination_field.clone())
                    .collect();
                match crate::source::spreadsheet::SpreadsheetReader::open(
                    desktop.clone(),
                    &w,
                    template.destination_id.clone(),
                    destination_sheet.map(str::to_string),
                    destination_row,
                    header_row,
                    columns,
                )
                .await
                {
                    Ok(mut r) => r.shape().unwrap_or(crate::source::SourceShape {
                        columns: vec![],
                    }),
                    Err(_) => crate::source::SourceShape { columns: vec![] },
                }
            }
            None => crate::source::SourceShape { columns: vec![] },
        }
    };

    // The model runs here, where both header rows are in hand. A failure to
    // load it is not a failure to preview: §4.3's safety check is the user
    // seeing the record, and the verdict is advice on top of that.
    let verdict = match labeling::shared(&model_path(&state)) {
        Ok(engine) => {
            run::preview::verdict_for(&engine, &template, &source_shape, &destination_shape)
                .unwrap_or_else(|e| crate::detect::verify::Verdict::Unsure {
                    confidence: 0.0,
                    reason: format!("the model could not be asked: {e}"),
                })
        }
        Err(e) => crate::detect::verify::Verdict::Unsure {
            confidence: 0.0,
            reason: format!("the model is unavailable: {e}"),
        },
    };

    let upcoming = {
        let conn = state.db.lock().await;
        run::preview::next_record(
            &conn,
            &playbook_id,
            &template,
            reader.as_mut(),
            destination_row,
            verdict,
        )
        .map_err(|e| e.to_string())?
    };

    match upcoming {
        run::preview::Upcoming::NothingToDo => Ok(PreviewOutcome::NothingToDo(NothingToPreview {
            reason: "every record in the source has already been processed".to_string(),
        })),
        run::preview::Upcoming::SuspiciousGap {
            position,
            rows_with_data_below,
        } => Ok(PreviewOutcome::NeedsAttention(NothingToPreview {
            reason: format!(
                "row {} is blank in the mapped columns but {rows_with_data_below} row(s) below \
                 it still have data, so this may not be the end of the source",
                position.row_key
            ),
        })),
        run::preview::Upcoming::Ready(mut preview) => {
            run::preview::label_fields(&mut preview, &source_shape, &destination_shape);

            let view = PreviewView {
                playbook_id: playbook_id.clone(),
                source_row: preview.position().row_key.clone(),
                destination_row: preview.destination_row(),
                fields: preview
                    .fields()
                    .iter()
                    .map(|f| PreviewFieldView {
                        source_field: f.source_field.clone(),
                        source_label: f.source_label.clone(),
                        destination_field: f.destination_field.clone(),
                        destination_label: f.destination_label.clone(),
                        value: f.value.clone(),
                    })
                    .collect(),
                verdict: describe_verdict(preview.verdict()),
                verdict_is_reassuring: matches!(
                    preview.verdict(),
                    crate::detect::verify::Verdict::Sensible { .. }
                ),
            };

            let mut slot = state.pending_preview.lock().map_err(|e| e.to_string())?;
            *slot = Some(preview);
            Ok(PreviewOutcome::Ready(view))
        }
    }
}

fn describe_verdict(v: &crate::detect::verify::Verdict) -> String {
    use crate::detect::verify::Verdict;
    match v {
        Verdict::Sensible { confidence } => {
            format!("the mapping looks sensible (confidence {confidence:.2})")
        }
        Verdict::NotSensible { confidence } => format!(
            "the model does not think this mapping makes sense (confidence {confidence:.2}) -- \
             check the record below carefully before continuing"
        ),
        Verdict::Unsure { reason, .. } => format!("no clear read on the mapping: {reason}"),
    }
}

/// The user declined the preview. §4.10: "cancels cleanly. Nothing activates."
///
/// The stored playbook is untouched -- it was already an ordinary playbook with
/// a template attached, and declining a run does not change what it is. What
/// declining prevents is the run, which is the whole of what §4.3 gates.
#[tauri::command]
pub async fn cancel_workflow_preview(state: State<'_, AppState>) -> Result<(), String> {
    let mut slot = state.pending_preview.lock().map_err(|e| e.to_string())?;
    if let Some(preview) = slot.take() {
        preview.decline();
    }
    Ok(())
}

/// Start the run the user just confirmed (§4.3, §4.10).
///
/// Takes no playbook id. That is deliberate: the run is defined by the preview
/// that was confirmed, so there is no parameter a caller could use to start
/// something other than what they were shown.
#[tauri::command]
pub async fn start_workflow_run(
    state: State<'_, AppState>,
    source_row: Option<u64>,
    header_row: Option<u64>,
    destination_row: Option<u64>,
) -> Result<RunStatusView, String> {
    {
        let active = state.active_run.lock().map_err(|e| e.to_string())?;
        if active.as_ref().is_some_and(|r| !r.is_finished()) {
            return Err("a workflow run is already in progress".to_string());
        }
    }

    // Taken, not borrowed: one confirmation starts one run.
    let preview = {
        let mut slot = state.pending_preview.lock().map_err(|e| e.to_string())?;
        slot.take().ok_or_else(|| {
            "no confirmed first-record preview; preview the next record before running".to_string()
        })?
    };
    let playbook_id = preview.playbook_id().to_string();
    let authorization = preview.accept();

    let template = {
        let conn = state.db.lock().await;
        store::load_template(&conn, &playbook_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "that playbook is not a templated workflow".to_string())?
    };

    let (db_path, key_path) = crate::db::paths_in(
        state
            .db_path
            .parent()
            .ok_or_else(|| "the database path has no parent directory".to_string())?,
    );

    let control = run::RunControl::new();
    let active = run::background::spawn(
        db_path,
        key_path,
        playbook_id.clone(),
        template.clone(),
        control,
        authorization,
        run::surfaces::factory_for(
            template,
            source_row.unwrap_or(2),
            header_row.unwrap_or(1),
            destination_row.unwrap_or(2),
        ),
    )?;

    {
        let conn = state.db.lock().await;
        run::set_run_state(&conn, &playbook_id, run::RunState::Running)
            .map_err(|e| e.to_string())?;
    }

    let finished = active.is_finished();
    let mut slot = state.active_run.lock().map_err(|e| e.to_string())?;
    *slot = Some(active);

    Ok(RunStatusView {
        playbook_id: Some(playbook_id),
        state: run::RunState::Running.as_str().to_string(),
        finished,
    })
}

// ------------------------------------------------------ run controls ----
//
// §4.6's Stop and Pause, reaching the run over IPC. Each is a thin wrapper: the
// decisions -- stop being permanent, pause backing up to the start of the
// in-progress record -- live in `run::control` and `run::run_with_control`,
// where they can be tested without a window.
//
// `run_state` is written here, at the moment the user's intent arrives, rather
// than when the loop next reaches a safe point. A run told to pause between two
// slow records should read as paused immediately; the alternative is a UI that
// ignores the button until the loop catches up.

/// What the frontend needs to render the running-state overlay.
#[derive(Debug, Serialize)]
pub struct RunStatusView {
    pub playbook_id: Option<String>,
    /// `idle`, `running` or `paused`.
    pub state: String,
    /// True once the thread has ended, whatever the reason.
    pub finished: bool,
}

/// The controls act on the run in progress, so they need one.
fn with_active_run<T>(
    state: &State<'_, AppState>,
    f: impl FnOnce(&run::background::ActiveRun) -> T,
) -> Result<T, String> {
    let slot = state.active_run.lock().map_err(|e| e.to_string())?;
    let active = slot
        .as_ref()
        .ok_or_else(|| "no workflow run is in progress".to_string())?;
    Ok(f(active))
}

/// Pause the run at its next safe point (§4.6).
#[tauri::command]
pub async fn pause_workflow_run(state: State<'_, AppState>) -> Result<RunStatusView, String> {
    let playbook_id = with_active_run(&state, |active| {
        active.control.pause();
        active.playbook_id.clone()
    })?;

    let conn = state.db.lock().await;
    run::set_run_state(&conn, &playbook_id, run::RunState::Paused).map_err(|e| e.to_string())?;

    Ok(RunStatusView {
        playbook_id: Some(playbook_id),
        state: run::RunState::Paused.as_str().to_string(),
        finished: false,
    })
}

/// Resume a paused run. It redoes the record it was in the middle of.
///
/// Refuses when there was nothing to resume, rather than reporting success:
/// `RunControl::resume` deliberately will not restart a stopped run, and a
/// caller told "resumed" about a run that stayed stopped would have no way to
/// tell.
#[tauri::command]
pub async fn resume_workflow_run(state: State<'_, AppState>) -> Result<RunStatusView, String> {
    let (playbook_id, resumed) = with_active_run(&state, |active| {
        (active.playbook_id.clone(), active.control.resume())
    })?;
    if !resumed {
        return Err("that run is not paused; a stopped run cannot be resumed".to_string());
    }

    let conn = state.db.lock().await;
    run::set_run_state(&conn, &playbook_id, run::RunState::Running).map_err(|e| e.to_string())?;

    Ok(RunStatusView {
        playbook_id: Some(playbook_id),
        state: run::RunState::Running.as_str().to_string(),
        finished: false,
    })
}

/// Stop the run for good (§4.6). The record in progress finishes cleanly, or --
/// if the run was paused mid-record -- is discarded.
///
/// The handle is taken out of app state, because a stopped run is over and
/// leaving it there would let a later Resume find something to talk to. The
/// thread is not joined here: joining would block the caller until the current
/// record finished, which is the opposite of what §4.10 asks for.
#[tauri::command]
pub async fn stop_workflow_run(state: State<'_, AppState>) -> Result<RunStatusView, String> {
    let active = {
        let mut slot = state.active_run.lock().map_err(|e| e.to_string())?;
        slot.take()
            .ok_or_else(|| "no workflow run is in progress".to_string())?
    };
    active.control.stop();

    let conn = state.db.lock().await;
    run::set_run_state(&conn, &active.playbook_id, run::RunState::Idle)
        .map_err(|e| e.to_string())?;

    Ok(RunStatusView {
        playbook_id: Some(active.playbook_id.clone()),
        state: run::RunState::Idle.as_str().to_string(),
        finished: active.is_finished(),
    })
}

/// The current run's state, for the overlay. Never an error when nothing is
/// running -- "nothing is running" is an answer, not a failure.
#[tauri::command]
pub async fn get_workflow_run_status(state: State<'_, AppState>) -> Result<RunStatusView, String> {
    let slot = state.active_run.lock().map_err(|e| e.to_string())?;
    let Some(active) = slot.as_ref() else {
        return Ok(RunStatusView {
            playbook_id: None,
            state: run::RunState::Idle.as_str().to_string(),
            finished: true,
        });
    };
    let state_str = match active.control.state() {
        run::ControlState::Paused => run::RunState::Paused,
        run::ControlState::Stopped => run::RunState::Idle,
        run::ControlState::Running => run::RunState::Running,
    };
    Ok(RunStatusView {
        playbook_id: Some(active.playbook_id.clone()),
        state: state_str.as_str().to_string(),
        finished: active.is_finished(),
    })
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
    runs_with_logs(&conn, runs)
}

/// Past runs whose playbook has been deleted, with their step logs.
///
/// The counterpart to `get_run_history`, which cannot reach these: it takes a
/// `playbook_id` to look up, and these rows no longer have one. Deleting a
/// playbook detaches its runs rather than removing them —
/// `migrations/20260803000002_init_runs.sql` says "run history must outlive its
/// playbook" — so without this the retained history was unreadable.
///
/// Takes no arguments deliberately. There is no id to scope by, which is
/// precisely what makes these runs orphaned. `RunHistoryEntry` carries no
/// `playbook_id` because for every row here it would be null.
#[tauri::command]
pub async fn get_orphaned_run_history(
    state: State<'_, AppState>,
) -> Result<Vec<RunHistoryEntry>, String> {
    let conn = state.db.lock().await;
    let runs = journal::load_orphaned_runs(&conn).map_err(|e| e.to_string())?;
    runs_with_logs(&conn, runs)
}

/// Attach each run's step log. Shared by both history commands so they cannot
/// present the same rows differently.
fn runs_with_logs(
    conn: &rusqlite::Connection,
    runs: Vec<journal::StoredRun>,
) -> Result<Vec<RunHistoryEntry>, String> {
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        let steps = journal::load_step_logs(conn, &run.id).map_err(|e| e.to_string())?;
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

    // ---------------- the stop -> proposal wiring (§4.1, §4.12) ----------------

    fn link(seq: u64, src: &str, dst: &str) -> crate::capture::grid::SourceLink {
        crate::capture::grid::SourceLink {
            seq,
            source_document: "Orders".into(),
            source_cell: src.into(),
            destination_document: "Invoices".into(),
            destination_cell: dst.into(),
        }
    }

    #[test]
    fn three_aligned_copies_are_offered_as_a_template() {
        let (proposal, reason) = propose_template(&[
            link(1, "C2", "B2"),
            link(2, "C3", "B3"),
            link(3, "C4", "B4"),
        ]);
        assert_eq!(reason, None);
        let p = proposal.expect("a pattern should have been detected");
        assert_eq!(p.source, "Orders");
        assert_eq!(p.destination, "Invoices");
        assert_eq!(p.source_step, 1);
        assert_eq!(p.destination_step, 1);
        assert_eq!(p.examples, 3);
        assert_eq!(p.fields.len(), 1);
        assert_eq!(p.fields[0].from, "C");
        assert_eq!(p.fields[0].to, "B");
    }

    #[test]
    fn two_copies_are_not_a_pattern_and_the_reason_says_what_to_do() {
        // §2's Rule of 3. The message has to be actionable, not a diagnosis.
        let (proposal, reason) = propose_template(&[link(1, "C2", "B2"), link(2, "C3", "B3")]);
        assert!(proposal.is_none());
        let reason = reason.expect("a refusal must explain itself");
        assert!(reason.contains("three are needed"), "unhelpful: {reason}");
    }

    #[test]
    fn an_ordinary_recording_proposes_nothing_and_reports_no_problem() {
        // Nothing was copied between grids, so there was never a pattern to
        // look for. That is not a failed detection and must not read as one.
        let (proposal, reason) = propose_template(&[]);
        assert!(proposal.is_none());
        assert_eq!(reason, None, "an ordinary recording is not a refusal");
    }

    #[test]
    fn a_still_source_is_reported_as_inconclusive_not_as_a_constant() {
        // §2 is explicit: a source that did not move is inconclusive, "not as
        // confirmation of a fixed, unchanging value".
        let (proposal, reason) = propose_template(&[
            link(1, "C2", "B2"),
            link(2, "C2", "B3"),
            link(3, "C2", "B4"),
        ]);
        assert!(proposal.is_none());
        let reason = reason.expect("a refusal must explain itself");
        assert!(
            reason.contains("stayed on one record"),
            "should name the still source: {reason}"
        );
    }

    #[test]
    fn a_multi_field_pattern_reports_every_mapped_field() {
        // §4.12: a pattern may span several fields as long as they advance
        // together.
        let mut links = Vec::new();
        for (i, row) in [2, 3, 4].iter().enumerate() {
            links.push(crate::capture::grid::SourceLink {
                seq: (i * 2) as u64,
                source_document: "Orders".into(),
                source_cell: format!("C{row}"),
                destination_document: "Invoices".into(),
                destination_cell: format!("B{row}"),
            });
            links.push(crate::capture::grid::SourceLink {
                seq: (i * 2 + 1) as u64,
                source_document: "Orders".into(),
                source_cell: format!("D{row}"),
                destination_document: "Invoices".into(),
                destination_cell: format!("E{row}"),
            });
        }
        let (proposal, _) = propose_template(&links);
        let p = proposal.expect("a two-field pattern should be detected");
        let pairs: Vec<(String, String)> = p
            .fields
            .iter()
            .map(|f| (f.from.clone(), f.to.clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("C".to_string(), "B".to_string()),
                ("D".to_string(), "E".to_string())
            ]
        );
    }

}
