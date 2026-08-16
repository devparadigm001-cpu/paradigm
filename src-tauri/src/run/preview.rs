//! §4.3's first-record safety check, and the gate it guards.
//!
//! > "Before committing to a full batch, the system shows the very next record
//! > it's about to write — real values, in the real destination — with a simple
//! > confirm/cancel. Catches a wrong mapping after one record instead of after
//! > twelve."
//!
//! ## The gate is a type, not a convention
//!
//! [`RunAuthorization`] has no public constructor and no public fields. The
//! only way to obtain one is [`Preview::accept`], and the only way to obtain a
//! [`Preview`] is to have actually read the record that is about to be written.
//! [`crate::run::background::spawn`] takes one by value.
//!
//! So "start a run without showing the preview" is not a discouraged path or a
//! code-review item -- it does not compile. That matters more here than
//! elsewhere: this check exists precisely because the alternative is writing
//! twelve wrong records, and a gate that a caller can forget is a gate that a
//! caller will eventually forget.
//!
//! The authorization is bound to a playbook AND a source position, and
//! `spawn` re-checks both. An acceptance for one workflow cannot start
//! another, and an acceptance for a record that has since been processed
//! cannot start a run that would now begin somewhere else.
//!
//! ## What the Qwen verdict does, and deliberately does not, do
//!
//! Item 4's check runs here -- this is the only point in the flow where a
//! reader is open on the source and nothing has been written yet, which is
//! what it needs to turn column letters into words.
//!
//! It is **advisory**. It does not block acceptance, and that is a measured
//! decision rather than a soft one: on qwen2.5-0.5b the verdict was recorded
//! answering "no" at 0.884 for a mapping that was correct, and the confidence
//! band across six real mappings was 0.861–0.898 whether the mapping was
//! sensible or nonsense (see `detect::verify`). A gate keyed to that signal
//! would block real work on a coin-flip. §4.3 asks for "a simple
//! confirm/cancel", and the human confirming is the gate; the verdict is
//! shown to inform them, which is what §4.1's "ask the user to re-record"
//! amounts to when the asking cannot be automated reliably.
//!
//! ## Privacy
//!
//! A [`Preview`] holds real cell values. That is the entire point -- §4.3 says
//! "real values, in the real destination". It is therefore transient in the
//! same way [`SourceRecord`] is: no `Serialize`, never written to the
//! database, and dropped when the preview is answered.

use rusqlite::Connection;

use crate::compile::CompiledTemplate;
use crate::db::DbError;
use crate::detect::verify::Verdict;
use crate::detect::Pattern;
use crate::source::{Advance, FieldRef, SourceError, SourcePosition, SourceReader, SourceShape};

/// One field of the record about to be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewField {
    pub source_field: String,
    /// The source's header for that column, when it has one.
    pub source_label: Option<String>,
    pub destination_field: String,
    pub destination_label: Option<String>,
    /// The real value. Transient -- see the module docs.
    pub value: String,
}

/// The record a run would write next, shown before it is written.
///
/// No `Serialize`, deliberately: this carries content, and a derive here would
/// make persisting it a one-line accident. The command layer builds a separate
/// display view.
#[derive(Debug, Clone)]
pub struct Preview {
    playbook_id: String,
    position: SourcePosition,
    destination_row: u64,
    fields: Vec<PreviewField>,
    verdict: Verdict,
}

impl Preview {
    pub fn position(&self) -> &SourcePosition {
        &self.position
    }
    pub fn destination_row(&self) -> u64 {
        self.destination_row
    }
    pub fn fields(&self) -> &[PreviewField] {
        &self.fields
    }
    pub fn verdict(&self) -> &Verdict {
        &self.verdict
    }
    pub fn playbook_id(&self) -> &str {
        &self.playbook_id
    }

    /// The user confirmed. §4.3's "confirm".
    ///
    /// Consumes the preview, so one acceptance cannot start two runs.
    pub fn accept(self) -> RunAuthorization {
        RunAuthorization {
            playbook_id: self.playbook_id,
            position: self.position,
        }
    }

    /// The user declined. §4.10: "cancels cleanly. Nothing activates."
    ///
    /// Exists as a named method rather than letting the caller drop the value,
    /// so that declining is a thing the code says rather than a thing it omits.
    /// Dropping still works and means the same; this is for readability at the
    /// call site.
    pub fn decline(self) {}
}

/// Proof that a preview was shown and confirmed.
///
/// Private fields, no constructor. See the module docs -- this is the gate.
#[derive(Debug, Clone)]
pub struct RunAuthorization {
    playbook_id: String,
    position: SourcePosition,
}

impl RunAuthorization {
    pub fn playbook_id(&self) -> &str {
        &self.playbook_id
    }
    pub fn position(&self) -> &SourcePosition {
        &self.position
    }

