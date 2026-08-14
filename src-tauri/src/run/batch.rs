//! §4.8's new-batch detection.
//!
//! > "The system watches the known source for new, unprocessed rows and offers
//! > a quick confirmation before running -- *'Found 12 new rows starting at row
//! > 45. Run the workflow on these?'* -- combining automatic detection with the
//! > brief, deliberate confirmation the user asked for, rather than either
//! > fully automatic (no chance to catch a mistake) or fully manual (defeats
//! > the point)."
//!
//! ## This does not start runs, and that is the point
//!
//! A batch confirmation and the §4.3 first-record preview answer different
//! questions. This one is about *scope* -- there are twelve new rows, do you
//! want them done. The preview is about *correctness* -- here is what the first
//! one will actually write, is that right.
//!
//! So answering yes here leads **into** the preview, not around it. There is no
//! function in this module that produces a [`RunAuthorization`], and no way to
//! reach `background::spawn` from a batch scan without going through
//! `preview::next_record` and `Preview::accept` like everything else. A batch
//! notification that could start a run directly would be a second, weaker
//! entry point, and the gate would be worth exactly as much as its weakest
//! entry.
//!
//! [`RunAuthorization`]: crate::run::preview::RunAuthorization
//!
//! ## Why the scan is bounded
//!
//! Every `peek` on a live spreadsheet is a real navigation -- roughly a second
//! per row. An unbounded scan of a sheet with two thousand new rows would take
//! half an hour before it could say anything. So the scan stops at a limit and
//! *says* it stopped, rather than reporting a number it did not finish
//! counting. "At least 200 new rows" is honest; "200 new rows" when there are
//! nine hundred is not.

use rusqlite::Connection;

use crate::compile::CompiledTemplate;
use crate::db::DbError;
use crate::source::{Advance, FieldRef, SourceError, SourcePosition, SourceReader};

/// How many rows a single scan will look at before reporting back.
///
/// Chosen against the cost of a `peek`, not picked round: at roughly a second
/// per row this is a few minutes in the worst case, and the worst case only
/// happens when there genuinely is a large new batch -- which is exactly when
/// the user would rather be told "at least this many" quickly than an exact
/// figure slowly.
pub const SCAN_LIMIT: usize = 200;

/// What a scan found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchScan {
    /// Every record in the source has already been processed.
    ///
    /// §4.7: "Re-running the workflow when nothing new exists in the source
    /// should be recognized and reported plainly, not silently do nothing."
    /// This is that recognition.
    NothingNew,
    /// New records are waiting.
    Found {
        /// How many unprocessed records the scan counted.
        count: usize,
        /// Where they start -- §4.8 asks for "starting at row 45", and a count
        /// without a location is not something a user can check.
        first_row: String,
        /// The scan hit [`SCAN_LIMIT`] and stopped, so `count` is a floor
        /// rather than a total.
        capped: bool,
    },
    /// §4.10's suspicious gap, hit while scanning. Reported rather than treated
    /// as the end of the source, for the same reason the run loop does.
    SuspiciousGap {
        position: SourcePosition,
        rows_with_data_below: usize,
    },
}

impl BatchScan {
    pub fn has_work(&self) -> bool {
        matches!(self, BatchScan::Found { .. })
    }

    /// The sentence §4.8 asks for.
    pub fn describe(&self) -> String {
        match self {
            BatchScan::NothingNew => {
                "No new records -- everything in the source has already been processed"
                    .to_string()
            }
            BatchScan::Found {
                count,
                first_row,
                capped,
            } => {
                let n = if *capped {
                    format!("At least {count}")
                } else {
                    format!("Found {count}")
                };
                let plural = if *count == 1 { "record" } else { "records" };
                format!("{n} new {plural} starting at row {first_row}. Run the workflow on these?")
            }
            BatchScan::SuspiciousGap {
                position,
                rows_with_data_below,
            } => format!(
                "Row {} is blank in the mapped columns but {rows_with_data_below} row(s) below \
                 it still have data, so this may not be the end of the source",
                position.row_key
            ),
        }
    }
}

/// Why a scan could not run.
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("could not read the source: {0}")]
    Source(#[from] SourceError),
    #[error("the template maps no fields, so there is nothing to scan for")]
    NoFields,
}

