//! §4.4's loop: read → map → write → mark done → check → repeat.
//!
//! This is what replay becomes for a templated workflow. It is a genuinely
//! different shape from `crate::replay`, and the difference is worth stating
//! plainly because it explains every decision below:
//!
//! | | `replay` | this |
//! |---|---|---|
//! | what it executes | a fixed list of recorded steps | one mapping, N times |
//! | how long it runs | known before it starts | until the source is exhausted |
//! | what it writes | the value that was recorded | a value read this run |
//! | on trouble | stop | *depends* -- see §4.4's two bullets |
//!
//! That last row is the substantive one. `replay` has a single failure
//! response, because a recorded step that cannot run is simply broken. A run
//! loop cannot work that way: a source of two hundred rows where row 47 is
//! missing an optional field must not be an all-or-nothing proposition.
//!
//! ## Why this is sync
//!
//! [`SourceReader`] is sync, deliberately (see its module docs), so the loop
//! is too. That is what makes the whole of §4.4 testable against fakes with no
//! spreadsheet, no desktop and no async runtime -- which matters, because the
//! loop's job is deciding *when to stop*, and every wrong answer there either
//! ends a run early or writes past the end of the data.
//!
//! ## What is durable
//!
//! Only [`SourcePosition`] -- source id and row key, no values. §3's rule, and
//! the same one `workflow_processed_rows` was built for. A [`SourceRecord`]
//! lives from its read to its write and is then dropped.

pub mod background;
pub mod batch;
pub mod control;
pub mod correction;
pub mod drift;
pub mod preview;
pub mod spreadsheet;
pub mod supervision;
pub mod surfaces;

use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

pub use control::{ControlState, RunControl};

use crate::compile::CompiledTemplate;
use crate::db::DbError;
use crate::source::{Advance, FieldRef, SourceError, SourcePosition, SourceReader, SourceRecord};

/// Where a run puts what it read.
///
/// Deliberately symmetric with [`SourceReader`]: it holds its own position and
/// advances it. That symmetry is not tidiness, it is §3 compliance -- the
/// destination's current row is runtime state, never a stored one. A template
/// records *"advance one row each run"* and has no column that could hold
/// *"and we got to row 48"*, so the starting row has to come from the surface
/// itself at run time. There is nowhere else it could legitimately come from.
pub trait DestinationWriter {
    /// Where the next write will land. Reported for the summary, not persisted.
    fn position(&self) -> String;

    /// Write one field of the current record.
    fn write(&mut self, field: &str, value: &str) -> Result<(), SourceError>;

    /// Move on by the template's destination step.
    fn advance(&mut self, step: i64) -> Result<(), SourceError>;

    /// The destination's own header row, for §4.5's drift check.
    ///
    /// Deliberately has no default implementation. A default returning "no
    /// columns" would let a writer opt out of drift detection by saying
    /// nothing, and the resulting run would look checked when it was not --
    /// which is worse than a writer that cannot answer, because it is
    /// indistinguishable from one that did.
    ///
    /// `columns` is what to look at and `header_row` is where, rather than
    /// state held on the writer: the caller knows which columns the mapping
    /// uses, and a writer that cached them would go stale the moment a
    /// correction repointed one.
    fn shape(
        &mut self,
        columns: &[String],
        header_row: u64,
    ) -> Result<crate::source::SourceShape, SourceError>;
}

/// How well one source record matched the shape the examples established.
///
/// §4.4 draws a line that a boolean cannot express: "doesn't fit the pattern at
/// all" stops the run, "missing a field the source normally has" does not. The
/// distinguishing question is whether there is anything to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordFit {
    /// Every mapped source field had a value.
    Complete,
    /// Some mapped fields were blank, but not all. §4.4: continue, because a
    /// blank cell is reversible -- "but log it clearly for the summary. Never
    /// silently skip without recording that it happened."
    MissingFields { fields: Vec<String> },
    /// Every mapped field was blank. There is nothing this record could
    /// contribute, so it does not fit the pattern at all.
    ///
    /// **When this is actually reachable**, which is narrower than it looks.
    /// `peek` classifies the same mapped columns by the same blank rule, so a
    /// reader that answers consistently can never produce it: if `peek` said
    /// `Record`, some mapped column held data, and this cannot then find them
    /// all blank. Reaching it means the source contradicted itself between the
    /// peek and the read -- someone edited the sheet mid-run, or a reader whose
    /// two answers disagree.
    ///
    /// It is kept, and kept distinct, precisely because that is a real thing to
    /// stop for: it is the difference between "the data ran out" and "the data
    /// moved under us", and the second is not something to write through.
    DoesNotFit,
}

/// Classify a record against the mapping. Pure, and exhaustively tested, for
/// the same reason `classify_row` is.
///
/// Blank is empty-or-whitespace, matching `classify_row`: a cell holding a
/// space is not data. A field the record does not carry at all counts as blank
/// rather than as an error -- a source that stopped producing a column is drift
/// (§4.5), and reporting it as a missing field is what surfaces it.
pub fn classify_fit(record: &SourceRecord, fields: &[FieldRef]) -> RecordFit {
    let missing: Vec<String> = fields
        .iter()
        .filter(|f| {
            record
                .fields
                .get(&f.name)
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        })
        .map(|f| f.name.clone())
        .collect();

    if missing.is_empty() {
        RecordFit::Complete
    } else if missing.len() == fields.len() {
        RecordFit::DoesNotFit
    } else {
        RecordFit::MissingFields { fields: missing }
    }
}

/// What one iteration of the loop did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Read, written, and marked done.
    Written { fit: RecordFit },
    /// Already in `workflow_processed_rows`, so it was not written again.
    /// §4.7's duplicate protection, and the reason it is persistent.
    AlreadyProcessed,
}

/// One record's line in the summary. Position only -- no values, ever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordReport {
    pub position: SourcePosition,
    pub destination: String,
    pub outcome: RecordOutcome,
}

/// Why the loop ended.
///
/// Every stop is named. §4.4 requires "stop, show the user exactly which record
/// and why", and a run that ended for an unstated reason cannot do that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStop {
    /// The source ran out. The only ordinary ending.
    Exhausted,
    /// §4.10's suspicious gap: blank mapped columns with data further down.
    /// Stopping is the whole point -- silently deciding the run is complete
    /// here is the failure this exists to prevent.
    SuspiciousGap {
        position: SourcePosition,
        rows_with_data_below: usize,
    },
    /// §4.4's first bullet: a record that does not fit the pattern at all.
    RecordDoesNotFit { position: SourcePosition },
    /// The source could not be read.
    SourceFailed {
        position: SourcePosition,
        reason: String,
    },
    /// The destination could not be written.
    ///
    /// Carries which fields *were* already written, because a partial record is
    /// materially different from an untouched one and the user has to know
    /// which they are looking at before they re-run.
    WriteFailed {
        position: SourcePosition,
        wrote: Vec<String>,
        reason: String,
    },
    /// The write succeeded but the row could not be marked as done.
    ///
    /// Its own case rather than folded into `WriteFailed`, because the hazard is
    /// the opposite one: the destination HAS the record and the database does
    /// not know it, so a re-run would write it twice. §4.7 exists to prevent
    /// exactly that, and a silent continue here would defeat it.
    MarkFailed {
        position: SourcePosition,
        reason: String,
    },
    /// The user stopped the run (§4.6). Permanent, and not a failure.
    ///
    /// `mid_record` says which of §4.6's two arms this was, because the user
    /// needs to know what happened to the record they interrupted:
    ///
    /// * `false` -- the stop landed between records. Everything read was
    ///   written and marked; nothing was in progress. This is also where a stop
    ///   pressed *during* a write arrives, after that record finished cleanly.
    /// * `true` -- the run was paused mid-record and then stopped, so that one
    ///   record was discarded. Any fields already written for it are still on
    ///   the destination and it is NOT marked processed, so re-running the
    ///   workflow will redo it from the start.
    ///
    /// `position` is the record the run had reached, which for `mid_record` is
    /// exactly the discarded one.
    Stopped {
        position: SourcePosition,
        mid_record: bool,
    },
    /// The run hit the iteration ceiling. A backstop against a reader that
    /// never reports `Exhausted`, not an expected ending.
    LimitReached { limit: usize },
}

impl RunStop {
    /// Did the run end the way a healthy run ends?
    pub fn is_clean(&self) -> bool {
        matches!(self, RunStop::Exhausted)
    }
}

