//! §4.5's format-drift check, on both sides.
//!
//! > "If the **destination** no longer looks like it did when the workflow was
//! > recorded [...] or the **source** has changed shape (a new column added,
//! > columns reordered): stop and ask, using the same mechanism for both."
//!
//! `detect_drift` and `SourceReader::shape` have existed since item 2. What was
//! missing was the other half of the comparison: nothing stored what the
//! surfaces looked like when the user confirmed the workflow, so there was
//! never a `recorded` to compare `current` against. Migration
//! 20260813000006 stores it; this module writes it, reads it, and decides what
//! a run should do about a difference.
//!
//! ## Which drift stops a run, and which does not
//!
//! Not every difference is dangerous, and treating them alike would either
//! block runs constantly or write into the wrong column. The rule here is
//! whether the drift touches a column the mapping actually uses:
//!
//! * A **mapped** column that was renamed, moved or vanished is blocking. The
//!   run would write customer names into whatever now sits at that locator,
//!   which is the exact failure §4.5 exists to prevent.
//! * An **unmapped** column that appeared or changed is reported but not
//!   blocking. §4.5 wants a new column surfaced -- "it is how a reorder usually
//!   announces itself" -- but a spreadsheet growing a notes column that this
//!   workflow never touches is not a reason to refuse to run.
//!
//! That distinction is the same one §4.10 already draws for end-of-data: only
//! the columns the mapping reads are allowed to decide the run's fate.

use rusqlite::Connection;
use uuid::Uuid;

use crate::compile::CompiledTemplate;
use crate::db::DbError;
use crate::source::{ColumnShape, Drift, SourceShape};

/// Which surface a shape describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Source,
    Destination,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Source => "source",
            Side::Destination => "destination",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "source" => Some(Side::Source),
            "destination" => Some(Side::Destination),
            _ => None,
        }
    }
}

/// Store the shape of one surface, replacing whatever was recorded before.
///
/// Replaces rather than accumulates: this records what the surface looks like
/// *now that the user has confirmed it*, so a correction confirmed later is
/// the new truth, not an addition to the old one.
pub fn record_shape(
    conn: &Connection,
    playbook_id: &str,
    side: Side,
    shape: &SourceShape,
) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM workflow_surface_shape WHERE playbook_id = ?1 AND side = ?2",
        rusqlite::params![playbook_id, side.as_str()],
    )?;
    for column in &shape.columns {
        conn.execute(
            "INSERT INTO workflow_surface_shape (id, playbook_id, side, locator, label)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                playbook_id,
                side.as_str(),
                &column.locator,
                &column.label
            ],
        )?;
    }
    Ok(())
}

/// Read back the shape recorded for one surface.
///
/// An empty shape means nothing was recorded, which is not the same as "the
/// surface has no columns" -- see [`check`], which refuses to call that a
/// clean comparison.
pub fn recorded_shape(
    conn: &Connection,
    playbook_id: &str,
    side: Side,
) -> Result<SourceShape, DbError> {
    let mut stmt = conn.prepare(
        "SELECT locator, label FROM workflow_surface_shape
          WHERE playbook_id = ?1 AND side = ?2
          ORDER BY locator",
    )?;
    let rows = stmt.query_map(rusqlite::params![playbook_id, side.as_str()], |r| {
        Ok(ColumnShape {
            locator: r.get(0)?,
            label: r.get(1)?,
        })
    })?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(SourceShape { columns })
}

/// One thing that changed, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftFinding {
    pub side: Side,
    pub drift: Drift,
    /// Does this touch a column the mapping uses?
    pub blocking: bool,
    /// §4.5's "Looks like column D now?" -- the locator to offer first, when
    /// there is a plausible one. `None` means the panel must ask openly.
    pub best_guess: Option<String>,
}

impl DriftFinding {
    /// What the correction panel says before the user clicks.
    pub fn describe(&self) -> String {
        let where_ = match self.side {
            Side::Source => "source",
            Side::Destination => "destination",
        };
        match &self.drift {
            Drift::LabelChanged { locator, was, now } => format!(
                "The {where_} column {locator} was {was:?} and is now {now:?}"
            ),
            Drift::Moved { label, was, now } => format!(
                "The {where_} column {label:?} has moved from {was} to {now}"
            ),
            Drift::Missing { locator, label } => format!(
                "The {where_} column {label:?} (was at {locator}) is no longer there"
            ),
            Drift::Added { locator, label } => {
                format!("The {where_} has a new column {label:?} at {locator}")
            }
        }
    }
}

