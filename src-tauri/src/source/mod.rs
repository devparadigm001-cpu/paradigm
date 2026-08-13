//! The source-reader interface: what a templated workflow reads records from.
//!
//! Backend item 2 of the templated-workflows design. Section 3 calls this the
//! one place the "spreadsheets first" scoping actually changes how something
//! gets built rather than just how it is described:
//!
//! > pattern detection, mapping storage, and replay should never talk to "a
//! > spreadsheet" directly. They talk to "a source," through a single interface
//! > (get the current position, read the next record, check for exhaustion,
//! > detect drift) that the spreadsheet reader implements first.
//!
//! The point is that a second source type -- a folder, an inbox -- is later a
//! matter of writing a new reader, not rewriting detection, mapping and replay.
//!
//! ## Why this trait is synchronous
//!
//! `terminator`'s element operations are sync (`text`, `set_value`, `press_key`);
//! only `Locator::first`/`all` are async, and those are needed once, to resolve
//! a handle -- `replay::grid_type` resolves the Name Box exactly that way. So a
//! reader does its async work when it is CONSTRUCTED and is sync thereafter.
//!
//! That keeps the trait object-safe, so a workflow can hold a
//! `Box<dyn SourceReader>` chosen from stored configuration at runtime, which is
//! how a second source type gets selected without generics threaded through
//! every caller. Async fns in traits would forfeit that, and `async-trait` is
//! not a dependency of this project -- adding one to work around a constraint
//! that construction-time resolution already removes would be the wrong trade.
//!
//! ## Privacy: values are transient, positions are durable
//!
//! Section 3 resolves a real, previously-refused question. Durable data is
//! **structural** -- "row 47: done" -- while actual source *content* is handled
//! only in memory, during detection and during each run.
//!
//! [`SourceRecord`] carries content, so it is deliberately awkward to persist:
//! it derives no `Serialize`, and its `Debug` prints field NAMES with their
//! values elided. A record is something to write to a destination and drop, not
//! something to log or store. What does get stored is [`SourcePosition`], which
//! carries no values at all -- see `workflow_processed_rows` in migration
//! 20260813000004.

use std::collections::BTreeMap;
use std::fmt;

/// Where a source is positioned, with no content attached.
///
/// This is the durable half: it is exactly what `workflow_processed_rows`
/// records, and it is safe to store precisely because it names a location and
/// nothing about what is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePosition {
    /// The source as a whole -- a spreadsheet document and sheet, a folder, a
    /// mailbox. Opaque to everything above this interface.
    pub source_id: String,
    /// The record within that source. For a spreadsheet this is the row.
    pub row_key: String,
}

/// One field the mapping reads from a source.
///
/// `name` is what the field means ("Customer Name"); `locator` is where the
/// reader finds it, in whatever terms that source uses (a spreadsheet column
/// like `"C"`). Detection and mapping deal in names; only the reader
/// interprets the locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRef {
    pub name: String,
    pub locator: String,
}

/// One record read from a source. **Transient. Never persist this.**
///
/// See the module docs: content lives in memory for the length of one write and
/// is then dropped. The missing `Serialize` and the eliding `Debug` are the
/// structural half of that rule; they make casual persistence and casual
/// logging both take a deliberate act.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub position: SourcePosition,
    /// Field name -> value, ordered so two reads of the same record compare
    /// equal regardless of the order fields were requested in.
    pub fields: BTreeMap<String, String>,
}

impl fmt::Debug for SourceRecord {
    /// Names and count only. A `Debug` that printed values would put source
    /// content into any log line that formatted a record, which is exactly the
    /// durable content trail Section 3 refuses.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceRecord")
            .field("position", &self.position)
            .field("fields", &self.fields.keys().collect::<Vec<_>>())
            .field("values", &format_args!("<{} elided>", self.fields.len()))
            .finish()
    }
}

/// What the reader found at the current position.
///
/// Three outcomes, not two, because 4.10 draws a distinction that a boolean
/// cannot: a gap in the mapped columns is only *conclusive* if there is nothing
/// beyond it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advance {
    /// There is a record here to read.
    Record,
    /// The mapped columns are empty here and stay empty. The run is complete.
    Exhausted,
    /// The mapped columns are empty here, but there is more data further down
    /// in those same columns. 4.10: "treated as suspicious rather than
    /// conclusive -- stop and ask rather than silently deciding the run is
    /// complete."
    SuspiciousGap { rows_with_data_below: usize },
}