    /// Does this authorization actually cover the run about to start?
    ///
    /// Checked by `spawn` rather than trusted. A stale acceptance -- for a
    /// different workflow, or for a record that has since been processed --
    /// would otherwise start a run the user never saw a preview for, which is
    /// the same failure as having no gate at all.
    pub fn covers(&self, playbook_id: &str, position: &SourcePosition) -> bool {
        self.playbook_id == playbook_id && &self.position == position
    }
}

/// Why a preview could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error(transparent)]
    Db(#[from] DbError),

    #[error("could not read the source: {0}")]
    Source(#[from] SourceError),

    #[error("the template maps no fields, so there is nothing to preview")]
    NoFields,
}

/// What [`next_record`] found.
#[derive(Debug, Clone)]
pub enum Upcoming {
    /// A record is ready to be shown.
    Ready(Preview),
    /// Nothing left to process -- every record is already done, or the source
    /// is empty. Not an error: §4.8's "nothing new" is a normal answer.
    NothingToDo,
    /// §4.10's suspicious gap, found before anything was written.
    SuspiciousGap {
        position: SourcePosition,
        rows_with_data_below: usize,
    },
}

/// Read the record a run would write next, WITHOUT writing it.
///
/// Skips records already in the ledger, exactly as the run loop does -- a
/// preview showing a record the run would then skip would be showing the wrong
/// thing. The reader is left positioned on the record it previewed, so a run
/// started afterwards begins there.
///
/// `verdict` is supplied by the caller rather than computed here, because
/// producing one needs a loaded language model and this function is otherwise
/// pure enough to test against a fake source. See [`verdict_for`].
pub fn next_record(
    conn: &Connection,
    playbook_id: &str,
    template: &CompiledTemplate,
    reader: &mut dyn SourceReader,
    destination_row: u64,
    verdict: Verdict,
) -> Result<Upcoming, PreviewError> {
    if template.fields.is_empty() {
        return Err(PreviewError::NoFields);
    }

    let fields: Vec<FieldRef> = template
        .fields
        .iter()
        .map(|m| FieldRef {
            name: m.source_field.clone(),
            locator: m.source_field.clone(),
        })
        .collect();

    // Bounded by the same ceiling the run loop uses, for the same reason: a
    // reader that never reports Exhausted must not spin here either.
    for _ in 0..crate::run::MAX_RECORDS {
        let position = reader.position();

        match reader.peek(&fields)? {
            Advance::Exhausted => return Ok(Upcoming::NothingToDo),
            Advance::SuspiciousGap {
                rows_with_data_below,
            } => {
                return Ok(Upcoming::SuspiciousGap {
                    position,
                    rows_with_data_below,
                })
            }
            Advance::Record => {}
        }

        if crate::run::is_processed(conn, playbook_id, &position)? {
            for _ in 0..template.source_step.max(1) {
                reader.advance()?;
            }
            continue;
        }

        let record = reader.read(&fields)?;
        let shown = template
            .fields
            .iter()
            .map(|m| PreviewField {
                source_field: m.source_field.clone(),
                source_label: None,
                destination_field: m.destination_field.clone(),
                destination_label: None,
                value: record
                    .fields
                    .get(&m.source_field)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();

        return Ok(Upcoming::Ready(Preview {
            playbook_id: playbook_id.to_string(),
            position,
            destination_row,
            fields: shown,
            verdict,
        }));
    }

    Ok(Upcoming::NothingToDo)
}

/// Attach header labels to a preview's fields, for display.
///
/// Separate from [`next_record`] because reading the destination's header row
/// needs a second reader open on the destination, which a caller may not have.
/// A preview without labels is still a valid preview -- it shows real values in
/// real cells, which is what §4.3 asks for.
pub fn label_fields(
    preview: &mut Preview,
    source_shape: &SourceShape,
    destination_shape: &SourceShape,
) {
    let look_up = |shape: &SourceShape, locator: &str| -> Option<String> {
        shape
            .columns
            .iter()
            .find(|c| c.locator.eq_ignore_ascii_case(locator))
            .map(|c| c.label.clone())
            .filter(|l| !l.trim().is_empty())
    };
    for f in &mut preview.fields {
        f.source_label = look_up(source_shape, &f.source_field);
        f.destination_label = look_up(destination_shape, &f.destination_field);
    }
}

/// Run item 4's check over a template, using both sides' header rows.
///
/// The labels come from the two shapes because `verify` needs a word for every
/// locator in the mapping -- including the destination's, which the source's
/// header row cannot supply.
pub fn verdict_for(
    engine: &crate::labeling::LabelingEngine,
    template: &CompiledTemplate,
    source_shape: &SourceShape,
    destination_shape: &SourceShape,
) -> Result<Verdict, crate::labeling::LabelingError> {
    let pattern = Pattern {
        fields: template.fields.clone(),
        source_step: template.source_step,
        destination_step: template.destination_step,
        examples: template.examples,
    };

    let labels = |locator: &str| -> Option<String> {
        for shape in [source_shape, destination_shape] {
            if let Some(c) = shape
                .columns
                .iter()
                .find(|c| c.locator.eq_ignore_ascii_case(locator))
            {
                if !c.label.trim().is_empty() {
                    return Some(c.label.clone());
                }
            }
        }
        None
    };

    let (verdict, _completion) = crate::detect::verify::verify(engine, &pattern, &labels)?;
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::FieldMapping;
    use crate::source::{ColumnShape, SourceRecord};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    struct Rows {
        rows: Vec<Option<&'static str>>,
        cursor: usize,
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
            "Preview",
            &crate::compile::ReversibilityPolicy::placeholder(),
            &crate::labeling::RedactionPolicy::placeholder(),
        );
        crate::compile::store::store(&mut conn, &pb).expect("store");
        (dir, conn, pb.id)
    }

    fn unsure() -> Verdict {
        Verdict::Unsure {
            confidence: 0.0,
            reason: "no model in this test".into(),
        }
    }

    #[test]
    fn the_preview_shows_the_first_record_with_its_real_value() {
        let (_d, conn, id) = db();
        let mut reader = Rows {
            rows: vec![Some("Acme"), Some("Globex")],
            cursor: 0,
        };
        let up = next_record(&conn, &id, &template(), &mut reader, 2, unsure()).expect("preview");

        let Upcoming::Ready(p) = up else {
            panic!("expected a record, got {up:?}")
        };
        assert_eq!(p.position().row_key, "1");
        assert_eq!(p.destination_row(), 2);
        assert_eq!(p.fields().len(), 1);
        assert_eq!(p.fields()[0].value, "Acme");
        assert_eq!(p.fields()[0].destination_field, "A");
    }

    #[test]
    fn previewing_writes_nothing_and_marks_nothing() {
        // The whole point: this runs BEFORE committing to a batch.
        let (_d, conn, id) = db();
        let mut reader = Rows {
            rows: vec![Some("Acme")],
            cursor: 0,
        };
        next_record(&conn, &id, &template(), &mut reader, 2, unsure()).expect("preview");
        assert_eq!(
            crate::run::processed_count(&conn, &id, "src").expect("count"),
            0,
            "a preview must not touch the ledger"
        );
    }

    #[test]
    fn the_preview_skips_records_already_processed() {
        // Showing a record the run would then skip would be showing the wrong
        // thing entirely.
        let (_d, conn, id) = db();
        crate::run::mark_processed(
            &conn,
            &id,
            &SourcePosition {
                source_id: "src".into(),
                row_key: "1".into(),
            },
        )
        .expect("mark");

        let mut reader = Rows {
            rows: vec![Some("Acme"), Some("Globex")],
            cursor: 0,
        };
        let up = next_record(&conn, &id, &template(), &mut reader, 2, unsure()).expect("preview");
        let Upcoming::Ready(p) = up else {
            panic!("expected a record, got {up:?}")
        };
        assert_eq!(p.position().row_key, "2");
        assert_eq!(p.fields()[0].value, "Globex");
    }

    #[test]
    fn nothing_left_to_do_is_an_answer_not_an_error() {
        let (_d, conn, id) = db();
        let mut reader = Rows {
            rows: vec![],
            cursor: 0,
        };
        let up = next_record(&conn, &id, &template(), &mut reader, 2, unsure()).expect("preview");
        assert!(matches!(up, Upcoming::NothingToDo), "got {up:?}");
    }

    #[test]
    fn a_suspicious_gap_is_reported_before_anything_is_written() {
        let (_d, conn, id) = db();
        let mut reader = Rows {
            rows: vec![None, Some("Globex")],
            cursor: 0,
        };
        let up = next_record(&conn, &id, &template(), &mut reader, 2, unsure()).expect("preview");
        assert!(
            matches!(up, Upcoming::SuspiciousGap { .. }),
            "got {up:?}"
        );
    }

    #[test]
    fn an_authorization_covers_only_what_it_was_issued_for() {
        let (_d, conn, id) = db();
        let mut reader = Rows {
            rows: vec![Some("Acme")],
            cursor: 0,
        };
        let Upcoming::Ready(p) =
            next_record(&conn, &id, &template(), &mut reader, 2, unsure()).expect("preview")
        else {
            panic!("expected a record")
        };
        let here = p.position().clone();
        let auth = p.accept();

        assert!(auth.covers(&id, &here));
        assert!(
            !auth.covers("some-other-playbook", &here),
            "an acceptance must not start a different workflow"
        );
        assert!(
            !auth.covers(
                &id,
                &SourcePosition {
                    source_id: "src".into(),
                    row_key: "9".into()
                }
            ),
            "an acceptance must not cover a record the user never saw"
        );
    }

    #[test]
    fn labels_come_from_both_sides_headers() {
        let (_d, conn, id) = db();
        let mut reader = Rows {
            rows: vec![Some("Acme")],
            cursor: 0,
        };
        let Upcoming::Ready(mut p) =
            next_record(&conn, &id, &template(), &mut reader, 2, unsure()).expect("preview")
        else {
            panic!("expected a record")
        };

        let source = SourceShape {
            columns: vec![ColumnShape {
                locator: "C".into(),
                label: "Customer".into(),
            }],
        };
        let destination = SourceShape {
            columns: vec![ColumnShape {
                locator: "A".into(),
                label: "Client".into(),
            }],
        };
        label_fields(&mut p, &source, &destination);

        assert_eq!(p.fields()[0].source_label.as_deref(), Some("Customer"));
        assert_eq!(p.fields()[0].destination_label.as_deref(), Some("Client"));
    }

    #[test]
    fn a_template_with_no_fields_cannot_be_previewed() {
        let (_d, conn, id) = db();
        let mut t = template();
        t.fields.clear();
        let mut reader = Rows {
            rows: vec![Some("Acme")],
            cursor: 0,
        };
        let err = next_record(&conn, &id, &t, &mut reader, 2, unsure()).expect_err("must refuse");
        assert!(matches!(err, PreviewError::NoFields), "got {err:?}");
    }
}

/// Warn when a run is about to write over a destination it has never written.
///
/// ## The situation this catches
///
/// The ledger decides two things, and the second is easy to miss: which rows
/// are new, and — through `batch::resume_destination_row` — **where the
/// destination resumes**. That resume row is `first_row + processed * step`, so
/// an empty ledger resumes at `first_row`: the TOP of the destination.
///
/// A workflow with an empty ledger is therefore not merely going to redo work.
/// It is going to write over whatever is already there, from the beginning,
/// and it will do so silently. Measured: two playbooks ran once each over the
/// same five source rows into the same destination, and the destination
/// afterwards held five rows, not ten. Ten writes, five rows, no duplicate to
/// notice.
///
/// The common way to arrive here is not exotic. Re-recording a workflow makes a
/// NEW workflow with a new id and an empty ledger, in the same list under a
/// similar name — and deleting the old one takes its ledger with it, by design.
/// It happened twice in one hour to the same user. See
/// `docs/known-issues/deleting-a-workflow-silently-discards-its-ledger.md`.
///
/// ## Why BOTH conditions are required
///
/// An empty ledger alone is the normal state of every genuinely new workflow
/// writing into a genuinely empty destination — warning there would fire on the
/// happy path and teach the user to dismiss it. Occupied cells alone are normal
/// too: a workflow that has processed rows before is *expected* to have filled
/// its destination, and resumes past it.
///
/// Only the pair is suspicious: nothing recorded as done, yet something already
/// written where this run is about to start.
pub fn overwrite_warning(
    processed: usize,
    destination_row: u64,
    occupied: &[String],
) -> Option<String> {
    if processed > 0 || occupied.is_empty() {
        return None;
    }
    let cells = occupied
        .iter()
        .map(|c| format!("{c}{destination_row}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "This workflow has no record of processing anything, so it will start writing at row \
         {destination_row} — but {cells} already contains data. If this workflow was recorded \
         again, it starts over and will write OVER what is there rather than after it. Check \
         the destination before confirming."
    ))
}

#[cfg(test)]
mod overwrite_tests {
    use super::overwrite_warning;

    /// The measured scenario: nothing in the ledger, data already at the target.
    #[test]
    fn an_empty_ledger_over_occupied_cells_warns() {
        let warning = overwrite_warning(0, 2, &["A".into(), "B".into()])
            .expect("this is the case the warning exists for");
        assert!(warning.contains("A2, B2"), "it must name the cells: {warning}");
        assert!(warning.contains("row 2"));
        assert!(
            warning.contains("OVER"),
            "the overwrite is the point, not the redundant work: {warning}"
        );
    }

    /// A genuinely new workflow writing into a genuinely empty destination is
    /// the happy path, and must stay silent -- a warning here would fire on
    /// every first run and train the user to ignore it.
    #[test]
    fn a_fresh_workflow_with_an_empty_destination_is_silent() {
        assert_eq!(overwrite_warning(0, 2, &[]), None);
    }

    /// And a workflow that HAS processed rows is expected to have filled its
    /// destination; it resumes past that, so occupied cells are not a signal.
    #[test]
    fn an_established_workflow_is_silent_even_over_occupied_cells() {
        assert_eq!(overwrite_warning(5, 7, &["A".into()]), None);
        assert_eq!(overwrite_warning(1, 3, &["A".into(), "B".into()]), None);
    }
}
