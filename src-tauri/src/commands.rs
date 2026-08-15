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
    /// Whether this is a confirmed repeating workflow, so the list can mark it
    /// and offer the batch check. Section 6 reuses this list rather than
    /// adding a parallel screen, so it has to be able to tell them apart.
    pub is_templated: bool,
    /// A repeating pattern was offered for this recording and the user declined
    /// it. Only ever true when `is_templated` is false.
    ///
    /// The list needs it to answer "why isn't this one repeating?", which had no
    /// answer before: a declined recording and one where detection found
    /// nothing produced identical rows.
    pub template_declined: bool,
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

/// Is a capture session currently open in the backend?
///
/// Exists because the two halves can disagree, and only one of them is right.
/// `state.session` lives in the Rust process; the phase driving Record Mode's
/// UI lives in React state in a webview. A webview reload -- routine under
/// `npm run tauri dev`, and possible in production -- resets the phase to
/// "idle" while the session carries on recording. The UI then shows nothing
/// happening, and the next Start is refused with "a recording session is
/// already active".
///
/// Without this, the frontend has no way to ask; it can only assume, and its
/// assumption is wrong exactly when it matters. Reports the fact and nothing
/// else -- what to do about a disagreement is the caller's decision.
#[tauri::command]
pub async fn record_session_active(state: State<'_, AppState>) -> Result<bool, String> {
    let slot = state.session.lock().map_err(|e| e.to_string())?;
    Ok(slot.is_some())
}

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
    let (template, no_template_reason) =
        propose_template(&report.source_links, report.pastes_observed);

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
    pastes_observed: usize,
) -> (Option<TemplateProposal>, Option<String>) {
    let Some((source, destination)) = detect::link::dominant_surfaces(links) else {
        // No usable source positions. Whether that is worth saying depends
        // entirely on whether the user copied anything at all, and those two
        // cases were previously indistinguishable -- both returned silence.
        //
        // A real recording hit the wrong half of that: six pastes from a
        // Google Doc into a Sheet produced no cell positions, so detection
        // said nothing and the review screen showed nothing, leaving the user
        // to guess why the pattern prompt never appeared.
        if pastes_observed > 0 {
            return (
                None,
                Some(format!(
                    "{pastes_observed} paste{} seen, but none of them could be traced to a \r
                     source cell. A repeating workflow has to copy FROM a spreadsheet -- \r
                     a document, a web page or a PDF has no cells to read a position from.",
                    if pastes_observed == 1 { "" } else { "s" }
                )),
            );
        }
        // Genuinely ordinary: nothing was copied, so there was never a
        // pattern to look for and silence is right.
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
    } else if was_offered_a_template(&state) {
        // Said no to something real. Recorded so that a later "why isn't this
        // repeating?" has the honest answer -- previously this row was
        // indistinguishable from a recording where detection found nothing, and
        // `no_template_reason` cannot fill the gap: it is only ever set when
        // there was no pattern, so it is silent by construction in exactly this
        // case.
        //
        // Recomputed from the same pending links `confirmed_template` uses,
        // rather than trusting a new flag from the caller. The frontend shows
        // the proposal if and only if `propose_template` returned one, so this
        // reproduces what was actually on screen instead of asking the UI to
        // report on itself.
        playbook.with_declined_template()
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
/// Was the user actually shown a repeating-pattern proposal for this recording?
///
/// Answers the question the way the review screen answered it: `stop_record_session`
/// offers a proposal exactly when [`propose_template`] returns one, so running
/// the same function over the same pending links reproduces what was on screen.
///
/// Never an error. A missing or unusable session means no proposal was shown,
/// which is a `false` rather than a failure -- this only ever decides whether to
/// record an extra fact, and refusing to store a playbook because that fact
/// could not be determined would be wildly out of proportion.
fn was_offered_a_template(state: &State<'_, AppState>) -> bool {
    let Ok(links) = state.pending_links.lock() else {
        return false;
    };
    let Some(links) = links.as_deref() else {
        return false;
    };
    // `pastes_observed` only affects the WORDING of a refusal, never whether a
    // proposal exists, so 0 is safe here and avoids threading it through state.
    propose_template(links, 0).0.is_some()
}

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

// ------------------------------------------- §4.5 correction scope ----

fn parse_side(raw: &str) -> Result<run::drift::Side, String> {
    run::drift::Side::parse(raw)
        .ok_or_else(|| format!("side must be \"source\" or \"destination\", not {raw:?}"))
}

/// What the user has selected, for the correction panel's confirm step.
#[derive(Debug, Serialize)]
pub struct SelectionView {
    pub column: String,
    pub label: Option<String>,
}

/// Read the column the user has clicked in the live spreadsheet (§4.5).
///
/// This is what makes the interaction click-only: the panel asks the user to
/// click the right column and then reads which one, so nothing is typed and a
/// mis-typed letter cannot silently repoint a mapping.
#[tauri::command]
pub async fn read_selected_column(
    state: State<'_, AppState>,
    playbook_id: String,
    side: String,
    header_row: Option<u64>,
) -> Result<SelectionView, String> {
    let side = parse_side(&side)?;
    let template = {
        let conn = state.db.lock().await;
        store::load_template(&conn, &playbook_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "that playbook is not a templated workflow".to_string())?
    };
    let surface = match side {
        run::drift::Side::Source => template.source_id,
        run::drift::Side::Destination => template.destination_id,
    };

    let desktop = terminator::Desktop::new(false, false)
        .map_err(|e| format!("accessibility engine unavailable: {e}"))?;
    let selection =
        run::surfaces::read_selection(&desktop, &surface, header_row.unwrap_or(1)).await?;

    Ok(SelectionView {
        column: selection.column,
        label: selection.label,
    })
}

/// Repoint a mapped column permanently (§4.5's "the format actually changed").
///
/// Updates the stored mapping AND re-records the shape, so the next drift
/// check compares against what the user just confirmed rather than against the
/// shape that drifted. Doing only the first would leave the workflow blocked by
/// a drift it had already been told about.
///
/// The label is required because the shape is a locator-plus-label pair: a
/// correction that moved the column without saying what it is now would leave a
/// recorded shape that no longer describes the sheet.
#[tauri::command]
pub async fn apply_permanent_correction(
    state: State<'_, AppState>,
    playbook_id: String,
    side: String,
    old_locator: String,
    new_locator: String,
    new_label: String,
) -> Result<(), String> {
    let side = parse_side(&side)?;
    let conn = state.db.lock().await;
    run::drift::apply_permanent_correction(
        &conn,
        &playbook_id,
        side,
        &old_locator,
        &new_locator,
        &new_label,
    )
    .map_err(|e| e.to_string())
}

/// Repoint a mapped column for ONE record only (§4.5's "this one order was
/// weird").
///
/// Applies to the run in progress and to exactly the source record named. It is
/// never written to the database and cannot outlive the record it addresses --
/// see `run::correction`.
#[tauri::command]
pub async fn apply_one_off_correction(
    state: State<'_, AppState>,
    source_row: String,
    side: String,
    old_locator: String,
    new_locator: String,
) -> Result<(), String> {
    let side = parse_side(&side)?;
    let slot = state.active_run.lock().map_err(|e| e.to_string())?;
    let active = slot.as_ref().ok_or_else(|| {
        "no workflow run is in progress; a one-off correction applies to a record in a run"
            .to_string()
    })?;
    active.corrections.add(run::correction::OneOffCorrection {
        row_key: source_row,
        side,
        old_locator,
        new_locator,
    });
    Ok(())
}

// ------------------------------------------- §4.8 new-batch detection ----

/// The answer to "is there anything new to do?".
#[derive(Debug, Serialize)]
pub struct NewBatchView {
    pub playbook_id: String,
    /// True only when there is work. The frontend shows the confirmation on
    /// this and nothing else.
    pub has_work: bool,
    pub count: usize,
    pub first_row: Option<String>,
    /// The count is a floor, because the scan stopped at its ceiling.
    pub capped: bool,
    /// The sentence §4.8 asks for, already worded.
    pub message: String,
}

/// Look for new, unprocessed records in a workflow's source (§4.8).
///
/// ## This cannot start a run
///
/// It returns a count and a sentence. Answering "yes, run these" means calling
/// `preview_workflow_run` and then `start_workflow_run`, exactly as any other
/// route to a run does -- there is no batch-specific entry point, because a
/// second way in would be a second gate to keep correct, and the weaker one
/// would win.
///
/// §4.10 scopes the watching: "whenever the app is open (including minimized)
/// -- not a separate, persistent background service". This is a command the app
/// calls; nothing here runs on its own.
#[tauri::command]
pub async fn check_for_new_records(
    state: State<'_, AppState>,
    playbook_id: String,
    source_row: Option<u64>,
    header_row: Option<u64>,
) -> Result<NewBatchView, String> {
    let template = {
        let conn = state.db.lock().await;
        store::load_template(&conn, &playbook_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "that playbook is not a templated workflow".to_string())?
    };

    let desktop = terminator::Desktop::new(false, false)
        .map_err(|e| format!("accessibility engine unavailable: {e}"))?;
    let (mut reader, _writer) = run::surfaces::open_for(
        &desktop,
        &template,
        source_row.unwrap_or(2),
        header_row.unwrap_or(1),
        // The scan never writes, so where the destination would start is
        // irrelevant to it. Passed only because opening a writer is part of
        // resolving the pair.
        2,
    )
    .await?;

    let scan = {
        let conn = state.db.lock().await;
        run::batch::scan(&conn, &playbook_id, &template, reader.as_mut())
            .map_err(|e| e.to_string())?
    };

    let (count, first_row, capped) = match &scan {
        run::batch::BatchScan::Found {
            count,
            first_row,
            capped,
        } => (*count, Some(first_row.clone()), *capped),
        _ => (0, None, false),
    };

    Ok(NewBatchView {
        playbook_id,
        has_work: scan.has_work() && count > 0,
        count,
        first_row,
        capped,
        message: scan.describe(),
    })
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
    /// Empty unless something is correctable.
    ///
    /// A prose reason cannot drive §4.5's panel: it needs the side, the locator
    /// that moved and the best guess as separate values in order to build a
    /// correction out of them.
    pub corrections: Vec<CorrectionRequestView>,
}

/// One blocking drift, structured enough to drive the correction panel.
#[derive(Debug, Serialize)]
pub struct CorrectionRequestView {
    /// `source` or `destination`.
    pub side: String,
    /// The locator the template names, and which is now wrong.
    pub old_locator: String,
    /// What that column was called when the workflow was confirmed.
    pub old_label: Option<String>,
    /// §4.5's "Looks like column D now?", when there is a plausible one.
    pub best_guess: Option<String>,
    pub detail: String,
}

/// Turn the blocking half of a drift check into correction requests.
fn correctable(check: &run::drift::DriftCheck) -> Vec<CorrectionRequestView> {
    check
        .findings()
        .iter()
        .filter(|f| f.blocking)
        .map(|f| {
            // The locator and label the recorded shape knew, which is what the
            // panel has to name when it asks "use this for X?".
            let (old_locator, old_label) = match &f.drift {
                crate::source::Drift::LabelChanged { locator, was, .. } => {
                    (locator.clone(), Some(was.clone()))
                }
                crate::source::Drift::Moved { label, was, .. } => {
                    (was.clone(), Some(label.clone()))
                }
                crate::source::Drift::Missing { locator, label } => {
                    (locator.clone(), Some(label.clone()))
                }
                crate::source::Drift::Added { locator, label } => {
                    (locator.clone(), Some(label.clone()))
                }
            };
            CorrectionRequestView {
                side: f.side.as_str().to_string(),
                old_locator,
                old_label,
                best_guess: f.best_guess.clone(),
                detail: f.describe(),
            }
        })
        .collect()
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

    // Where the destination carries on, derived rather than assumed.
    //
    // Defaulting this to "the first row" would be wrong for every run after
    // the first: a second batch would overwrite the first batch's output
    // instead of appending to it. The ledger knows how many records this
    // workflow has taken from this source, and a count plus the advancement
    // rule gives the row -- without ever storing a row index, which §3 does
    // not allow. An explicit value from the caller still wins.
    let destination_row = match destination_row {
        Some(r) => r,
        None => {
            let conn = state.db.lock().await;
            run::batch::resume_destination_row(
                &conn,
                &playbook_id,
                &template.source_id,
                2,
                template.destination_step,
            )
            .map_err(|e| e.to_string())?
        }
    };

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

    // §4.5, at the point the user is about to confirm.
    //
    // Blocking drift stops here rather than at the run, and stopping here is
    // stronger: no preview is produced, so no `RunAuthorization` can exist, so
    // there is nothing to start. The run thread checks again anyway, because
    // the two moments are different and a column can move in between.
    //
    // The baseline is recorded on the first look. §4.5 wants the shape "at
    // template-confirmation time", and this is the first moment both surfaces
    // are open and the user is being asked to confirm them. Recording it when
    // the user declines is harmless -- declining does not change what the
    // sheets look like, and the alternative is a workflow that can never
    // detect drift because it never captured a baseline.
    {
        let conn = state.db.lock().await;
        let check = run::drift::check(
            &conn,
            &playbook_id,
            &template,
            &source_shape,
            &destination_shape,
        )
        .map_err(|e| e.to_string())?;

        if !check.may_run() {
            return Ok(PreviewOutcome::NeedsAttention(NothingToPreview {
                reason: check
                    .findings()
                    .iter()
                    .filter(|f| f.blocking)
                    .map(|f| match &f.best_guess {
                        Some(g) => format!("{} -- looks like column {g} now?", f.describe()),
                        None => f.describe(),
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
                // The same findings as structure, so the panel can build a
                // correction rather than parse a sentence.
                corrections: correctable(&check),
            }));
        }

        if matches!(check, run::drift::DriftCheck::NothingRecorded) {
            run::drift::record_shape(
                &conn,
                &playbook_id,
                run::drift::Side::Source,
                &source_shape,
            )
            .map_err(|e| e.to_string())?;
            run::drift::record_shape(
                &conn,
                &playbook_id,
                run::drift::Side::Destination,
                &destination_shape,
            )
            .map_err(|e| e.to_string())?;
        }
    }

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
            corrections: Vec::new(),
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
            // A suspicious gap is not a mapping problem: there is nothing here
            // for the correction panel to repoint.
            corrections: Vec::new(),
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
    // §4.5: stop and ask on a record missing a mapped field, so a one-off
    // correction has a record to attach to. Off by default, which is exactly
    // §4.4's documented behaviour.
    supervise: Option<bool>,
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
    // Resolved the same way the preview resolves it, so the run writes where
    // the user was shown it would. Two different defaults here would mean the
    // preview showed row 5 and the run wrote row 2.
    let destination_row = match destination_row {
        Some(r) => r,
        None => {
            let conn = state.db.lock().await;
            run::batch::resume_destination_row(
                &conn,
                &playbook_id,
                &template.source_id,
                2,
                template.destination_step,
            )
            .map_err(|e| e.to_string())?
        }
    };

    let active = run::background::spawn(
        db_path,
        key_path,
        playbook_id.clone(),
        template.clone(),
        header_row.unwrap_or(1),
        control,
        // §4.5's opt-in supervision. Off unless the caller asks, which keeps
        // §4.4's "continue past an incomplete record" as the default.
        if supervise.unwrap_or(false) {
            run::supervision::RunSupervision::on()
        } else {
            run::supervision::RunSupervision::off()
        },
        authorization,
        run::surfaces::factory_for(
            template,
            source_row.unwrap_or(2),
            header_row.unwrap_or(1),
            destination_row,
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
        ..Default::default()
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
#[derive(Debug, Default, Serialize)]
pub struct RunStatusView {
    pub playbook_id: Option<String>,
    /// `idle`, `running` or `paused`.
    pub state: String,
    /// True once the thread has ended, whatever the reason.
    pub finished: bool,
    /// §4.5: the record a supervised run has stopped on, when it has stopped on
    /// one. This is what gives the correction panel a record to attach a
    /// one-off to — without it the panel can only ever offer the permanent
    /// scope.
    pub awaiting_row: Option<String>,
    /// The mapped columns that were empty on that record.
    pub awaiting_missing_fields: Vec<String>,
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
        ..Default::default()
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
        ..Default::default()
    })
}

/// Stop the run for good (§4.6). The record in progress finishes cleanly, or --
/// if the run was paused mid-record -- is discarded.
///
/// The handle STAYS in app state. An earlier version took it out, on the
/// reasoning that a stopped run is over and leaving it would give a later
/// Resume something to talk to — but `RunControl::stop` is already permanent,
/// so Resume refuses on its own, and removing the handle broke something that
/// matters more: `get_workflow_run_report` reads the outcome through this
/// slot, so a stopped run had no reachable summary at all. The user pressed
/// Stop and got "Run finished" with nothing about what it had done.
///
/// Caught by driving the real UI; every unit test passed throughout, because
/// each command was correct on its own and only the pair was wrong.
///
/// The thread is not joined here: joining would block the caller until the
/// current record finished, which is the opposite of what §4.10 asks for.
#[tauri::command]
pub async fn stop_workflow_run(state: State<'_, AppState>) -> Result<RunStatusView, String> {
    let playbook_id = {
        let slot = state.active_run.lock().map_err(|e| e.to_string())?;
        let active = slot
            .as_ref()
            .ok_or_else(|| "no workflow run is in progress".to_string())?;
        active.control.stop();
        active.playbook_id.clone()
    };

    let conn = state.db.lock().await;
    run::set_run_state(&conn, &playbook_id, run::RunState::Idle).map_err(|e| e.to_string())?;

    Ok(RunStatusView {
        playbook_id: Some(playbook_id),
        state: run::RunState::Idle.as_str().to_string(),
        finished: false,
        ..Default::default()
    })
}

/// One record worth looking at. §4.9 shows these only when there are any.
#[derive(Debug, Serialize)]
pub struct FlaggedRecordView {
    pub source_row: String,
    pub destination_row: String,
    /// Which mapped fields the source did not have a value for.
    pub missing_fields: Vec<String>,
}

/// §4.9's end-of-run summary: quiet by default, detailed only when it matters.
#[derive(Debug, Serialize)]
pub struct RunSummaryView {
    pub playbook_id: String,
    /// `completed` | `stopped` | `needs_correction` | `failed` | `incomplete`
    pub status: String,
    /// The one plain line a clean run gets.
    pub headline: String,
    /// §4.9's "Processed orders 45–57". Null when nothing was written, which
    /// is a real outcome rather than an error.
    pub processed_range: Option<String>,
    pub written: usize,
    pub skipped: usize,
    /// §4.9: "shown only when greater than zero". The frontend hides it at 0
    /// rather than rendering "0 flagged for review", which would be exactly
    /// the noise the quiet-by-default rule exists to avoid.
    pub flagged: usize,
    /// Source rows that used a one-off correction (§4.5). Shown only when
    /// non-empty, for the same reason as `flagged`: a clean run should not
    /// carry a line saying nothing was corrected.
    pub corrected: Vec<String>,
    /// Whether the report should expand at all.
    pub needs_attention: bool,
    /// Empty on a clean run.
    pub flagged_records: Vec<FlaggedRecordView>,
    /// Why the run ended, when it was not ordinary exhaustion.
    pub stop_reason: Option<String>,
}

/// The summary of the run that just finished (§4.9).
///
/// `None` while a run is still going, and `None` when there has not been one.
/// Those are the same answer for a summary: there is nothing to show yet.
///
/// Reading does not clear it -- the user may close the summary and want it
/// back, and a report that vanished on first read would make that impossible.
/// `stop_workflow_run` and a new run are what replace it.
#[tauri::command]
pub async fn get_workflow_run_report(
    state: State<'_, AppState>,
) -> Result<Option<RunSummaryView>, String> {
    let slot = state.active_run.lock().map_err(|e| e.to_string())?;
    let Some(active) = slot.as_ref() else {
        return Ok(None);
    };
    let Some(outcome) = active.outcome() else {
        return Ok(None);
    };
    Ok(Some(summarise(&active.playbook_id, &outcome)))
}

fn summarise(playbook_id: &str, outcome: &run::background::RunOutcome) -> RunSummaryView {
    use run::background::RunOutcome;

    let base = |status: &str, headline: String, stop_reason: Option<String>| RunSummaryView {
        playbook_id: playbook_id.to_string(),
        status: status.to_string(),
        headline,
        processed_range: None,
        written: 0,
        skipped: 0,
        flagged: 0,
        corrected: Vec::new(),
        needs_attention: true,
        flagged_records: Vec::new(),
        stop_reason,
    };

    let report = match outcome {
        // Not a fault, and phrased so: §4.5 wants the correction panel, and a
        // summary reading "failed" would send the user looking for a bug.
        RunOutcome::DriftDetected(detail) => {
            return base(
                "needs_correction",
                "Stopped before writing anything — the sheet has changed".to_string(),
                Some(detail.clone()),
            )
        }
        RunOutcome::Failed(e) => {
            return base("failed", "The run could not start".to_string(), Some(e.clone()))
        }
        RunOutcome::Finished(r) => r,
    };

    let flagged_records: Vec<FlaggedRecordView> = report
        .incomplete()
        .into_iter()
        .map(|r| FlaggedRecordView {
            source_row: r.position.row_key.clone(),
            destination_row: r.destination.clone(),
            missing_fields: match &r.outcome {
                run::RecordOutcome::Written {
                    fit: run::RecordFit::MissingFields { fields },
                } => fields.clone(),
                _ => Vec::new(),
            },
        })
        .collect();

    let processed_range = report
        .processed_range()
        .map(|(first, last)| if first == last { first } else { format!("{first}–{last}") });

    let (status, stop_reason) = match &report.stop {
        run::RunStop::Exhausted => ("completed", None),
        run::RunStop::Stopped { mid_record, .. } => (
            "stopped",
            Some(if *mid_record {
                "You stopped it while it was paused mid-record, so that record was \
                 discarded and will be redone next time."
                    .to_string()
            } else {
                "You stopped it. The record it was working on finished first.".to_string()
            }),
        ),
        run::RunStop::SuspiciousGap { position, rows_with_data_below } => (
            "incomplete",
            Some(format!(
                "Stopped at row {}: the mapped columns are blank there but {rows_with_data_below} \
                 row(s) below still have data.",
                position.row_key
            )),
        ),
        run::RunStop::RecordDoesNotFit { position } => (
            "incomplete",
            Some(format!(
                "Stopped at row {}: it no longer matches the pattern.",
                position.row_key
            )),
        ),
        run::RunStop::SourceFailed { position, reason } => (
            "incomplete",
            Some(format!("Could not read row {}: {reason}", position.row_key)),
        ),
        run::RunStop::WriteFailed { position, cell, wrote, reason } => (
            "incomplete",
            Some(format!(
                "Row {} stopped part-way. {}

The destination cell {} MAY HAVE BEEN \r
                 ALTERED: a write that fails verification has still typed into the cell, \r
                 so check it before re-running.

What failed: {reason}",
                position.row_key,
                if wrote.is_empty() {
                    "No cell finished writing successfully.".to_string()
                } else {
                    format!("Finished writing: {}.", wrote.join(", "))
                },
                if cell.is_empty() { "for this record".to_string() } else { cell.clone() },
            )),
        ),
        run::RunStop::MarkFailed { position, reason } => (
            "incomplete",
            Some(format!(
                "Row {} was written but could not be recorded as done, so a re-run would \
                 write it twice: {reason}",
                position.row_key
            )),
        ),
        run::RunStop::LimitReached { limit } => (
            "incomplete",
            Some(format!("Stopped after {limit} records, the per-run ceiling.")),
        ),
    };

    let written = report.written();
    let needs_attention = status != "completed" || !flagged_records.is_empty();

    // §4.9: "A clean run gets a short, plain line."
    let headline = match &processed_range {
        Some(range) if written == 1 => format!("Processed row {range}."),
        Some(range) => format!("Processed rows {range} — {written} records."),
        // A failed write has ALREADY typed into the destination -- what fails
        // is the verification, not the typing. "Nothing was written" would be
        // false in exactly the moment the user most needs it to be true, and it
        // was: a run that mangled a cell reported that nothing had happened.
        None if matches!(report.stop, run::RunStop::WriteFailed { .. }) => {
            "No record completed — but the destination may have been changed.".to_string()
        }
        None if report.skipped() > 0 => {
            "Nothing new to do — every record was already processed.".to_string()
        }
        None => "Nothing was written.".to_string(),
    };

    RunSummaryView {
        playbook_id: playbook_id.to_string(),
        status: status.to_string(),
        headline,
        processed_range,
        written,
        skipped: report.skipped(),
        flagged: flagged_records.len(),
        corrected: report.corrected.clone(),
        needs_attention,
        flagged_records,
        stop_reason,
    }
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
        ..Default::default()
        });
    };
    let state_str = match active.control.state() {
        run::ControlState::Paused => run::RunState::Paused,
        run::ControlState::Stopped => run::RunState::Idle,
        run::ControlState::Running => run::RunState::Running,
    };
    // §4.5: what a supervised run has stopped on. `None` on an ordinary run,
    // which is what keeps the correction panel's one-off branch unavailable
    // unless there is genuinely a record to attach it to.
    let awaiting = active.supervision.awaiting();
    Ok(RunStatusView {
        playbook_id: Some(active.playbook_id.clone()),
        state: state_str.as_str().to_string(),
        finished: active.is_finished(),
        awaiting_row: awaiting.as_ref().map(|a| a.row_key.clone()),
        awaiting_missing_fields: awaiting
            .map(|a| a.missing_fields)
            .unwrap_or_default(),
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
            is_templated: p.is_templated,
            template_declined: p.template_declined,
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
        let (proposal, reason) = propose_template(
            &[link(1, "C2", "B2"), link(2, "C3", "B3"), link(3, "C4", "B4")],
            3,
        );
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
        let (proposal, reason) = propose_template(&[link(1, "C2", "B2"), link(2, "C3", "B3")], 2);
        assert!(proposal.is_none());
        let reason = reason.expect("a refusal must explain itself");
        assert!(reason.contains("three are needed"), "unhelpful: {reason}");
    }

    #[test]
    fn an_ordinary_recording_proposes_nothing_and_reports_no_problem() {
        // Nothing was copied between grids, so there was never a pattern to
        // look for. That is not a failed detection and must not read as one.
        let (proposal, reason) = propose_template(&[], 0);
        assert!(proposal.is_none());
        assert_eq!(reason, None, "an ordinary recording is not a refusal");
    }

    #[test]
    fn pastes_that_could_not_be_traced_to_a_cell_say_so_rather_than_going_quiet() {
        // The real failure this came from: six pastes out of a Google Doc into
        // a Sheet. No cell positions can be read from a document, so no source
        // links form -- and the review screen previously showed NOTHING, so the
        // user was left guessing why the pattern prompt never appeared.
        let (proposal, reason) = propose_template(&[], 6);
        assert!(proposal.is_none());
        let reason = reason.expect("six pastes and no links must be explained");
        assert!(reason.contains("6 pastes"), "should count them: {reason}");
        assert!(
            reason.contains("spreadsheet"),
            "should say what a source has to be: {reason}"
        );
    }

    #[test]
    fn one_untraceable_paste_reads_as_singular() {
        let (_, reason) = propose_template(&[], 1);
        let reason = reason.expect("explained");
        assert!(reason.contains("1 paste seen"), "{reason}");
    }

    #[test]
    fn a_still_source_is_reported_as_inconclusive_not_as_a_constant() {
        // §2 is explicit: a source that did not move is inconclusive, "not as
        // confirmation of a fixed, unchanging value".
        let (proposal, reason) = propose_template(
            &[link(1, "C2", "B2"), link(2, "C2", "B3"), link(3, "C2", "B4")],
            3,
        );
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
        let (proposal, _) = propose_template(&links, links.len());
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

    // ---------------- the shapes the frontend is typed against ----------------
    //
    // `src/components/templated-workflow/types.ts` and
    // `src/components/record-mode/types.ts` hand-mirror these structs. Nothing
    // generates one from the other, so the only thing keeping them in step is a
    // test that fails when a field is renamed or a tag changes.
    //
    // Not hypothetical: `CaptureSummary` had already drifted three fields out
    // of date on the TypeScript side before Section 6 started, and nothing
    // noticed because the frontend never read them.

    #[test]
    fn a_failed_write_never_reports_that_nothing_happened() {
        // The defect this pins, seen for real: a run typed into A2, failed
        // verification, and the summary said "Nothing was written." while the
        // cell had visibly changed. A user reads that and believes the sheet is
        // untouched, at the one moment that belief is most expensive.
        let report = run::RunReport {
            playbook_id: "pb".into(),
            stop: run::RunStop::WriteFailed {
                position: crate::source::SourcePosition {
                    source_id: "src".into(),
                    row_key: "2".into(),
                },
                cell: "A2".into(),
                wrote: Vec::new(),
                reason: "wrote \"X\" but the cell reads \"Y\"".into(),
            },
            records: Vec::new(),
            corrected: Vec::new(),
        };
        let v = summarise("pb", &run::background::RunOutcome::Finished(report));

        assert!(
            !v.headline.contains("Nothing was written"),
            "headline must not claim nothing happened: {}",
            v.headline
        );
        assert!(
            v.headline.contains("may have been changed"),
            "headline must warn the destination changed: {}",
            v.headline
        );
        let reason = v.stop_reason.expect("a failed write must explain itself");
        assert!(
            reason.contains("A2"),
            "must name the cell that may have been altered: {reason}"
        );
        assert!(v.needs_attention);
    }

    #[test]
    fn preview_outcome_serialises_as_a_kind_tagged_union() {
        // The riskiest shape in the contract: an internally-tagged enum whose
        // variant names are camelCased and whose payload is FLATTENED alongside
        // the tag rather than nested under it.
        let ready = PreviewOutcome::Ready(PreviewView {
            playbook_id: "pb".into(),
            source_row: "2".into(),
            destination_row: 2,
            fields: vec![PreviewFieldView {
                source_field: "C".into(),
                source_label: Some("Customer".into()),
                destination_field: "A".into(),
                destination_label: None,
                value: "Acme".into(),
            }],
            verdict: "looks sensible".into(),
            verdict_is_reassuring: true,
        });
        let v = serde_json::to_value(&ready).expect("serialise");
        assert_eq!(v["kind"], "ready");
        // Flattened, NOT nested: a `v["Ready"]` here would mean the TypeScript
        // intersection type is wrong.
        assert_eq!(v["playbook_id"], "pb");
        assert_eq!(v["source_row"], "2");
        assert_eq!(v["destination_row"], 2);
        assert_eq!(v["fields"][0]["source_field"], "C");
        assert_eq!(v["fields"][0]["source_label"], "Customer");
        assert!(v["fields"][0]["destination_label"].is_null());
        assert_eq!(v["verdict_is_reassuring"], true);

        let nothing = PreviewOutcome::NothingToDo(NothingToPreview {
            reason: "all done".into(),
            corrections: Vec::new(),
        });
        assert_eq!(
            serde_json::to_value(&nothing).expect("serialise")["kind"],
            "nothingToDo",
            "the TypeScript union matches on this exact string"
        );

        let attention = PreviewOutcome::NeedsAttention(NothingToPreview {
            reason: "a column moved".into(),
            corrections: vec![CorrectionRequestView {
                side: "destination".into(),
                old_locator: "A".into(),
                old_label: Some("Client".into()),
                best_guess: Some("D".into()),
                detail: "The destination column A was \"Client\"".into(),
            }],
        });
        assert_eq!(
            serde_json::to_value(&attention).expect("serialise")["kind"],
            "needsAttention"
        );
    }

    #[test]
    fn the_run_status_and_batch_views_keep_their_field_names() {
        let status = RunStatusView {
            playbook_id: Some("pb".into()),
            state: "running".into(),
            finished: false,
        ..Default::default()
        };
        let v = serde_json::to_value(&status).expect("serialise");
        assert_eq!(v["playbook_id"], "pb");
        assert_eq!(v["state"], "running");
        assert_eq!(v["finished"], false);

        let batch = NewBatchView {
            playbook_id: "pb".into(),
            has_work: true,
            count: 12,
            first_row: Some("45".into()),
            capped: false,
            message: "Found 12 new records starting at row 45. Run the workflow on these?".into(),
        };
        let v = serde_json::to_value(&batch).expect("serialise");
        assert_eq!(v["has_work"], true);
        assert_eq!(v["count"], 12);
        assert_eq!(v["first_row"], "45");
        assert_eq!(v["capped"], false);
        assert!(
            v["message"].as_str().unwrap().contains("starting at row 45"),
            "the batch message is rendered verbatim by the frontend"
        );
    }

    #[test]
    fn a_template_proposal_reaches_the_frontend_with_from_and_to() {
        // `MappedField` renames the pair to from/to for display. A frontend
        // typed against source_field/destination_field would render undefined
        // with no error anywhere.
        let proposal = TemplateProposal {
            source: "doc!Sheet1".into(),
            destination: "doc!Sheet2".into(),
            fields: vec![MappedField {
                from: "C".into(),
                to: "A".into(),
            }],
            source_step: 1,
            destination_step: 1,
            examples: 3,
        };
        let v = serde_json::to_value(&proposal).expect("serialise");
        assert_eq!(v["fields"][0]["from"], "C");
        assert_eq!(v["fields"][0]["to"], "A");
        assert_eq!(v["examples"], 3);
    }

    #[test]
    fn a_playbook_summary_says_whether_it_is_templated() {
        // Section 6 reuses one list for both kinds, so this is the field the
        // templated behaviour hangs off. It did not exist until Section 6
        // needed it.
        let view = PlaybookSummaryView {
            id: "pb".into(),
            name: "Invoices".into(),
            source: "record_mode".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            step_count: 3,
            irreversible_count: 0,
            is_templated: true,
            template_declined: false,
        };
        let v = serde_json::to_value(&view).expect("serialise");
        assert_eq!(v["is_templated"], true);
    }

    #[test]
    fn a_declined_proposal_is_distinguishable_from_no_proposal() {
        // The whole point of the field. Both of these are ordinary playbooks --
        // `is_templated` is false for each -- and before this they were the
        // same row, so "you were asked and said no" was unanswerable.
        let declined = PlaybookSummaryView {
            id: "pb".into(),
            name: "Invoices".into(),
            source: "record_mode".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            step_count: 3,
            irreversible_count: 0,
            is_templated: false,
            template_declined: true,
        };
        let never_offered = PlaybookSummaryView {
            template_declined: false,
            ..PlaybookSummaryView {
                id: "pb2".into(),
                name: "Invoices".into(),
                source: "record_mode".into(),
                created_at: "now".into(),
                updated_at: "now".into(),
                step_count: 3,
                irreversible_count: 0,
                is_templated: false,
                template_declined: true,
            }
        };

        let a = serde_json::to_value(&declined).expect("serialise");
        let b = serde_json::to_value(&never_offered).expect("serialise");
        assert_eq!(a["is_templated"], false, "a declined playbook is not templated");
        assert_eq!(b["is_templated"], false);
        assert_eq!(a["template_declined"], true);
        assert_eq!(b["template_declined"], false);
        assert_ne!(
            a["template_declined"], b["template_declined"],
            "the two ways of being an ordinary playbook must not look identical"
        );
    }
}