/// The shape of a source, for drift comparison (4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceShape {
    pub columns: Vec<ColumnShape>,
}

/// One column's identity: where it is, and what it calls itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnShape {
    pub locator: String,
    pub label: String,
}

/// A way the source stopped looking like it did when the workflow was recorded.
///
/// 4.5 names two shapes explicitly -- "a new column added, columns reordered"
/// -- and both are represented distinctly, because the correction prompt reads
/// differently for each: a moved column has a plausible best guess to offer, a
/// vanished one does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    /// Same place, different label. The column that held customer names does
    /// not look like customer names any more.
    LabelChanged {
        locator: String,
        was: String,
        now: String,
    },
    /// Same label, different place -- the reordering case. This is the one that
    /// can offer "Looks like column D now?".
    Moved {
        label: String,
        was: String,
        now: String,
    },
    /// A label the recording knew is gone entirely.
    Missing { locator: String, label: String },
    /// A label the recording never saw. Harmless on its own, but 4.5 treats a
    /// new column as a shape change worth asking about, because it is how a
    /// reorder usually announces itself.
    Added { locator: String, label: String },
}

/// Decide what is at the current position, from the mapped columns' values.
///
/// Pure on purpose. This is the rule 4.10 specifies, and keeping it out of the
/// reader means it can be tested exhaustively without a live spreadsheet --
/// which matters, because every wrong answer here either ends a run early or
/// runs it off the end of the data.
///
/// `current` is the value of each mapped column at this position; `ahead` is
/// the same for each position scanned beyond it. Empty and whitespace-only are
/// both blank: a cell holding a space is not data.
///
/// Note what is deliberately NOT considered: columns the mapping does not read.
/// 4.10 is explicit that a stray formatting gap in an unrelated column must not
/// be mistaken for the end of the data, so unmapped columns never reach here.
pub fn classify_row(current: &[String], ahead: &[Vec<String>]) -> Advance {
    let has_data = |row: &[String]| row.iter().any(|v| !v.trim().is_empty());

    if has_data(current) {
        return Advance::Record;
    }
    let below = ahead.iter().filter(|row| has_data(row)).count();
    if below == 0 {
        Advance::Exhausted
    } else {
        Advance::SuspiciousGap {
            rows_with_data_below: below,
        }
    }
}

/// Compare a recorded shape against the current one (4.5).
///
/// Pure, for the same reason as [`classify_row`]: the comparison is where the
/// correctness lives, and it should be testable without a spreadsheet in front
/// of it.
///
/// A column whose label moved is reported as `Moved` rather than as a
/// `Missing` plus an `Added`, because that pairing is what lets the correction
/// panel offer a best guess instead of an open question.
pub fn detect_drift(recorded: &SourceShape, current: &SourceShape) -> Vec<Drift> {
    let mut drifts = Vec::new();

    for was in &recorded.columns {
        match current.columns.iter().find(|c| c.locator == was.locator) {
            // Same place, same label: no drift from this column.
            Some(now) if now.label == was.label => {}
            // Same place, different label -- unless that label simply moved.
            Some(now) => {
                if let Some(moved_to) = current
                    .columns
                    .iter()
                    .find(|c| c.label == was.label && c.locator != was.locator)
                {
                    drifts.push(Drift::Moved {
                        label: was.label.clone(),
                        was: was.locator.clone(),
                        now: moved_to.locator.clone(),
                    });
                } else {
                    drifts.push(Drift::LabelChanged {
                        locator: was.locator.clone(),
                        was: was.label.clone(),
                        now: now.label.clone(),
                    });
                }
            }
            // The place is gone. If the label turned up elsewhere it moved;
            // otherwise it is genuinely missing.
            None => {
                if let Some(moved_to) = current.columns.iter().find(|c| c.label == was.label) {
                    drifts.push(Drift::Moved {
                        label: was.label.clone(),
                        was: was.locator.clone(),
                        now: moved_to.locator.clone(),
                    });
                } else {
                    drifts.push(Drift::Missing {
                        locator: was.locator.clone(),
                        label: was.label.clone(),
                    });
                }
            }
        }
    }

    // A column is an addition when its LABEL is new -- a new place holding a
    // label the recording already knew is a move, and is reported above.
    //
    // The exception is a locator already reported as `LabelChanged`. There the
    // old label did not go anywhere, it was replaced in place, and that single
    // fact is the whole story; also calling it an addition would double-report
    // one change as two. Contrast an insertion, where the old label MOVED
    // elsewhere and the label now sitting in its place is genuinely additional
    // -- which is the case that first exposed this, when keying additions on an
    // unknown locator missed it entirely.
    let relabelled_in_place: Vec<String> = drifts
        .iter()
        .filter_map(|d| match d {
            Drift::LabelChanged { locator, .. } => Some(locator.clone()),
            _ => None,
        })
        .collect();

    for now in &current.columns {
        let known_label = recorded.columns.iter().any(|c| c.label == now.label);
        let already_explained = relabelled_in_place.contains(&now.locator);
        if !known_label && !already_explained {
            drifts.push(Drift::Added {
                locator: now.locator.clone(),
                label: now.label.clone(),
            });
        }
    }

    drifts
}