/// The outcome of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub playbook_id: String,
    pub stop: RunStop,
    pub records: Vec<RecordReport>,
    /// Source rows that used a one-off correction (§4.5).
    pub corrected: Vec<String>,
}

impl RunReport {
    pub fn written(&self) -> usize {
        self.records
            .iter()
            .filter(|r| matches!(r.outcome, RecordOutcome::Written { .. }))
            .count()
    }

    pub fn skipped(&self) -> usize {
        self.records
            .iter()
            .filter(|r| r.outcome == RecordOutcome::AlreadyProcessed)
            .count()
    }

    /// The first and last source records actually written, for §4.9's
    /// "Processed orders 45–57".
    ///
    /// Written records only. Including skipped ones would report a range the
    /// run did not process, which is the opposite of what §4.9 wants the line
    /// for -- it "confirms it found the right starting point", and a range
    /// starting at a record this run deliberately skipped would confirm the
    /// wrong thing.
    ///
    /// `None` when nothing was written, which is a real outcome (everything
    /// already processed, or stopped before the first record) and not an error.
    pub fn processed_range(&self) -> Option<(String, String)> {
        let mut written = self
            .records
            .iter()
            .filter(|r| matches!(r.outcome, RecordOutcome::Written { .. }))
            .map(|r| r.position.row_key.clone());
        let first = written.next()?;
        let last = written.last().unwrap_or_else(|| first.clone());
        Some((first, last))
    }

    /// Records written despite a gap. §4.4: "log it clearly for the summary."
    pub fn incomplete(&self) -> Vec<&RecordReport> {
        self.records
            .iter()
            .filter(|r| {
                matches!(
                    &r.outcome,
                    RecordOutcome::Written {
                        fit: RecordFit::MissingFields { .. }
                    }
                )
            })
            .collect()
    }
}

/// A run that could not start.
///
/// Separate from [`RunStop`] on purpose: these mean nothing happened at all, so
/// there is no partial state to explain and nothing to re-run carefully.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Db(#[from] DbError),

    #[error("the template maps no fields, so there is nothing to run")]
    NoFields,

    /// The reader advances one position at a time, so a backwards run cannot be
    /// expressed. Refused loudly rather than run in the wrong direction: the
    /// schema permits a negative step (`CHECK (source_step <> 0)` only) and this
    /// is where that gap is caught.
    #[error("a source step of {0} runs backwards, which this reader cannot do")]
    BackwardsSource(i64),
}

/// Hard ceiling on iterations.
///
/// Not a real limit -- it is far above any plausible source -- but a loop whose
/// exit depends on an external surface reporting `Exhausted` needs a backstop
/// that does not rely on that surface behaving. A reader stuck on one row would
/// otherwise spin forever.
pub const MAX_RECORDS: usize = 100_000;

/// Has this workflow already taken this row from this source? (§4.7)
///
/// Keyed by all three of playbook, source and row, matching the table's UNIQUE
/// constraint -- §4.13: two workflows reading the same sheet keep separate
/// ledgers, and neither one's progress hides a row from the other.
pub fn is_processed(
    conn: &Connection,
    playbook_id: &str,
    position: &SourcePosition,
) -> Result<bool, DbError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workflow_processed_rows
          WHERE playbook_id = ?1 AND source_id = ?2 AND row_key = ?3",
        rusqlite::params![playbook_id, &position.source_id, &position.row_key],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Record a row as done, durably.
///
/// Plain `INSERT`, not `INSERT OR IGNORE`: the loop checks [`is_processed`]
/// first, so a conflict here means an assumption is wrong, and `OR IGNORE`
/// would also swallow a foreign-key failure -- silently not recording a row
/// that was written, which is precisely the state §4.7 exists to prevent.
pub fn mark_processed(
    conn: &Connection,
    playbook_id: &str,
    position: &SourcePosition,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO workflow_processed_rows (id, playbook_id, source_id, row_key)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            Uuid::new_v4().to_string(),
            playbook_id,
            &position.source_id,
            &position.row_key
        ],
    )?;
    Ok(())
}

/// Whether a workflow is executing, and whether the user has paused it.
///
/// §4.11's `run_state`, which migration 20260813000004 added and nothing wrote
/// until now. Orthogonal to `template_state`: a confirmed workflow can be idle,
/// running or paused, and the two answer different questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running,
    Paused,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Idle => "idle",
            RunState::Running => "running",
            RunState::Paused => "paused",
        }
    }

    /// Parse what the database holds. `None` for anything else, which the CHECK
    /// constraint should already make impossible -- reported rather than
    /// defaulted, because silently reading an unknown state as `Idle` would
    /// make a running workflow look startable.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "idle" => Some(RunState::Idle),
            "running" => Some(RunState::Running),
            "paused" => Some(RunState::Paused),
            _ => None,
        }
    }
}

/// Record that a workflow is idle, running or paused.
///
/// This is state the UI reads, so it is written when the user's intent arrives
/// rather than when the loop next notices -- a run told to pause between two
/// slow records should show as paused immediately, not once it gets there.
pub fn set_run_state(
    conn: &Connection,
    playbook_id: &str,
    state: RunState,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE playbooks SET run_state = ?2 WHERE id = ?1",
        rusqlite::params![playbook_id, state.as_str()],
    )?;
    Ok(())
}

/// Read a workflow's run state.
pub fn get_run_state(conn: &Connection, playbook_id: &str) -> Result<Option<RunState>, DbError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT run_state FROM playbooks WHERE id = ?1",
            [playbook_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.as_deref().and_then(RunState::parse))
}