/// Look for unprocessed records in the source.
///
/// Counts only records the ledger does not already have, which is what makes
/// this answer "new" rather than "present". Already-processed records are
/// stepped over without reading their content -- the same order the run loop
/// and the preview use, and for the same reason: a row this workflow has
/// already done should not have its values pulled into memory to find that out.
///
/// The reader is left wherever the scan stopped. Callers that want to run
/// afterwards should open a fresh one; the run loop skips processed records on
/// its own, so it does not need this one's position.
pub fn scan(
    conn: &Connection,
    playbook_id: &str,
    template: &CompiledTemplate,
    reader: &mut dyn SourceReader,
) -> Result<BatchScan, BatchError> {
    scan_with_limit(conn, playbook_id, template, reader, SCAN_LIMIT)
}

/// [`scan`] with an explicit ceiling, so tests do not have to stage 200 rows to
/// exercise the capped path.
pub fn scan_with_limit(
    conn: &Connection,
    playbook_id: &str,
    template: &CompiledTemplate,
    reader: &mut dyn SourceReader,
    limit: usize,
) -> Result<BatchScan, BatchError> {
    if template.fields.is_empty() {
        return Err(BatchError::NoFields);
    }

    let fields: Vec<FieldRef> = template
        .fields
        .iter()
        .map(|m| FieldRef {
            name: m.source_field.clone(),
            locator: m.source_field.clone(),
        })
        .collect();

    let mut count = 0usize;
    let mut first_row: Option<String> = None;
    // Rows LOOKED AT, not rows counted. A scan that steps over 300 already-done
    // rows has done 300 navigations regardless of finding nothing new, and the
    // ceiling exists to bound the work, not the answer.
    let mut examined = 0usize;

    loop {
        if examined >= limit {
            return Ok(match first_row {
                Some(first_row) => BatchScan::Found {
                    count,
                    first_row,
                    capped: true,
                },
                // Stopped while still stepping over processed rows, so nothing
                // new was seen -- but the source was not exhausted either, and
                // saying "nothing new" would be a claim the scan never reached.
                None => BatchScan::Found {
                    count: 0,
                    first_row: reader.position().row_key,
                    capped: true,
                },
            });
        }
        examined += 1;

        let position = reader.position();
        match reader.peek(&fields)? {
            Advance::Exhausted => break,
            Advance::SuspiciousGap {
                rows_with_data_below,
            } => {
                return Ok(BatchScan::SuspiciousGap {
                    position,
                    rows_with_data_below,
                })
            }
            Advance::Record => {}
        }

        if !crate::run::is_processed(conn, playbook_id, &position)? {
            if first_row.is_none() {
                first_row = Some(position.row_key.clone());
            }
            count += 1;
        }

        for _ in 0..template.source_step.max(1) {
            reader.advance()?;
        }
    }

    Ok(match first_row {
        Some(first_row) => BatchScan::Found {
            count,
            first_row,
            capped: false,
        },
        None => BatchScan::NothingNew,
    })
}