/// Why a read could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("the source is no longer open or reachable: {0}")]
    Unreachable(String),
    #[error("could not read {locator:?} at {row:?}: {reason}")]
    Unreadable {
        locator: String,
        row: String,
        reason: String,
    },
    #[error("the source moved unexpectedly: {0}")]
    PositionLost(String),
}

/// What pattern detection, mapping and replay talk to instead of a spreadsheet.
///
/// ## Why reading and advancing are separate
///
/// 4.4's loop is read -> write -> **mark done** -> repeat, and 4.7 requires the
/// mark to be durable. If reading implicitly advanced, a crash between the write
/// and the mark would leave the reader past a row that was never recorded as
/// processed, and that row would be silently skipped forever. Separating them
/// makes the point where `workflow_processed_rows` is written an explicit step
/// the caller cannot omit by accident.
pub trait SourceReader {
    /// Where the reader is now. Safe to persist -- carries no content.
    fn position(&self) -> SourcePosition;

    /// What is at the current position, without reading its values.
    ///
    /// Separate from [`read`](Self::read) so a run can ask "is there anything
    /// left" without pulling content into memory to find out.
    fn peek(&mut self, fields: &[FieldRef]) -> Result<Advance, SourceError>;

    /// Read the current record. Transient -- see the module docs.
    fn read(&mut self, fields: &[FieldRef]) -> Result<SourceRecord, SourceError>;

    /// Move to the next position. Call only after the current record has been
    /// written AND recorded as processed.
    fn advance(&mut self) -> Result<(), SourceError>;