/// How many rows this workflow has taken from a source. For the summary and for
/// §4.8's "nothing new" reporting.
pub fn processed_count(
    conn: &Connection,
    playbook_id: &str,
    source_id: &str,
) -> Result<usize, DbError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workflow_processed_rows
          WHERE playbook_id = ?1 AND source_id = ?2",
        rusqlite::params![playbook_id, source_id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// The fields a template reads, in the reader's terms.
///
/// For a spreadsheet the field IS the locator -- detection produced column
/// letters, and a column letter is both what the mapping calls the field and
/// where the reader finds it. Kept as a named conversion rather than inlined so
/// a source where those differ has one obvious place to diverge.
fn source_fields(template: &CompiledTemplate) -> Vec<FieldRef> {
    template
        .fields
        .iter()
        .map(|m| FieldRef {
            name: m.source_field.clone(),
            locator: m.source_field.clone(),
        })
        .collect()
}

/// Run a templated workflow to exhaustion. §4.4.
///
/// ## Order of operations, and one deliberate departure
///
/// §4.4 lists *write → mark done → check*. This checks the record's fit
/// **before** writing it. The literal order would have the loop write a record
/// it is about to declare unfit and stop on, which is the one outcome §4.4's
/// first bullet is trying to avoid -- "stop, show the user exactly which record
/// and why" reads as a stop *instead of* the write, not after it. Everything
/// else follows §4.4 exactly, and the write → mark → advance order in
/// particular is load-bearing: `SourceReader::advance` is documented as safe to
/// call only once the record is both written and recorded, so that a crash
/// between the two cannot skip a row forever.
///
/// The duplicate check happens before the read, not after: a row already
/// processed should not have its content pulled into memory at all.
///
/// Runs to exhaustion with no way to intervene. For a run the user can stop or
/// pause, see [`run_with_control`], which this delegates to.
pub fn run(
    conn: &Connection,
    playbook_id: &str,
    template: &CompiledTemplate,
    reader: &mut dyn SourceReader,
    writer: &mut dyn DestinationWriter,
) -> Result<RunReport, RunError> {
    run_with_control(
        conn,
        playbook_id,
        template,
        reader,
        writer,
        &RunControl::new(),
        &correction::RunCorrections::new(),
        &supervision::RunSupervision::off(),
    )
}

/// §4.4's loop, with §4.6's Stop and Pause.
///
/// ## Where the safe points are, and why they are where they are
///
/// The record is durable only once [`mark_processed`] has written it, and
/// neither the reader nor the writer advances until after that. Everything
/// §4.6 asks for falls out of that ordering rather than needing machinery:
///
/// **Pause, between records.** Nothing is in progress, so the loop simply
/// waits. Resuming carries on.
///
/// **Pause, mid-record.** Checked before each field write. The partial record
/// is abandoned -- not marked, and neither side advanced -- so resuming
/// re-reads the same position and rewrites every field from the top. That is
/// §4.6's "backs up to the start of whatever record was in progress and redoes
/// it cleanly from the beginning". The already-written fields are overwritten
/// with the same values, which is the harmless redo §4.6 says it prefers "over
/// any risk of a half-done state surviving a pause".
///
/// **Stop, during a normal run.** Checked at the top of each record, so a stop
/// arriving mid-record lets that record finish and be marked before the loop
/// exits. §4.6's "allowed to finish cleanly", and it leaves the destination and
/// the ledger agreeing with each other.
///
/// **Stop, while paused mid-record.** The waiting loop is released with a halt
/// and the abandoned record is never marked. §4.6's other arm, "fully
/// discarded" -- and the only one of the two that is actually achievable
/// mid-write, since undoing a write to a live spreadsheet is not something this
/// system can do. §4.10 makes the same choice explicitly for prior records: a
/// stopped run "keeps whatever it already successfully wrote".
///
/// So both arms of §4.6's Stop clause are real behaviours, reached by different
/// routes, rather than one being picked and the other written off.
pub fn run_with_control(
    conn: &Connection,
    playbook_id: &str,
    template: &CompiledTemplate,
    reader: &mut dyn SourceReader,
    writer: &mut dyn DestinationWriter,
    control: &RunControl,
    corrections_handle: &correction::RunCorrections,
    supervision: &supervision::RunSupervision,
) -> Result<RunReport, RunError> {
    if template.fields.is_empty() {
        return Err(RunError::NoFields);
    }
    if template.source_step < 0 {
        return Err(RunError::BackwardsSource(template.source_step));
    }

    let fields = source_fields(template);
    let mut records = Vec::new();
    // Which records used a one-off correction, for the summary. §4.5 wants the
    // distinction between a one-time exception and a permanent change to be
    // visible, and a correction that left no trace in the report would make a
    // corrected record indistinguishable from an ordinary one.
    let mut corrected: Vec<String> = Vec::new();

    let stop = loop {
        if records.len() >= MAX_RECORDS {
            break RunStop::LimitReached { limit: MAX_RECORDS };
        }

        let position = reader.position();

        // 0. The between-records safe point. Nothing is in progress here, so a
        //    pause just waits and a stop just ends -- and a stop that arrived
        //    while the previous record was being written lands here, which is
        //    what lets that record finish cleanly first.
        if !control.wait_while_paused() {
            break RunStop::Stopped {
                position,
                mid_record: false,
            };
        }

        // 1. Is there anything here? `peek` rather than `read` so "is the run
        //    over" never pulls content into memory to find out.
        match reader.peek(&fields) {
            Ok(Advance::Exhausted) => break RunStop::Exhausted,
            Ok(Advance::SuspiciousGap {
                rows_with_data_below,
            }) => {
                break RunStop::SuspiciousGap {
                    position,
                    rows_with_data_below,
                }
            }
            Ok(Advance::Record) => {}
            Err(e) => {
                break RunStop::SourceFailed {
                    position,
                    reason: e.to_string(),
                }
            }
        }

        // 2. §4.7. Before the read, so an already-processed row's content is
        //    never loaded at all.
        match is_processed(conn, playbook_id, &position) {
            Ok(true) => {
                records.push(RecordReport {
                    position: position.clone(),
                    destination: writer.position(),
                    outcome: RecordOutcome::AlreadyProcessed,
                });
                // The SOURCE advances; the destination does NOT.
                //
                // The destination position means "where the next write goes",
                // and skipping a record writes nothing, so moving it would
                // leave a gap the size of however many records were skipped.
                //
                // This was wrong until a live run caught it: a second batch
                // resumed the destination at row 5 (correct), then skipped the
                // three already-processed source rows and advanced the writer
                // three times along with them, so the new records landed at
                // rows 8 and 9 with three blank rows above them. Both halves
                // were individually defensible and together they double-counted
                // the same three records.
                if let Some(stop) = advance_source(reader, template, &position) {
                    break stop;
                }
                continue;
            }
            Ok(false) => {}
            Err(e) => return Err(e.into()),
        }

        // 2b. §4.5's one-off corrections, resolved for THIS record.
        //
        // The sequence is unchanged; only "which columns does this record
        // use" stops being a constant read off the template. A correction can
        // move a locator and nothing else -- not the order, not the fit check,
        // not the obligation to mark before advancing.
        //
        // Asked for by row key, so a correction for another record simply is
        // not returned. Not consumed here: the record is not done yet.
        let corrections = corrections_handle.for_record(&position.row_key);
        let effective = if corrections.is_empty() {
            template.fields.clone()
        } else {
            correction::apply(&template.fields, &corrections)
        };
        let read_fields: Vec<FieldRef> = effective
            .iter()
            .map(|m| FieldRef {
                name: m.source_field.clone(),
                locator: m.source_field.clone(),
            })
            .collect();

        // 3. Read. Transient from here to the write, then dropped.
        let record = match reader.read(&read_fields) {
            Ok(r) => r,
            Err(e) => {
                break RunStop::SourceFailed {
                    position,
                    reason: e.to_string(),
                }
            }
        };

        // 4. Check the fit -- before writing, per the note above. Against the
        //    EFFECTIVE fields: a corrected record must be judged by the columns
        //    it will actually read, or a correction that fixed a record would
        //    still be refused for not fitting the mapping it no longer uses.
        let fit = classify_fit(&record, &read_fields);
        if fit == RecordFit::DoesNotFit {
            break RunStop::RecordDoesNotFit { position };
        }

        // 4b. §4.5's mid-run correction point, only when the user asked for it.
        //
        // With supervision off -- the default -- none of this runs and §4.4's
        // "continue, but log it" is untouched. With it on, an incomplete record
        // pauses instead of being written blank, so the correction panel has a
        // record in hand to attach a one-off to.
        //
        // The pause is item 7's, not new machinery: `control.pause()` sets the
        // same state the Pause button sets, and resuming goes through the same
        // `wait_while_paused`. The redo is item 7's guarantee too -- `continue`
        // re-reads the record from the start, which is exactly how a correction
        // applied while paused takes effect.
        if let RecordFit::MissingFields { fields: missing } = &fit {
            if supervision.should_ask(&position.row_key) {
                // Pause FIRST, then announce what we are waiting on.
                //
                // The other order deadlocks, and did: anything watching for
                // `awaiting` can act the instant it is set, and a resume that
                // arrives before the pause exists is a no-op -- the loop then
                // pauses into a wait nobody will release. Establishing the
                // pause first makes the announcement the last thing that
                // happens, so any responder is acting on a state that is
                // already true.
                control.pause();
                supervision.begin(crate::run::supervision::AwaitingRecord {
                    row_key: position.row_key.clone(),
                    missing_fields: missing.clone(),
                });
                let carry_on = control.wait_while_paused();
                supervision.finish();
                if !carry_on {
                    break RunStop::Stopped {
                        position,
                        mid_record: true,
                    };
                }
                // Redo from the top. A correction entered while paused is
                // picked up on the re-read; if none was, the record is now
                // marked asked and will be written as it is, which is §4.4's
                // behaviour reached by the user's decision rather than by
                // default.
                continue;
            }
        }

        // 5. Write. A field the record does not carry is written blank rather
        //    than skipped: §4.4 calls this the reversible case, and leaving the
        //    destination cell holding whatever it held before would be neither
        //    reversible nor honest.
        let destination = writer.position();
        let mut wrote = Vec::new();
        let mut write_failure = None;
        let mut interrupted = None;
        for mapping in &effective {
            // The mid-record safe point. Asked cheaply and non-blockingly,
            // because on almost every field the answer is no.
            //
            // Only Pause is honoured here. A Stop is deliberately NOT checked
            // mid-record: letting the record finish is what makes it clean, and
            // §4.6 offers exactly that as one of its two arms.
            if control.is_paused() {
                if control.wait_while_paused() {
                    // Resumed. Abandon this attempt and redo the record from
                    // the top -- nothing was marked and neither side advanced,
                    // so `continue` re-reads this same position.
                    interrupted = Some(false);
                } else {
                    // Stopped while paused: the record is discarded entirely.
                    interrupted = Some(true);
                }
                break;
            }

            let value = record
                .fields
                .get(&mapping.source_field)
                .map(String::as_str)
                .unwrap_or("");
            match writer.write(&mapping.destination_field, value) {
                Ok(()) => wrote.push(mapping.destination_field.clone()),
                Err(e) => {
                    write_failure = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(reason) = write_failure {
            break RunStop::WriteFailed {
                position,
                wrote,
                reason,
            };
        }

        // A pause landed mid-record. The record is NOT marked either way, which
        // is what makes both outcomes clean: on resume it is redone from the
        // top, and on stop it is as though it never began.
        match interrupted {
            Some(true) => {
                break RunStop::Stopped {
                    position,
                    mid_record: true,
                }
            }
            Some(false) => continue,
            None => {}
        }

        // 6. Mark done, durably, BEFORE either side advances.
        if let Err(e) = mark_processed(conn, playbook_id, &position) {
            break RunStop::MarkFailed {
                position,
                reason: e.to_string(),
            };
        }

        // §4.5: the correction expires HERE, with the record it named, and not
        // a moment earlier. `mark_processed` succeeding is the same instant
        // §4.7 considers the record finished, so the two agree by construction.
        let expired = corrections_handle.expire(&position.row_key);
        if expired > 0 {
            corrected.push(position.row_key.clone());
        }

        records.push(RecordReport {
            position: position.clone(),
            destination,
            outcome: RecordOutcome::Written { fit },
        });

        // 7. Repeat.
        if let Some(stop) = advance_both(reader, writer, template, &position) {
            break stop;
        }
    };

    Ok(RunReport {
        playbook_id: playbook_id.to_string(),
        stop,
        records,
        corrected,
    })
}

/// Move both sides on by the template's steps.
///
/// The source advances one position at a time, `source_step` times, because
/// that is the only motion [`SourceReader`] exposes -- a step of 2 means every
/// other row, so it takes two moves to get there.
fn advance_source(
    reader: &mut dyn SourceReader,
    template: &CompiledTemplate,
    position: &SourcePosition,
) -> Option<RunStop> {
    for _ in 0..template.source_step {
        if let Err(e) = reader.advance() {
            return Some(RunStop::SourceFailed {
                position: position.clone(),
                reason: e.to_string(),
            });
        }
    }
    None
}

fn advance_both(
    reader: &mut dyn SourceReader,
    writer: &mut dyn DestinationWriter,
    template: &CompiledTemplate,
    position: &SourcePosition,
) -> Option<RunStop> {
    if let Some(stop) = advance_source(reader, template, position) {
        return Some(stop);
    }
    if let Err(e) = writer.advance(template.destination_step) {
        return Some(RunStop::WriteFailed {
            position: position.clone(),
            wrote: Vec::new(),
            reason: e.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{compile, ReversibilityPolicy};
    use crate::detect::FieldMapping;
    use crate::labeling::RedactionPolicy;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    /// A source with known content and no spreadsheet behind it.
    ///
    /// Fakes rather than a live surface on purpose: this loop's job is deciding
    /// when to stop, and the interesting cases -- a gap with data below it, a
    /// row that does not fit, a write that fails halfway -- are all ones a real
    /// spreadsheet would make laborious to stage and impossible to stage
    /// exactly. `SpreadsheetReader` is what proves the loop drives real Sheets;
    /// this is what proves the loop is correct.
    struct FakeReader {
        source_id: String,
        rows: Vec<BTreeMap<String, String>>,
        cursor: usize,
        fail_at: Option<usize>,
        /// Blank this row's mapped columns after `peek` has answered, staging a
        /// source edited underneath a running workflow.
        blank_after_peek: Option<usize>,
    }

    impl FakeReader {
        fn new(rows: Vec<Vec<(&str, &str)>>) -> Self {
            Self {
                source_id: "sheet-A".into(),
                rows: rows
                    .into_iter()
                    .map(|r| {
                        r.into_iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect()
                    })
                    .collect(),
                cursor: 0,
                fail_at: None,
                blank_after_peek: None,
            }
        }

        fn row(&self, i: usize) -> Option<&BTreeMap<String, String>> {
            self.rows.get(i)
        }
    }

    impl SourceReader for FakeReader {
        fn position(&self) -> SourcePosition {
            SourcePosition {
                source_id: self.source_id.clone(),
                // 1-based, like a spreadsheet row.
                row_key: (self.cursor + 1).to_string(),
            }
        }

        fn peek(&mut self, fields: &[FieldRef]) -> Result<Advance, SourceError> {
            if self.fail_at == Some(self.cursor) {
                return Err(SourceError::Unreachable("fake failure".into()));
            }
            let values = |row: &BTreeMap<String, String>| -> Vec<String> {
                fields
                    .iter()
                    .map(|f| row.get(&f.name).cloned().unwrap_or_default())
                    .collect()
            };
            let current = match self.row(self.cursor) {
                Some(r) => values(r),
                // Past the end is the same shape as a blank row with nothing
                // below it, which `classify_row` already calls Exhausted.
                None => return Ok(Advance::Exhausted),
            };
            let ahead: Vec<Vec<String>> = (self.cursor + 1..self.rows.len())
                .filter_map(|i| self.row(i))
                .map(values)
                .collect();
            let answer = crate::source::classify_row(&current, &ahead);
            // The edit lands after the answer is computed, so `peek` reports
            // what was there and the following `read` finds what is there now.
            if self.blank_after_peek == Some(self.cursor) {
                if let Some(row) = self.rows.get_mut(self.cursor) {
                    for v in row.values_mut() {
                        v.clear();
                    }
                }
            }
            Ok(answer)
        }

        fn read(&mut self, fields: &[FieldRef]) -> Result<SourceRecord, SourceError> {
            let position = self.position();
            let row = self
                .row(self.cursor)
                .ok_or_else(|| SourceError::PositionLost("past the end".into()))?;
            let mut out = BTreeMap::new();
            for f in fields {
                if let Some(v) = row.get(&f.name) {
                    out.insert(f.name.clone(), v.clone());
                }
            }
            Ok(SourceRecord {
                position,
                fields: out,
            })
        }

        fn advance(&mut self) -> Result<(), SourceError> {
            self.cursor += 1;
            Ok(())
        }

        fn shape(&mut self) -> Result<crate::source::SourceShape, SourceError> {
            Ok(crate::source::SourceShape { columns: vec![] })
        }
    }

    /// A destination that records what it was told to write.
    struct FakeWriter {
        row: i64,
        writes: Vec<(String, String, String)>,
        /// Fail on the Nth write call, to stage a partial record.
        fail_on_write: Option<usize>,
        calls: usize,
        /// Fire a control action from inside the Nth write, so a pause or stop
        /// lands at an exactly known point mid-record rather than at whatever
        /// moment a sleeping test thread happens to wake up.
        trigger: Option<(usize, Trigger)>,
        control: Option<RunControl>,
    }

    #[derive(Debug, Clone, Copy)]
    enum Trigger {
        Pause,
        Stop,
    }

    impl FakeWriter {
        fn new() -> Self {
            Self {
                row: 2,
                writes: Vec::new(),
                fail_on_write: None,
                calls: 0,
                trigger: None,
                control: None,
            }
        }

        /// Fire `what` from inside write call `nth`.
        fn triggering(mut self, nth: usize, what: Trigger, control: &RunControl) -> Self {
            self.trigger = Some((nth, what));
            self.control = Some(control.clone());
            self
        }
    }

    impl DestinationWriter for FakeWriter {
        fn position(&self) -> String {
            self.row.to_string()
        }

        fn write(&mut self, field: &str, value: &str) -> Result<(), SourceError> {
            self.calls += 1;
            if self.fail_on_write == Some(self.calls) {
                return Err(SourceError::Unreachable("destination closed".into()));
            }
            if let (Some((nth, what)), Some(control)) = (self.trigger, self.control.as_ref()) {
                if nth == self.calls {
                    match what {
                        Trigger::Pause => control.pause(),
                        Trigger::Stop => control.stop(),
                    }
                }
            }
            self.writes
                .push((self.row.to_string(), field.to_string(), value.to_string()));
            Ok(())
        }

        fn advance(&mut self, step: i64) -> Result<(), SourceError> {
            self.row += step;
            Ok(())
        }

        /// No headers. These tests record no shape either, so `drift::check`
        /// answers `NothingRecorded` and the run proceeds -- which is the
        /// correct behaviour for a workflow whose surfaces were never
        /// captured, and is what keeps every test above about the loop rather
        /// than about drift.
        fn shape(
            &mut self,
            _columns: &[String],
            _header_row: u64,
        ) -> Result<crate::source::SourceShape, SourceError> {
            Ok(crate::source::SourceShape { columns: vec![] })
        }
    }

    fn template(
        fields: &[(&str, &str)],
        source_step: i64,
        destination_step: i64,
    ) -> CompiledTemplate {
        CompiledTemplate {
            source_id: "sheet-A".into(),
            destination_id: "sheet-B".into(),
            source_step,
            destination_step,
            examples: 3,
            fields: fields
                .iter()
                .map(|(s, d)| FieldMapping {
                    source_field: s.to_string(),
                    destination_field: d.to_string(),
                })
                .collect(),
        }
    }

    fn a_playbook(conn: &mut Connection, label: &str, button: &str) -> String {
        let mut stream = crate::capture::CapturedStream::new(
            crate::capture::ExclusionList::from_patterns(["!never!"]),
        );
        stream.admit(crate::capture::ActionCandidate {
            kind: crate::capture::ActionKind::Click,
            identifiers: vec!["app.exe".into()],
            process_name: None,
            element_role: Some("Button".into()),
            element_name: Some(button.to_string()),
            payload: None,
            detail: None,
            timestamp_ms: 0,
        });
        let pb = compile(
            stream.actions(),
            label,
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        crate::compile::store::store(conn, &pb).expect("store playbook");
        pb.id
    }

    /// A real migrated, encrypted database with one real playbook in it, so the
    /// foreign key on `workflow_processed_rows` is satisfied by something that
    /// actually exists.
    fn db_with_playbook() -> (TempDir, Connection, String) {
        let dir = TempDir::new().expect("temp dir");
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        let mut conn = crate::db::open(&db_path, &key_path).expect("open encrypted db");
        let id = a_playbook(&mut conn, "Invoice run", "Next");
        (dir, conn, id)
    }

    // ---------- classify_fit, exhaustively ----------

    fn record(fields: &[(&str, &str)]) -> SourceRecord {
        SourceRecord {
            position: SourcePosition {
                source_id: "s".into(),
                row_key: "1".into(),
            },
            fields: fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn refs(names: &[&str]) -> Vec<FieldRef> {
        names
            .iter()
            .map(|n| FieldRef {
                name: n.to_string(),
                locator: n.to_string(),
            })
            .collect()
    }

    #[test]
    fn a_record_with_every_mapped_field_is_complete() {
        let fit = classify_fit(&record(&[("C", "Acme"), ("D", "99")]), &refs(&["C", "D"]));
        assert_eq!(fit, RecordFit::Complete);
    }

    #[test]
    fn a_record_missing_one_field_is_incomplete_not_unfit() {
        // Section 4.4's second bullet: continue, but say so.
        let fit = classify_fit(&record(&[("C", "Acme"), ("D", "  ")]), &refs(&["C", "D"]));
        assert_eq!(
            fit,
            RecordFit::MissingFields {
                fields: vec!["D".to_string()]
            }
        );
    }

    #[test]
    fn a_field_absent_entirely_counts_as_missing_not_as_an_error() {
        // A source that stopped producing a column is drift, and reporting it
        // as a missing field is what surfaces it rather than crashing on it.
        let fit = classify_fit(&record(&[("C", "Acme")]), &refs(&["C", "D"]));
        assert_eq!(
            fit,
            RecordFit::MissingFields {
                fields: vec!["D".to_string()]
            }
        );
    }

    #[test]
    fn a_record_with_nothing_in_any_mapped_field_does_not_fit() {
        let fit = classify_fit(&record(&[("C", ""), ("D", " ")]), &refs(&["C", "D"]));
        assert_eq!(fit, RecordFit::DoesNotFit);
    }

    // ---------- the loop ----------

    #[test]
    fn the_loop_reads_maps_writes_and_marks_every_record() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme"), ("D", "100")],
            vec![("C", "Globex"), ("D", "200")],
            vec![("C", "Initech"), ("D", "300")],
        ]);
        let mut writer = FakeWriter::new();

        let report = run(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(report.stop, RunStop::Exhausted);
        assert!(report.stop.is_clean());
        assert_eq!(report.written(), 3);
        assert_eq!(report.skipped(), 0);

        // The mapping was applied: source C -> destination A, D -> B, and the
        // destination advanced one row per record from its starting row.
        assert_eq!(
            writer.writes,
            vec![
                ("2".to_string(), "A".to_string(), "Acme".to_string()),
                ("2".to_string(), "B".to_string(), "100".to_string()),
                ("3".to_string(), "A".to_string(), "Globex".to_string()),
                ("3".to_string(), "B".to_string(), "200".to_string()),
                ("4".to_string(), "A".to_string(), "Initech".to_string()),
                ("4".to_string(), "B".to_string(), "300".to_string()),
            ]
        );

        // And every row is durably marked.
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 3);
    }

    #[test]
    fn rerunning_writes_nothing_a_second_time() {
        // Section 4.7's headline: "Re-running the workflow when nothing new
        // exists in the source should be recognized and reported plainly, not
        // silently do nothing and not silently reprocess everything."
        let (_dir, conn, id) = db_with_playbook();
        let rows = vec![
            vec![("C", "Acme"), ("D", "100")],
            vec![("C", "Globex"), ("D", "200")],
            vec![("C", "Initech"), ("D", "300")],
        ];
        let tpl = template(&[("C", "A"), ("D", "B")], 1, 1);

        let mut w1 = FakeWriter::new();
        run(&conn, &id, &tpl, &mut FakeReader::new(rows.clone()), &mut w1).expect("first run");
        assert_eq!(w1.writes.len(), 6);

        let mut w2 = FakeWriter::new();
        let second = run(&conn, &id, &tpl, &mut FakeReader::new(rows), &mut w2).expect("second run");

        assert_eq!(second.stop, RunStop::Exhausted);
        assert_eq!(second.written(), 0);
        assert_eq!(second.skipped(), 3, "every row should be recognised as done");
        assert!(
            w2.writes.is_empty(),
            "a re-run must not touch the destination"
        );
        // Still three. The skip path must not add ledger rows either.
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 3);
    }

    #[test]
    fn a_row_already_processed_is_skipped_while_the_rest_are_written() {
        let (_dir, conn, id) = db_with_playbook();
        // Stage row 2 as already done, as a half-finished earlier run would.
        mark_processed(
            &conn,
            &id,
            &SourcePosition {
                source_id: "sheet-A".into(),
                row_key: "2".into(),
            },
        )
        .expect("pre-mark");

        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme")],
            vec![("C", "Globex")],
            vec![("C", "Initech")],
        ]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(report.written(), 2);
        assert_eq!(report.skipped(), 1);

        // Rows, not just values. This test previously asserted only the values
        // and passed while the destination was advancing on skipped records --
        // which put a blank row in the middle of every resumed batch. A live
        // run caught it; the assertion that should have is this one.
        assert_eq!(
            writer.writes,
            vec![
                ("2".to_string(), "A".to_string(), "Acme".to_string()),
                ("3".to_string(), "A".to_string(), "Initech".to_string()),
            ],
            "a skipped record must not leave a gap in the destination"
        );
    }

    #[test]
    fn a_record_emptied_between_the_peek_and_the_read_stops_the_run() {
        // The only way `DoesNotFit` is genuinely reachable -- see its docs.
        // `peek` and `classify_fit` apply the same blank rule to the same
        // mapped columns, so a self-consistent reader can never produce it;
        // what produces it is the source changing underneath a running
        // workflow. Staged literally: row 2 reads as data when peeked and is
        // empty by the time it is read.
        //
        // Stopping is right. "The data ran out" and "the data moved under us"
        // are different, and the second is not something to write through.
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme")],
            vec![("C", "Globex")],
            vec![("C", "Initech")],
        ]);
        reader.blank_after_peek = Some(1);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(
            report.stop,
            RunStop::RecordDoesNotFit {
                position: SourcePosition {
                    source_id: "sheet-A".into(),
                    row_key: "2".into(),
                }
            },
            "the stop must name exactly which record, per 4.4"
        );
        assert_eq!(report.written(), 1, "only the good row before it");
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(values, vec!["Acme"], "the unfit record must not be written");
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 1);
    }

    #[test]
    fn a_record_missing_one_field_is_written_and_reported_not_skipped() {
        // 4.4: "continue (this is a reversible situation -- a blank cell isn't
        // damaging), but log it clearly for the summary. Never silently skip
        // without recording that it happened."
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme"), ("D", "100")],
            vec![("C", "Globex")], // D absent
            vec![("C", "Initech"), ("D", "300")],
        ]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(
            report.stop,
            RunStop::Exhausted,
            "a gap must not stop the run"
        );
        assert_eq!(report.written(), 3);

        let incomplete = report.incomplete();
        assert_eq!(incomplete.len(), 1, "the gap must reach the summary");
        assert_eq!(incomplete[0].position.row_key, "2");
        assert_eq!(
            incomplete[0].outcome,
            RecordOutcome::Written {
                fit: RecordFit::MissingFields {
                    fields: vec!["D".to_string()]
                }
            }
        );

        // The missing field is written blank, not left holding whatever was
        // there before -- that is what makes it reversible.
        assert!(writer
            .writes
            .contains(&("3".to_string(), "B".to_string(), String::new())));
    }

    #[test]
    fn a_suspicious_gap_stops_the_run_rather_than_calling_it_complete() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme")],
            vec![("C", "")],        // blank...
            vec![("C", "Initech")], // ...but data below it
        ]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(
            report.stop,
            RunStop::SuspiciousGap {
                position: SourcePosition {
                    source_id: "sheet-A".into(),
                    row_key: "2".into(),
                },
                rows_with_data_below: 1,
            }
        );
        assert!(!report.stop.is_clean());
        assert_eq!(report.written(), 1);
    }

    #[test]
    fn a_failed_write_stops_and_names_what_it_had_already_written() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme"), ("D", "100")]]);
        let mut writer = FakeWriter::new();
        writer.fail_on_write = Some(2); // first field lands, second does not

        let report = run(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        match &report.stop {
            RunStop::WriteFailed {
                position, wrote, ..
            } => {
                assert_eq!(position.row_key, "1");
                assert_eq!(
                    wrote,
                    &vec!["A".to_string()],
                    "a half-written record must say so"
                );
            }
            other => panic!("expected WriteFailed, got {other:?}"),
        }
        // The row must NOT be marked done: it was not fully written, and
        // marking it would lose it forever.
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 0);
    }

    #[test]
    fn a_write_that_cannot_be_marked_done_stops_immediately() {
        // The dangerous direction: the destination HAS the record and the
        // ledger does not know it. Staged with a playbook id that does not
        // exist, so the foreign key rejects the mark for real.
        let dir = TempDir::new().expect("temp dir");
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        let conn = crate::db::open(&db_path, &key_path).expect("open db");

        let mut reader = FakeReader::new(vec![vec![("C", "Acme")], vec![("C", "Globex")]]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            "no-such-playbook",
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        match &report.stop {
            RunStop::MarkFailed { position, .. } => assert_eq!(position.row_key, "1"),
            other => panic!("expected MarkFailed, got {other:?}"),
        }
        assert_eq!(
            writer.writes.len(),
            1,
            "it must not go on to the next record"
        );
        assert_eq!(
            report.written(),
            0,
            "an unmarked write is not a completed record"
        );
    }

    #[test]
    fn a_source_that_fails_mid_run_stops_and_names_the_position() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme")], vec![("C", "Globex")]]);
        reader.fail_at = Some(1);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        match &report.stop {
            RunStop::SourceFailed { position, reason } => {
                assert_eq!(position.row_key, "2");
                assert!(reason.contains("fake failure"), "reason: {reason}");
            }
            other => panic!("expected SourceFailed, got {other:?}"),
        }
        assert_eq!(report.written(), 1);
    }

    #[test]
    fn a_source_step_of_two_reads_every_other_row() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme")],
            vec![("C", "skip me")],
            vec![("C", "Initech")],
            vec![("C", "skip me too")],
        ]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 2, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(report.stop, RunStop::Exhausted);
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(values, vec!["Acme", "Initech"]);
    }

    #[test]
    fn a_destination_step_of_two_leaves_a_row_between_records() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme")], vec![("C", "Globex")]]);
        let mut writer = FakeWriter::new();
        run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 2),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        let rows: Vec<&str> = writer.writes.iter().map(|w| w.0.as_str()).collect();
        assert_eq!(rows, vec!["2", "4"]);
    }

    #[test]
    fn two_workflows_reading_the_same_source_keep_separate_ledgers() {
        // Section 4.13. The first workflow processing every row must not make
        // the second one think there is nothing to do.
        let (_dir, mut conn, first) = db_with_playbook();
        let second = a_playbook(&mut conn, "Second workflow", "Back");

        let rows = vec![vec![("C", "Acme")], vec![("C", "Globex")]];
        let tpl = template(&[("C", "A")], 1, 1);

        let mut w1 = FakeWriter::new();
        run(
            &conn,
            &first,
            &tpl,
            &mut FakeReader::new(rows.clone()),
            &mut w1,
        )
        .expect("first");
        assert_eq!(w1.writes.len(), 2);

        let mut w2 = FakeWriter::new();
        let r2 = run(&conn, &second, &tpl, &mut FakeReader::new(rows), &mut w2).expect("second");

        assert_eq!(r2.written(), 2, "the second workflow has its own ledger");
        assert_eq!(r2.skipped(), 0);
        assert_eq!(processed_count(&conn, &first, "sheet-A").expect("c"), 2);
        assert_eq!(processed_count(&conn, &second, "sheet-A").expect("c"), 2);
    }

    #[test]
    fn the_processed_range_covers_only_what_was_written() {
        // §4.9's line "confirms it found the right starting point". A range
        // that began at a record this run skipped would confirm the opposite.
        let (_dir, conn, id) = db_with_playbook();
        mark_processed(
            &conn,
            &id,
            &SourcePosition {
                source_id: "sheet-A".into(),
                row_key: "1".into(),
            },
        )
        .expect("pre-mark");

        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme")],
            vec![("C", "Globex")],
            vec![("C", "Initech")],
        ]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(report.skipped(), 1);
        assert_eq!(
            report.processed_range(),
            Some(("2".to_string(), "3".to_string())),
            "the skipped first record must not widen the range"
        );
    }

    #[test]
    fn a_run_that_wrote_nothing_has_no_range() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");
        assert_eq!(report.processed_range(), None);
    }

    #[test]
    fn a_single_written_record_reports_a_range_of_one() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme")]]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");
        assert_eq!(
            report.processed_range(),
            Some(("1".to_string(), "1".to_string())),
            "one record is a range from itself to itself, not None"
        );
    }

    #[test]
    fn an_empty_source_completes_immediately_without_writing() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![]);
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(report.stop, RunStop::Exhausted);
        assert_eq!(report.records.len(), 0);
        assert!(writer.writes.is_empty());
    }

    #[test]
    fn a_backwards_source_step_is_refused_before_anything_runs() {
        // The schema allows it (CHECK is only `<> 0`) and the reader cannot do
        // it, so this is where it has to be caught.
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme")]]);
        let mut writer = FakeWriter::new();
        let err = run(
            &conn,
            &id,
            &template(&[("C", "A")], -1, 1),
            &mut reader,
            &mut writer,
        )
        .expect_err("must refuse");

        assert!(matches!(err, RunError::BackwardsSource(-1)), "got {err:?}");
        assert!(writer.writes.is_empty(), "nothing may have happened");
    }

    // ---------------- §4.5 one-off corrections, in the loop ----------------

    fn one_off(row: &str, old: &str, new: &str) -> correction::OneOffCorrection {
        correction::OneOffCorrection {
            row_key: row.to_string(),
            side: drift::Side::Source,
            old_locator: old.to_string(),
            new_locator: new.to_string(),
        }
    }

    /// Rows carrying BOTH the mapped column and an alternative, so a correction
    /// has somewhere real to point.
    fn rows_with_an_alternative() -> Vec<Vec<(&'static str, &'static str)>> {
        vec![
            vec![("C", "Acme"), ("E", "ACME CORP")],
            vec![("C", "Globex"), ("E", "GLOBEX LTD")],
            vec![("C", "Initech"), ("E", "INITECH INC")],
        ]
    }

    #[test]
    fn a_one_off_correction_applies_to_exactly_one_record_and_no_other() {
        // The whole of §4.5's distinction, in one assertion: record 2 reads
        // from the corrected column, records 1 and 3 read from the mapping as
        // recorded. If a correction leaked forward, record 3 would say
        // "INITECH INC".
        let (_dir, conn, id) = db_with_playbook();
        let corrections = correction::RunCorrections::new();
        corrections.add(one_off("2", "C", "E"));

        let mut reader = FakeReader::new(rows_with_an_alternative());
        let mut writer = FakeWriter::new();
        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
            &RunControl::new(),
            &corrections,
            &supervision::RunSupervision::off(),
        )
        .expect("run");

        assert_eq!(report.stop, RunStop::Exhausted);
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(
            values,
            vec!["Acme", "GLOBEX LTD", "Initech"],
            "only record 2 should have used the corrected column"
        );

        // It is reported, so a corrected record is distinguishable from an
        // ordinary one in the summary.
        assert_eq!(report.corrected, vec!["2".to_string()]);
    }

    #[test]
    fn a_correction_expires_with_the_record_it_named() {
        let (_dir, conn, id) = db_with_playbook();
        let corrections = correction::RunCorrections::new();
        corrections.add(one_off("2", "C", "E"));
        assert_eq!(corrections.pending(), 1);

        let mut reader = FakeReader::new(rows_with_an_alternative());
        let mut writer = FakeWriter::new();
        run_with_control(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
            &RunControl::new(),
            &corrections,
            &supervision::RunSupervision::off(),
        )
        .expect("run");

        assert_eq!(
            corrections.pending(),
            0,
            "the correction must not survive the record it applied to"
        );
    }

    #[test]
    fn a_correction_for_a_record_the_run_never_reached_does_not_expire() {
        // Expiry is tied to the record being MARKED, not to the run ending. A
        // run stopped before record 3 leaves record 3's correction intact, so
        // the user does not have to enter it again.
        let (_dir, conn, id) = db_with_playbook();
        let corrections = correction::RunCorrections::new();
        corrections.add(one_off("3", "C", "E"));

        let control = RunControl::new();
        let mut reader = FakeReader::new(rows_with_an_alternative());
        // Stop during record 2, so record 3 is never processed.
        let mut writer = FakeWriter::new().triggering(2, Trigger::Stop, &control);
        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
            &control,
            &corrections,
            &supervision::RunSupervision::off(),
        )
        .expect("run");

        assert!(matches!(report.stop, RunStop::Stopped { .. }));
        assert_eq!(
            corrections.pending(),
            1,
            "record 3 was never processed, so its correction still applies"
        );
        assert!(report.corrected.is_empty());
    }

    #[test]
    fn a_correction_does_not_apply_to_a_record_already_processed() {
        // A skipped record is not re-read, so its correction is not consumed
        // either -- it would be wrong to silently discard a correction for a
        // record this run never touched.
        let (_dir, conn, id) = db_with_playbook();
        mark_processed(
            &conn,
            &id,
            &SourcePosition {
                source_id: "sheet-A".into(),
                row_key: "2".into(),
            },
        )
        .expect("pre-mark");

        let corrections = correction::RunCorrections::new();
        corrections.add(one_off("2", "C", "E"));

        let mut reader = FakeReader::new(rows_with_an_alternative());
        let mut writer = FakeWriter::new();
        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
            &RunControl::new(),
            &corrections,
            &supervision::RunSupervision::off(),
        )
        .expect("run");

        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(values, vec!["Acme", "Initech"]);
        assert_eq!(corrections.pending(), 1, "never reached, so never consumed");
        assert!(report.corrected.is_empty());
    }

    #[test]
    fn an_uncorrected_run_is_completely_unaffected() {
        // The regression that matters: this touches the loop every record goes
        // through. With no corrections the behaviour must be byte-for-byte what
        // it was.
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(rows_with_an_alternative());
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(report.stop, RunStop::Exhausted);
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(values, vec!["Acme", "Globex", "Initech"]);
        assert!(report.corrected.is_empty());
    }

    // ---------------- §4.5 opt-in supervision ----------------

    /// Row 2 is complete, row 3 is missing the mapped column but has the value
    /// in E, row 4 is complete again. The row-4 shape, once more: it is what
    /// distinguishes "corrected one record" from "changed the mapping".
    fn rows_with_one_odd_record() -> Vec<Vec<(&'static str, &'static str)>> {
        // TWO mapped columns, and that is load-bearing. With one, "missing"
        // and "blank" are the same thing: `peek` sees the only mapped column
        // empty and reports a SuspiciousGap, so the run stops before the
        // record is ever read and MissingFields is unreachable. Supervision
        // is only meaningful for a multi-field mapping.
        vec![
            vec![("C", "Acme"), ("D", "100"), ("E", "ACME CORP")],
            // C missing, D present -- peek sees data, the fit sees a gap.
            vec![("D", "200"), ("E", "GLOBEX LTD")],
            vec![("C", "Initech"), ("D", "300"), ("E", "INITECH INC")],
        ]
    }

    #[test]
    fn with_supervision_off_an_incomplete_record_is_written_blank_exactly_as_before() {
        // THE REGRESSION THAT MATTERS. §4.4: continue, write the gap blank, log
        // it. This is the default and every existing caller gets it.
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(rows_with_one_odd_record());
        let mut writer = FakeWriter::new();
        let report = run(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
        )
        .expect("run");

        assert_eq!(report.stop, RunStop::Exhausted);
        assert_eq!(report.written(), 3, "the incomplete record is still written");
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(
            values,
            vec!["Acme", "100", "", "200", "Initech", "300"],
            "the gap is written blank and the run carried on -- §4.4 exactly"
        );
        assert_eq!(report.incomplete().len(), 1, "and it is flagged");
    }

    #[test]
    fn with_supervision_on_an_incomplete_record_pauses_the_run() {
        // The pause is item 7's: the loop ends up in exactly the state the
        // Pause button produces, and the run is left waiting rather than ended.
        let (_dir, conn, id) = db_with_playbook();
        let control = RunControl::new();
        let supervision = supervision::RunSupervision::on();

        // Released from another thread once it is actually waiting, so the
        // test does not depend on timing.
        let releaser = {
            let c = control.clone();
            let s = supervision.clone();
            std::thread::spawn(move || {
                while s.awaiting().is_none() {
                    std::thread::yield_now();
                }
                // What the panel would read to open on this record.
                let waiting = s.awaiting().expect("awaiting");
                assert_eq!(waiting.row_key, "2");
                assert_eq!(waiting.missing_fields, vec!["C".to_string()]);
                c.resume();
            })
        };

        let mut reader = FakeReader::new(rows_with_one_odd_record());
        let mut writer = FakeWriter::new();
        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
            &control,
            &correction::RunCorrections::new(),
            &supervision,
        )
        .expect("run");
        releaser.join().expect("join");

        // Resumed without correcting, so the record is written as it is --
        // §4.4's outcome, reached by the user's decision.
        assert_eq!(report.stop, RunStop::Exhausted);
        assert_eq!(report.written(), 3);
        assert_eq!(supervision.awaiting(), None, "nothing left waiting");
    }

    #[test]
    fn a_correction_entered_while_supervised_fixes_that_record_and_no_other() {
        // The whole point, and the row-4 proof: correct row 2 while the run is
        // paused on it, resume, and row 3 must still read the mapped column.
        let (_dir, conn, id) = db_with_playbook();
        let control = RunControl::new();
        let supervision = supervision::RunSupervision::on();
        let corrections = correction::RunCorrections::new();

        let fixer = {
            let c = control.clone();
            let s = supervision.clone();
            let k = corrections.clone();
            std::thread::spawn(move || {
                while s.awaiting().is_none() {
                    std::thread::yield_now();
                }
                let waiting = s.awaiting().expect("awaiting");
                // Exactly what the panel does: a one-off for the record the
                // run stopped on.
                k.add(correction::OneOffCorrection {
                    row_key: waiting.row_key,
                    side: drift::Side::Source,
                    old_locator: "C".into(),
                    new_locator: "E".into(),
                });
                c.resume();
            })
        };

        let mut reader = FakeReader::new(rows_with_one_odd_record());
        let mut writer = FakeWriter::new();
        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
            &control,
            &corrections,
            &supervision,
        )
        .expect("run");
        fixer.join().expect("join");

        assert_eq!(report.stop, RunStop::Exhausted);
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(
            values,
            vec!["Acme", "100", "GLOBEX LTD", "200", "Initech", "300"],
            "row 2 used the corrected column; ROW 3 STILL USED THE ORIGINAL"
        );
        assert_eq!(report.corrected, vec!["2".to_string()]);
        assert!(
            report.incomplete().is_empty(),
            "the corrected record is no longer incomplete"
        );
    }

    #[test]
    fn a_template_with_no_fields_is_refused() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme")]]);
        let mut writer = FakeWriter::new();
        let err = run(&conn, &id, &template(&[], 1, 1), &mut reader, &mut writer)
            .expect_err("must refuse");
        assert!(matches!(err, RunError::NoFields), "got {err:?}");
    }

    // ---------------- §4.6 run controls ----------------

    /// Three records of two fields each, so a control can land *between* the
    /// two writes of one record and the mid-record path is really exercised.
    fn two_field_rows() -> Vec<Vec<(&'static str, &'static str)>> {
        vec![
            vec![("C", "Acme"), ("D", "100")],
            vec![("C", "Globex"), ("D", "200")],
            vec![("C", "Initech"), ("D", "300")],
        ]
    }

    #[test]
    fn a_stop_lets_the_record_being_written_finish_cleanly() {
        // §4.6: "Whatever record was actively being written when Stop is
        // pressed is either allowed to finish cleanly or fully discarded --
        // never left half-written." This is the first arm. The stop fires
        // during record 2's FIRST field, and record 2 must still come out whole.
        let (_dir, conn, id) = db_with_playbook();
        let control = RunControl::new();
        let mut reader = FakeReader::new(two_field_rows());
        let mut writer = FakeWriter::new().triggering(3, Trigger::Stop, &control);

        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
            &control,
            &correction::RunCorrections::new(),
            &supervision::RunSupervision::off(),
        )
        .expect("run");

        match &report.stop {
            RunStop::Stopped {
                position,
                mid_record,
            } => {
                assert!(!mid_record, "nothing was in progress when it stopped");
                assert_eq!(position.row_key, "3", "it stopped before record 3");
            }
            other => panic!("expected Stopped, got {other:?}"),
        }

        // Record 2 finished: BOTH its fields were written, not just the one
        // that was in flight when the stop arrived.
        assert_eq!(
            writer.writes,
            vec![
                ("2".to_string(), "A".to_string(), "Acme".to_string()),
                ("2".to_string(), "B".to_string(), "100".to_string()),
                ("3".to_string(), "A".to_string(), "Globex".to_string()),
                ("3".to_string(), "B".to_string(), "200".to_string()),
            ]
        );
        assert_eq!(report.written(), 2);
        // And the ledger agrees with the destination, which is the point of
        // finishing rather than abandoning.
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 2);
    }

    #[test]
    fn a_stop_while_paused_mid_record_discards_that_record_entirely() {
        // §4.6's other arm. Pause lands between record 2's two fields, then the
        // run is stopped while it waits.
        let (_dir, conn, id) = db_with_playbook();
        let control = RunControl::new();
        let mut reader = FakeReader::new(two_field_rows());
        let mut writer = FakeWriter::new().triggering(3, Trigger::Pause, &control);

        // Release the paused run with a stop rather than a resume. Waits until
        // the pause is actually observable, so the stop cannot land early.
        let stopper = {
            let c = control.clone();
            std::thread::spawn(move || {
                while !c.is_paused() {
                    std::thread::yield_now();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                c.stop();
            })
        };

        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
            &control,
            &correction::RunCorrections::new(),
            &supervision::RunSupervision::off(),
        )
        .expect("run");
        stopper.join().expect("join");

        match &report.stop {
            RunStop::Stopped {
                position,
                mid_record,
            } => {
                assert!(mid_record, "a record was in progress and was discarded");
                assert_eq!(position.row_key, "2", "it names the discarded record");
            }
            other => panic!("expected Stopped, got {other:?}"),
        }

        // Record 2's first field did reach the destination -- undoing a write
        // to a live sheet is not something this system can do, and §4.10 says a
        // stopped run keeps what it wrote.
        assert_eq!(writer.writes.len(), 3);
        // What matters is that it is NOT marked processed, so a re-run redoes
        // it from the start rather than skipping a half-written record forever.
        assert_eq!(report.written(), 1, "only record 1 completed");
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 1);
        assert!(
            !is_processed(
                &conn,
                &id,
                &SourcePosition {
                    source_id: "sheet-A".into(),
                    row_key: "2".into()
                }
            )
            .expect("check"),
            "a discarded record must never be marked done"
        );
    }

    #[test]
    fn a_pause_mid_record_redoes_that_record_from_the_beginning() {
        // §4.6: "backs up to the start of whatever record was in progress and
        // redoes it cleanly from the beginning -- never resumes mid-write."
        let (_dir, conn, id) = db_with_playbook();
        let control = RunControl::new();
        let mut reader = FakeReader::new(vec![
            vec![("C", "Acme"), ("D", "100")],
            vec![("C", "Globex"), ("D", "200")],
        ]);
        let mut writer = FakeWriter::new().triggering(3, Trigger::Pause, &control);

        let resumer = {
            let c = control.clone();
            std::thread::spawn(move || {
                while !c.is_paused() {
                    std::thread::yield_now();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                c.resume();
            })
        };

        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A"), ("D", "B")], 1, 1),
            &mut reader,
            &mut writer,
            &control,
            &correction::RunCorrections::new(),
            &supervision::RunSupervision::off(),
        )
        .expect("run");
        resumer.join().expect("join");

        assert_eq!(report.stop, RunStop::Exhausted, "a pause must not end a run");
        assert_eq!(report.written(), 2);

        // The redo is visible and is the whole point: record 2's first field
        // was written, the pause landed, and on resume the record was written
        // again from its FIRST field -- not continued from its second.
        assert_eq!(
            writer.writes,
            vec![
                ("2".to_string(), "A".to_string(), "Acme".to_string()),
                ("2".to_string(), "B".to_string(), "100".to_string()),
                ("3".to_string(), "A".to_string(), "Globex".to_string()),
                ("3".to_string(), "A".to_string(), "Globex".to_string()),
                ("3".to_string(), "B".to_string(), "200".to_string()),
            ],
            "record 2 should be rewritten from its first field on resume"
        );

        // Redone, not double-counted: the harmless rewrite must not become a
        // second ledger row, which the UNIQUE constraint would reject anyway.
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 2);
    }

    #[test]
    fn a_pause_between_records_waits_and_then_carries_on() {
        // The simple case: nothing is in progress, so there is nothing to redo.
        let (_dir, conn, id) = db_with_playbook();
        let control = RunControl::new();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme")], vec![("C", "Globex")]]);
        // One field per record, so the trigger fires on the last write of
        // record 1 and the pause is seen at the top of record 2.
        let mut writer = FakeWriter::new().triggering(1, Trigger::Pause, &control);

        let resumer = {
            let c = control.clone();
            std::thread::spawn(move || {
                while !c.is_paused() {
                    std::thread::yield_now();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                c.resume();
            })
        };

        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
            &control,
            &correction::RunCorrections::new(),
            &supervision::RunSupervision::off(),
        )
        .expect("run");
        resumer.join().expect("join");

        assert_eq!(report.stop, RunStop::Exhausted);
        assert_eq!(report.written(), 2);
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(values, vec!["Acme", "Globex"], "no record was redone");
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 2);
    }

    #[test]
    fn a_run_stopped_before_it_starts_writes_nothing() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(two_field_rows());
        let mut writer = FakeWriter::new();

        let report = run_with_control(
            &conn,
            &id,
            &template(&[("C", "A")], 1, 1),
            &mut reader,
            &mut writer,
            &RunControl::stopped(),
            &correction::RunCorrections::new(),
            &supervision::RunSupervision::off(),
        )
        .expect("run");

        assert!(matches!(report.stop, RunStop::Stopped { .. }));
        assert!(writer.writes.is_empty());
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 0);
    }

    #[test]
    fn a_stopped_run_can_be_resumed_by_rerunning_and_skips_what_it_finished() {
        // Stop is permanent for the RUN, not for the workflow: §4.7's ledger is
        // what makes picking up again safe, and this is the two features
        // meeting. The second run must not rewrite records the first completed.
        let (_dir, conn, id) = db_with_playbook();
        let control = RunControl::new();
        let tpl = template(&[("C", "A"), ("D", "B")], 1, 1);

        let mut w1 = FakeWriter::new().triggering(3, Trigger::Stop, &control);
        let first = run_with_control(
            &conn,
            &id,
            &tpl,
            &mut FakeReader::new(two_field_rows()),
            &mut w1,
            &control,
            &correction::RunCorrections::new(),
            &supervision::RunSupervision::off(),
        )
        .expect("first run");
        assert!(matches!(first.stop, RunStop::Stopped { .. }));
        assert_eq!(first.written(), 2);

        let mut w2 = FakeWriter::new();
        let second = run_with_control(
            &conn,
            &id,
            &tpl,
            &mut FakeReader::new(two_field_rows()),
            &mut w2,
            &RunControl::new(),
            &correction::RunCorrections::new(),
            &supervision::RunSupervision::off(),
        )
        .expect("second run");

        assert_eq!(second.stop, RunStop::Exhausted);
        assert_eq!(second.written(), 1, "only the record the stop never reached");
        assert_eq!(second.skipped(), 2, "the two the first run completed");
        let values: Vec<&str> = w2.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(values, vec!["Initech", "300"]);
        assert_eq!(processed_count(&conn, &id, "sheet-A").expect("count"), 3);
    }
}