/// The answer to "is it safe to run?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftCheck {
    /// Both surfaces still look the way they did.
    Unchanged,
    /// Something changed. `blocking` says whether a run must stop and ask.
    Changed {
        findings: Vec<DriftFinding>,
        blocking: bool,
    },
    /// No shape was ever recorded, so nothing can be compared.
    ///
    /// Its own case rather than being folded into `Unchanged`: reporting "no
    /// drift" for a workflow whose shape was never captured would be a
    /// reassurance nothing checked. Workflows confirmed before this existed
    /// land here.
    NothingRecorded,
}

impl DriftCheck {
    /// May a run proceed?
    pub fn may_run(&self) -> bool {
        match self {
            DriftCheck::Unchanged | DriftCheck::NothingRecorded => true,
            DriftCheck::Changed { blocking, .. } => !blocking,
        }
    }

    pub fn findings(&self) -> &[DriftFinding] {
        match self {
            DriftCheck::Changed { findings, .. } => findings,
            _ => &[],
        }
    }
}

/// Compare one surface's recorded shape against its current one.
fn findings_for(
    side: Side,
    recorded: &SourceShape,
    current: &SourceShape,
    mapped: &[String],
) -> Vec<DriftFinding> {
    let touches_mapping = |locator: &str| {
        mapped
            .iter()
            .any(|m| m.eq_ignore_ascii_case(locator))
    };

    crate::source::detect_drift(recorded, current)
        .into_iter()
        .map(|drift| {
            let (blocking, best_guess) = match &drift {
                // The column is where it was but means something else now.
                // Writing there would put the value under the wrong heading.
                Drift::LabelChanged { locator, .. } => (touches_mapping(locator), None),
                // The one case with a real answer to offer.
                Drift::Moved { was, now, .. } => {
                    (touches_mapping(was), Some(now.clone()))
                }
                Drift::Missing { locator, .. } => (touches_mapping(locator), None),
                // §4.5 wants this surfaced, but a column this workflow never
                // reads appearing is not a reason to refuse to run.
                Drift::Added { .. } => (false, None),
            };
            DriftFinding {
                side,
                drift,
                blocking,
                best_guess,
            }
        })
        .collect()
}

/// §4.5's check, both sides.
///
/// `mapped_source` / `mapped_destination` are the locators the template
/// actually uses; drift outside them is reported without blocking.
pub fn check(
    conn: &Connection,
    playbook_id: &str,
    template: &CompiledTemplate,
    current_source: &SourceShape,
    current_destination: &SourceShape,
) -> Result<DriftCheck, DbError> {
    let recorded_source = recorded_shape(conn, playbook_id, Side::Source)?;
    let recorded_destination = recorded_shape(conn, playbook_id, Side::Destination)?;

    if recorded_source.columns.is_empty() && recorded_destination.columns.is_empty() {
        return Ok(DriftCheck::NothingRecorded);
    }

    let mapped_source: Vec<String> = template
        .fields
        .iter()
        .map(|f| f.source_field.clone())
        .collect();
    let mapped_destination: Vec<String> = template
        .fields
        .iter()
        .map(|f| f.destination_field.clone())
        .collect();

    let mut findings = findings_for(
        Side::Source,
        &recorded_source,
        current_source,
        &mapped_source,
    );
    findings.extend(findings_for(
        Side::Destination,
        &recorded_destination,
        current_destination,
        &mapped_destination,
    ));

    if findings.is_empty() {
        return Ok(DriftCheck::Unchanged);
    }
    let blocking = findings.iter().any(|f| f.blocking);
    Ok(DriftCheck::Changed { findings, blocking })
}