    /// The source's current shape, for [`detect_drift`].
    fn shape(&mut self) -> Result<SourceShape, SourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn shape(cols: &[(&str, &str)]) -> SourceShape {
        SourceShape {
            columns: cols
                .iter()
                .map(|(locator, label)| ColumnShape {
                    locator: locator.to_string(),
                    label: label.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_row_with_any_mapped_value_is_a_record() {
        assert_eq!(classify_row(&row(&["Ada", "12"]), &[]), Advance::Record);
        // One populated mapped column is enough -- a record missing a field is
        // 4.4's "continue but log it", not an ending.
        assert_eq!(classify_row(&row(&["", "12"]), &[]), Advance::Record);
        assert_eq!(classify_row(&row(&["Ada", ""]), &[]), Advance::Record);
    }

    #[test]
    fn a_blank_row_with_nothing_below_is_the_end() {
        assert_eq!(classify_row(&row(&["", ""]), &[]), Advance::Exhausted);
        assert_eq!(
            classify_row(&row(&["", ""]), &[row(&["", ""]), row(&["", ""])]),
            Advance::Exhausted
        );
    }

    #[test]
    fn a_blank_row_with_data_below_is_suspicious_not_the_end() {
        // The 4.10 distinction, and the reason this returns three outcomes
        // rather than a bool: silently treating this as the end would abandon
        // every remaining record without saying so.
        assert_eq!(
            classify_row(&row(&["", ""]), &[row(&["", ""]), row(&["Ada", "12"])]),
            Advance::SuspiciousGap {
                rows_with_data_below: 1
            }
        );
        assert_eq!(
            classify_row(
                &row(&["", ""]),
                &[row(&["Ada", ""]), row(&["", "9"]), row(&["", ""])]
            ),
            Advance::SuspiciousGap {
                rows_with_data_below: 2
            }
        );
    }

    #[test]
    fn whitespace_is_blank() {
        // A cell holding a space is a formatting artefact, not data. Treating
        // it as data would run past the end of every sheet that has one.
        assert_eq!(classify_row(&row(&[" ", "\t"]), &[]), Advance::Exhausted);
        assert_eq!(
            classify_row(&row(&["  "]), &[row(&["Ada"])]),
            Advance::SuspiciousGap {
                rows_with_data_below: 1
            }
        );
    }

    #[test]
    fn an_unchanged_shape_has_no_drift() {
        let s = shape(&[("B", "Name"), ("E", "Total")]);
        assert!(detect_drift(&s, &s).is_empty());
    }

    #[test]
    fn a_relabelled_column_is_reported_where_it_sits() {
        let was = shape(&[("B", "Name"), ("E", "Total")]);
        let now = shape(&[("B", "Client Phone"), ("E", "Total")]);
        assert_eq!(
            detect_drift(&was, &now),
            vec![Drift::LabelChanged {
                locator: "B".into(),
                was: "Name".into(),
                now: "Client Phone".into(),
            }]
        );
    }

    #[test]
    fn a_reordered_column_is_reported_as_moved_not_as_missing_plus_added() {
        // 4.5's reordering case. Reporting this as Missing+Added would lose the
        // pairing the correction panel needs to offer "Looks like column D now?"
        let was = shape(&[("B", "Name"), ("E", "Total")]);
        let now = shape(&[("D", "Name"), ("E", "Total")]);
        assert_eq!(
            detect_drift(&was, &now),
            vec![Drift::Moved {
                label: "Name".into(),
                was: "B".into(),
                now: "D".into(),
            }]
        );
    }

    #[test]
    fn a_column_moved_aside_by_an_insertion_is_still_a_move() {
        // A new column inserted at B pushes Name to C. Both facts are reported:
        // the move is what the correction needs, the addition is what 4.5 says
        // is worth asking about.
        let was = shape(&[("B", "Name"), ("E", "Total")]);
        let now = shape(&[("B", "Region"), ("C", "Name"), ("E", "Total")]);
        let drifts = detect_drift(&was, &now);
        assert!(drifts.contains(&Drift::Moved {
            label: "Name".into(),
            was: "B".into(),
            now: "C".into(),
        }));
        assert!(drifts.contains(&Drift::Added {
            locator: "B".into(),
            label: "Region".into(),
        }));
    }

    #[test]
    fn a_vanished_column_is_missing_and_offers_no_guess() {
        let was = shape(&[("B", "Name"), ("E", "Total")]);
        let now = shape(&[("E", "Total")]);
        assert_eq!(
            detect_drift(&was, &now),
            vec![Drift::Missing {
                locator: "B".into(),
                label: "Name".into(),
            }]
        );
    }

    #[test]
    fn a_record_does_not_print_its_values() {
        // The Debug half of the Section 3 rule. A log line that formatted a
        // record must not become the durable content trail the design refuses.
        let mut fields = BTreeMap::new();
        fields.insert("Customer Name".to_string(), "Ada Lovelace".to_string());
        fields.insert("Total".to_string(), "1234.56".to_string());
        let record = SourceRecord {
            position: SourcePosition {
                source_id: "orders".into(),
                row_key: "47".into(),
            },
            fields,
        };

        let printed = format!("{record:?}");
        assert!(
            !printed.contains("Ada Lovelace") && !printed.contains("1234.56"),
            "values must not appear in Debug output, got: {printed}"
        );
        // The shape is still diagnosable -- names and position survive.
        assert!(printed.contains("Customer Name"));
        assert!(printed.contains("47"));
        assert!(printed.contains("2 elided"));
    }
}