/// Where the destination should carry on writing.
///
/// ## Why this is derived rather than stored
///
/// §3 keeps durable data structural, and "we got to row 48" is a row index --
/// exactly what the template schema has no column for. But the ledger already
/// records *how many* records this workflow has taken from this source, and a
/// count is not an index. So the resume row falls out of two things that are
/// both legitimately durable: the count, and the advancement rule.
///
/// ## The assumption it makes, stated
///
/// That the destination started empty at `first_row` and that this workflow is
/// the only thing writing to it. Both hold for the case §4.8 describes -- a
/// workflow that owns its destination and appends to it -- and neither is
/// checked here. A destination someone else has also been writing into would
/// resume in the wrong place, which is why the §4.3 preview shows the target
/// cell before anything is written: that is the check.
pub fn resume_destination_row(
    conn: &Connection,
    playbook_id: &str,
    source_id: &str,
    first_row: u64,
    destination_step: i64,
) -> Result<u64, DbError> {
    let done = crate::run::processed_count(conn, playbook_id, source_id)? as i64;
    let offset = done.saturating_mul(destination_step);
    Ok(((first_row as i64) + offset).max(1) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::FieldMapping;
    use crate::source::{SourceRecord, SourceShape};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    struct Rows {
        rows: Vec<Option<&'static str>>,
        cursor: usize,
    }

    impl Rows {
        fn new(rows: Vec<Option<&'static str>>) -> Self {
            Self { rows, cursor: 0 }
        }
    }

    impl SourceReader for Rows {
        fn position(&self) -> SourcePosition {
            SourcePosition {
                source_id: "src".into(),
                row_key: (self.cursor + 1).to_string(),
            }
        }
        fn peek(&mut self, _: &[FieldRef]) -> Result<Advance, SourceError> {
            match self.rows.get(self.cursor) {
                None => Ok(Advance::Exhausted),
                Some(Some(_)) => Ok(Advance::Record),
                Some(None) => {
                    let below = self.rows[self.cursor + 1..]
                        .iter()
                        .filter(|r| r.is_some())
                        .count();
                    Ok(if below == 0 {
                        Advance::Exhausted
                    } else {
                        Advance::SuspiciousGap {
                            rows_with_data_below: below,
                        }
                    })
                }
            }
        }
        fn read(&mut self, _: &[FieldRef]) -> Result<SourceRecord, SourceError> {
            let v = self
                .rows
                .get(self.cursor)
                .and_then(|r| *r)
                .ok_or_else(|| SourceError::PositionLost("past the end".into()))?;
            let mut fields = BTreeMap::new();
            fields.insert("C".to_string(), v.to_string());
            Ok(SourceRecord {
                position: self.position(),
                fields,
            })
        }
        fn advance(&mut self) -> Result<(), SourceError> {
            self.cursor += 1;
            Ok(())
        }
        fn shape(&mut self) -> Result<SourceShape, SourceError> {
            Ok(SourceShape { columns: vec![] })
        }
    }

    fn template() -> CompiledTemplate {
        CompiledTemplate {
            source_id: "src".into(),
            destination_id: "dst".into(),
            source_step: 1,
            destination_step: 1,
            examples: 3,
            fields: vec![FieldMapping {
                source_field: "C".into(),
                destination_field: "A".into(),
            }],
        }
    }

    fn db() -> (TempDir, Connection, String) {
        let dir = TempDir::new().expect("temp dir");
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        let mut conn = crate::db::open(&db_path, &key_path).expect("open");
        let mut stream = crate::capture::CapturedStream::new(
            crate::capture::ExclusionList::from_patterns(["!never!"]),
        );
        stream.admit(crate::capture::ActionCandidate {
            kind: crate::capture::ActionKind::Click,
            identifiers: vec!["app.exe".into()],
            process_name: None,
            element_role: Some("Button".into()),
            element_name: Some("Next".into()),
            payload: None,
            detail: None,
            timestamp_ms: 0,
        });
        let pb = crate::compile::compile(
            stream.actions(),
            "Batch",
            &crate::compile::ReversibilityPolicy::placeholder(),
            &crate::labeling::RedactionPolicy::placeholder(),
        )
        .with_template(template());
        crate::compile::store::store(&mut conn, &pb).expect("store");
        (dir, conn, pb.id)
    }

    fn mark(conn: &Connection, id: &str, rows: &[&str]) {
        for r in rows {
            crate::run::mark_processed(
                conn,
                id,
                &SourcePosition {
                    source_id: "src".into(),
                    row_key: r.to_string(),
                },
            )
            .expect("mark");
        }
    }

    #[test]
    fn an_untouched_source_is_all_new() {
        let (_d, conn, id) = db();
        let mut r = Rows::new(vec![Some("a"), Some("b"), Some("c")]);
        let scan = scan(&conn, &id, &template(), &mut r).expect("scan");
        assert_eq!(
            scan,
            BatchScan::Found {
                count: 3,
                first_row: "1".into(),
                capped: false
            }
        );
    }

    #[test]
    fn only_the_records_not_already_processed_are_counted() {
        // The heart of §4.8: the ledger is what makes "new" mean new.
        let (_d, conn, id) = db();
        mark(&conn, &id, &["1", "2", "3"]);
        let mut r = Rows::new(vec![
            Some("a"),
            Some("b"),
            Some("c"),
            Some("d"),
            Some("e"),
        ]);
        let scan = scan(&conn, &id, &template(), &mut r).expect("scan");
        assert_eq!(
            scan,
            BatchScan::Found {
                count: 2,
                first_row: "4".into(),
                capped: false
            },
            "the two added rows, starting where they actually start"
        );
        assert!(scan.describe().contains("starting at row 4"), "{}", scan.describe());
    }

    #[test]
    fn a_fully_processed_source_reports_nothing_new_rather_than_zero_found() {
        // §4.7 wants this "recognized and reported plainly", which a
        // `Found { count: 0 }` would not be -- the UI would have to know that
        // zero means something different from the other Founds.
        let (_d, conn, id) = db();
        mark(&conn, &id, &["1", "2"]);
        let mut r = Rows::new(vec![Some("a"), Some("b")]);
        let scan = scan(&conn, &id, &template(), &mut r).expect("scan");
        assert_eq!(scan, BatchScan::NothingNew);
        assert!(!scan.has_work());
        assert!(scan.describe().contains("No new records"), "{}", scan.describe());
    }

    #[test]
    fn an_empty_source_reports_nothing_new() {
        let (_d, conn, id) = db();
        let mut r = Rows::new(vec![]);
        assert_eq!(
            scan(&conn, &id, &template(), &mut r).expect("scan"),
            BatchScan::NothingNew
        );
    }

    #[test]
    fn a_gap_in_the_middle_counts_only_what_precedes_it_and_says_why() {
        // Scanning past a suspicious gap would count rows the run itself would
        // refuse to reach, so the two must agree about where the source ends.
        let (_d, conn, id) = db();
        let mut r = Rows::new(vec![Some("a"), None, Some("c")]);
        let scan = scan(&conn, &id, &template(), &mut r).expect("scan");
        assert!(
            matches!(scan, BatchScan::SuspiciousGap { .. }),
            "got {scan:?}"
        );
        assert!(scan.describe().contains("may not be the end"), "{}", scan.describe());
    }

    #[test]
    fn a_capped_scan_says_at_least_rather_than_a_number_it_did_not_finish() {
        let (_d, conn, id) = db();
        let mut r = Rows::new(vec![Some("a"), Some("b"), Some("c"), Some("d")]);
        let scan = scan_with_limit(&conn, &id, &template(), &mut r, 2).expect("scan");
        assert_eq!(
            scan,
            BatchScan::Found {
                count: 2,
                first_row: "1".into(),
                capped: true
            }
        );
        assert!(scan.describe().starts_with("At least 2"), "{}", scan.describe());
    }

    #[test]
    fn the_ceiling_bounds_work_not_findings() {
        // 3 processed rows then 1 new one, with a ceiling of 3: the scan spends
        // its whole budget stepping over done rows and never reaches the new
        // one. It must not claim there is nothing new.
        let (_d, conn, id) = db();
        mark(&conn, &id, &["1", "2", "3"]);
        let mut r = Rows::new(vec![Some("a"), Some("b"), Some("c"), Some("d")]);
        let scan = scan_with_limit(&conn, &id, &template(), &mut r, 3).expect("scan");
        assert_eq!(
            scan,
            BatchScan::Found {
                count: 0,
                first_row: "4".into(),
                capped: true
            },
            "a scan that ran out of budget must not report NothingNew"
        );
    }

    #[test]
    fn one_record_reads_as_singular() {
        let (_d, conn, id) = db();
        mark(&conn, &id, &["1"]);
        let mut r = Rows::new(vec![Some("a"), Some("b")]);
        let scan = scan(&conn, &id, &template(), &mut r).expect("scan");
        assert!(
            scan.describe().contains("1 new record starting"),
            "{}",
            scan.describe()
        );
    }

    #[test]
    fn the_destination_resumes_after_what_was_already_written() {
        let (_d, conn, id) = db();
        assert_eq!(
            resume_destination_row(&conn, &id, "src", 2, 1).expect("resume"),
            2,
            "nothing processed yet, so it starts where it was told to"
        );
        mark(&conn, &id, &["1", "2", "3"]);
        assert_eq!(
            resume_destination_row(&conn, &id, "src", 2, 1).expect("resume"),
            5,
            "three written, so the fourth goes below them"
        );
    }

    #[test]
    fn a_destination_step_of_two_resumes_twice_as_far_down() {
        let (_d, conn, id) = db();
        mark(&conn, &id, &["1", "2"]);
        assert_eq!(
            resume_destination_row(&conn, &id, "src", 2, 2).expect("resume"),
            6
        );
    }

    #[test]
    fn a_template_with_no_fields_cannot_be_scanned() {
        let (_d, conn, id) = db();
        let mut t = template();
        t.fields.clear();
        let mut r = Rows::new(vec![Some("a")]);
        let err = scan(&conn, &id, &t, &mut r).expect_err("must refuse");
        assert!(matches!(err, BatchError::NoFields), "got {err:?}");
    }
}