/// Apply a correction the user confirmed, permanently (§4.5's "correction
/// scope").
///
/// Repoints one mapped field at a new locator and re-records the shape, so the
/// next run compares against what the user just confirmed rather than against
/// the shape that drifted.
///
/// The one-off case is deliberately NOT here: §4.5 distinguishes "this one
/// order was weird" from "the format actually changed", and a one-off applies
/// to a single record in a single run. Persisting it would be exactly the
/// confusion the question exists to avoid.
pub fn apply_permanent_correction(
    conn: &Connection,
    playbook_id: &str,
    side: Side,
    old_locator: &str,
    new_locator: &str,
    new_label: &str,
) -> Result<(), DbError> {
    let column = match side {
        Side::Source => "source_field",
        Side::Destination => "destination_field",
    };
    conn.execute(
        &format!(
            "UPDATE workflow_field_mappings SET {column} = ?3
              WHERE playbook_id = ?1 AND {column} = ?2"
        ),
        rusqlite::params![playbook_id, old_locator, new_locator],
    )?;
    conn.execute(
        "DELETE FROM workflow_surface_shape
          WHERE playbook_id = ?1 AND side = ?2 AND locator = ?3",
        rusqlite::params![playbook_id, side.as_str(), old_locator],
    )?;
    conn.execute(
        "INSERT INTO workflow_surface_shape (id, playbook_id, side, locator, label)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (playbook_id, side, locator)
         DO UPDATE SET label = excluded.label",
        rusqlite::params![
            Uuid::new_v4().to_string(),
            playbook_id,
            side.as_str(),
            new_locator,
            new_label
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::FieldMapping;
    use tempfile::TempDir;

    fn shape(cols: &[(&str, &str)]) -> SourceShape {
        SourceShape {
            columns: cols
                .iter()
                .map(|(l, n)| ColumnShape {
                    locator: l.to_string(),
                    label: n.to_string(),
                })
                .collect(),
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
            "Drift",
            &crate::compile::ReversibilityPolicy::placeholder(),
            &crate::labeling::RedactionPolicy::placeholder(),
        )
        .with_template(template());
        crate::compile::store::store(&mut conn, &pb).expect("store");
        (dir, conn, pb.id)
    }

    #[test]
    fn a_recorded_shape_round_trips() {
        let (_d, conn, id) = db();
        let s = shape(&[("C", "Customer"), ("D", "Amount")]);
        record_shape(&conn, &id, Side::Source, &s).expect("record");
        assert_eq!(recorded_shape(&conn, &id, Side::Source).expect("read"), s);
    }

    #[test]
    fn the_two_sides_are_recorded_separately() {
        // §4.5 checks both, and a destination shape overwriting the source's
        // would silently make one of them unverifiable.
        let (_d, conn, id) = db();
        record_shape(&conn, &id, Side::Source, &shape(&[("C", "Customer")])).expect("src");
        record_shape(
            &conn,
            &id,
            Side::Destination,
            &shape(&[("A", "Client")]),
        )
        .expect("dst");

        assert_eq!(
            recorded_shape(&conn, &id, Side::Source).expect("read"),
            shape(&[("C", "Customer")])
        );
        assert_eq!(
            recorded_shape(&conn, &id, Side::Destination).expect("read"),
            shape(&[("A", "Client")])
        );
    }

    #[test]
    fn recording_replaces_rather_than_accumulates() {
        let (_d, conn, id) = db();
        record_shape(&conn, &id, Side::Source, &shape(&[("C", "Customer")])).expect("first");
        record_shape(&conn, &id, Side::Source, &shape(&[("C", "Client Name")])).expect("second");
        assert_eq!(
            recorded_shape(&conn, &id, Side::Source).expect("read"),
            shape(&[("C", "Client Name")]),
            "a confirmed correction is the new truth, not an addition"
        );
    }

    #[test]
    fn an_unchanged_pair_of_surfaces_reports_unchanged() {
        let (_d, conn, id) = db();
        let src = shape(&[("C", "Customer")]);
        let dst = shape(&[("A", "Client")]);
        record_shape(&conn, &id, Side::Source, &src).expect("src");
        record_shape(&conn, &id, Side::Destination, &dst).expect("dst");

        let check = check(&conn, &id, &template(), &src, &dst).expect("check");
        assert_eq!(check, DriftCheck::Unchanged);
        assert!(check.may_run());
    }

    #[test]
    fn a_renamed_destination_column_blocks_the_run() {
        // The dangerous direction: the run writes to the destination, so a
        // column that means something else now would receive the wrong values.
        let (_d, conn, id) = db();
        record_shape(&conn, &id, Side::Source, &shape(&[("C", "Customer")])).expect("src");
        record_shape(&conn, &id, Side::Destination, &shape(&[("A", "Client")]))
            .expect("dst");

        let check = check(
            &conn,
            &id,
            &template(),
            &shape(&[("C", "Customer")]),
            &shape(&[("A", "Invoice Date")]),
        )
        .expect("check");

        assert!(!check.may_run(), "a mapped column changing meaning must block");
        let f = &check.findings()[0];
        assert_eq!(f.side, Side::Destination);
        assert!(f.blocking);
        assert!(
            f.describe().contains("Invoice Date"),
            "the panel must say what it is now: {}",
            f.describe()
        );
    }

    #[test]
    fn a_moved_mapped_column_blocks_and_offers_where_it_went() {
        // §4.5's "Looks like column D now?" -- the one case with a real answer.
        let (_d, conn, id) = db();
        record_shape(&conn, &id, Side::Source, &shape(&[("C", "Customer")])).expect("src");
        record_shape(&conn, &id, Side::Destination, &shape(&[("A", "Client")]))
            .expect("dst");

        let check = check(
            &conn,
            &id,
            &template(),
            &shape(&[("D", "Customer")]),
            &shape(&[("A", "Client")]),
        )
        .expect("check");

        assert!(!check.may_run());
        let f = &check.findings()[0];
        assert_eq!(f.side, Side::Source);
        assert_eq!(f.best_guess.as_deref(), Some("D"));
        assert!(f.describe().contains("moved"), "{}", f.describe());
    }

    #[test]
    fn a_new_unmapped_column_is_reported_without_blocking() {
        // §4.5 wants it surfaced -- it is how a reorder announces itself -- but
        // a sheet growing a notes column this workflow never reads is not a
        // reason to refuse to run.
        let (_d, conn, id) = db();
        record_shape(&conn, &id, Side::Source, &shape(&[("C", "Customer")])).expect("src");
        record_shape(&conn, &id, Side::Destination, &shape(&[("A", "Client")]))
            .expect("dst");

        let check = check(
            &conn,
            &id,
            &template(),
            &shape(&[("C", "Customer"), ("Z", "Notes")]),
            &shape(&[("A", "Client")]),
        )
        .expect("check");

        assert!(check.may_run(), "an unmapped addition must not block");
        assert_eq!(check.findings().len(), 1);
        assert!(!check.findings()[0].blocking);
    }

    #[test]
    fn a_workflow_with_no_recorded_shape_says_so_rather_than_reporting_clean() {
        // Reporting "no drift" for something nothing checked would be a
        // reassurance that was never earned.
        let (_d, conn, id) = db();
        let check = check(
            &conn,
            &id,
            &template(),
            &shape(&[("C", "Customer")]),
            &shape(&[("A", "Client")]),
        )
        .expect("check");
        assert_eq!(check, DriftCheck::NothingRecorded);
        assert!(check.may_run(), "an unrecorded shape must not block old workflows");
    }

    #[test]
    fn a_permanent_correction_repoints_the_mapping_and_the_shape() {
        let (_d, conn, id) = db();
        record_shape(&conn, &id, Side::Source, &shape(&[("C", "Customer")])).expect("src");
        record_shape(&conn, &id, Side::Destination, &shape(&[("A", "Client")]))
            .expect("dst");

        apply_permanent_correction(&conn, &id, Side::Source, "C", "D", "Customer")
            .expect("correct");

        let t = crate::compile::store::load_template(&conn, &id)
            .expect("load")
            .expect("template");
        assert_eq!(t.fields[0].source_field, "D", "the mapping must follow");

        // And the next run compares against what the user just confirmed.
        let check = check(
            &conn,
            &id,
            &t,
            &shape(&[("D", "Customer")]),
            &shape(&[("A", "Client")]),
        )
        .expect("check");
        assert_eq!(check, DriftCheck::Unchanged);
    }
}
