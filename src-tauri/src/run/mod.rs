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

use rusqlite::Connection;
use uuid::Uuid;

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
pub fn run(
    conn: &Connection,
    playbook_id: &str,
    template: &CompiledTemplate,
    reader: &mut dyn SourceReader,
    writer: &mut dyn DestinationWriter,
) -> Result<RunReport, RunError> {
    if template.fields.is_empty() {
        return Err(RunError::NoFields);
    }
    if template.source_step < 0 {
        return Err(RunError::BackwardsSource(template.source_step));
    }

    let fields = source_fields(template);
    let mut records = Vec::new();

    let stop = loop {
        if records.len() >= MAX_RECORDS {
            break RunStop::LimitReached { limit: MAX_RECORDS };
        }

        let position = reader.position();

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
                if let Some(stop) = advance_both(reader, writer, template, &position) {
                    break stop;
                }
                continue;
            }
            Ok(false) => {}
            Err(e) => return Err(e.into()),
        }

        // 3. Read. Transient from here to the write, then dropped.
        let record = match reader.read(&fields) {
            Ok(r) => r,
            Err(e) => {
                break RunStop::SourceFailed {
                    position,
                    reason: e.to_string(),
                }
            }
        };

        // 4. Check the fit -- before writing, per the note above.
        let fit = classify_fit(&record, &fields);
        if fit == RecordFit::DoesNotFit {
            break RunStop::RecordDoesNotFit { position };
        }

        // 5. Write. A field the record does not carry is written blank rather
        //    than skipped: §4.4 calls this the reversible case, and leaving the
        //    destination cell holding whatever it held before would be neither
        //    reversible nor honest.
        let destination = writer.position();
        let mut wrote = Vec::new();
        let mut write_failure = None;
        for mapping in &template.fields {
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

        // 6. Mark done, durably, BEFORE either side advances.
        if let Err(e) = mark_processed(conn, playbook_id, &position) {
            break RunStop::MarkFailed {
                position,
                reason: e.to_string(),
            };
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
    })
}

/// Move both sides on by the template's steps.
///
/// The source advances one position at a time, `source_step` times, because
/// that is the only motion [`SourceReader`] exposes -- a step of 2 means every
/// other row, so it takes two moves to get there.
fn advance_both(
    reader: &mut dyn SourceReader,
    writer: &mut dyn DestinationWriter,
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
    }

    impl FakeWriter {
        fn new() -> Self {
            Self {
                row: 2,
                writes: Vec::new(),
                fail_on_write: None,
                calls: 0,
            }
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
            self.writes
                .push((self.row.to_string(), field.to_string(), value.to_string()));
            Ok(())
        }

        fn advance(&mut self, step: i64) -> Result<(), SourceError> {
            self.row += step;
            Ok(())
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
        let values: Vec<&str> = writer.writes.iter().map(|w| w.2.as_str()).collect();
        assert_eq!(values, vec!["Acme", "Initech"], "Globex was already done");
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

    #[test]
    fn a_template_with_no_fields_is_refused() {
        let (_dir, conn, id) = db_with_playbook();
        let mut reader = FakeReader::new(vec![vec![("C", "Acme")]]);
        let mut writer = FakeWriter::new();
        let err = run(&conn, &id, &template(&[], 1, 1), &mut reader, &mut writer)
            .expect_err("must refuse");
        assert!(matches!(err, RunError::NoFields), "got {err:?}");
    }
}

