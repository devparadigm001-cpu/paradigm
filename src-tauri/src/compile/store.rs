//! Write a validated playbook to the encrypted local store, and read it back.

use rusqlite::{Connection, OptionalExtension};

use crate::db::DbError;

use super::validate::{validate, ValidationError};
use super::{CompiledPlaybook, CompiledTemplate, ControlRole};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("playbook failed validation and was NOT stored:\n  - {}", .0.iter().map(|e| e.describe()).collect::<Vec<_>>().join("\n  - "))]
    Invalid(Vec<ValidationError>),

    #[error(transparent)]
    Db(#[from] DbError),

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Validate, then store the playbook and all its steps in one transaction.
///
/// Validation runs inside this function rather than being left to the caller:
/// there is no code path that stores an unvalidated playbook.
pub fn store(conn: &mut Connection, playbook: &CompiledPlaybook) -> Result<(), StoreError> {
    let errors = validate(playbook);
    if !errors.is_empty() {
        return Err(StoreError::Invalid(errors));
    }

    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO playbooks (id, name, source) VALUES (?1, ?2, ?3)",
        (&playbook.id, &playbook.name, &playbook.source),
    )?;

    // Additive: written only when a template was attached, so an ordinary
    // recording produces exactly the rows it always did, in exactly the same
    // transaction. Before the steps, so the field mappings' foreign key has a
    // template to point at.
    if let Some(template) = &playbook.template {
        tx.execute(
            "INSERT INTO workflow_templates
                 (playbook_id, source_id, destination_id, source_step,
                  destination_step, examples)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                &playbook.id,
                &template.source_id,
                &template.destination_id,
                template.source_step,
                template.destination_step,
                template.examples as i64,
            ),
        )?;
        for field in &template.fields {
            tx.execute(
                "INSERT INTO workflow_field_mappings
                     (id, playbook_id, source_field, destination_field)
                 VALUES (?1, ?2, ?3, ?4)",
                (
                    uuid::Uuid::new_v4().to_string(),
                    &playbook.id,
                    &field.source_field,
                    &field.destination_field,
                ),
            )?;
        }
    }

    for step in &playbook.steps {
        tx.execute(
            "INSERT INTO playbook_steps
                 (id, playbook_id, step_order, action_type, control_role, reversible,
                  action_payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                &step.id,
                &playbook.id,
                step.step_order,
                &step.action_type,
                step.control_role.as_str(),
                i64::from(step.reversible),
                &step.action_payload_json,
            ),
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// A row as it exists in the database, read back with no compile-time state.
#[derive(Debug, Clone)]
pub struct StoredStep {
    pub id: String,
    pub step_order: i64,
    pub action_type: String,
    pub control_role: String,
    /// Nullable in the schema; Record Mode always assigns it, so a NULL here
    /// would mean something wrote a step by another route.
    pub reversible: Option<bool>,
    pub action_payload_json: String,
}

#[derive(Debug, Clone)]
pub struct StoredPlaybook {
    pub id: String,
    pub name: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub steps: Vec<StoredStep>,
}

/// Read a playbook and its steps back, ordered.
pub fn load(conn: &Connection, playbook_id: &str) -> Result<StoredPlaybook, DbError> {
    let (name, source, created_at, updated_at) = conn.query_row(
        "SELECT name, source, created_at, updated_at FROM playbooks WHERE id = ?1",
        [playbook_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;

    let mut stmt = conn.prepare(
        "SELECT id, step_order, action_type, control_role, reversible, action_payload_json
           FROM playbook_steps
          WHERE playbook_id = ?1
          ORDER BY step_order",
    )?;
    let steps = stmt
        .query_map([playbook_id], |r| {
            Ok(StoredStep {
                id: r.get(0)?,
                step_order: r.get(1)?,
                action_type: r.get(2)?,
                control_role: r.get(3)?,
                reversible: r.get::<_, Option<i64>>(4)?.map(|v| v != 0),
                action_payload_json: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(StoredPlaybook {
        id: playbook_id.to_string(),
        name,
        source,
        created_at,
        updated_at,
        steps,
    })
}

/// Summary row for listing stored playbooks.
#[derive(Debug, Clone)]
pub struct PlaybookSummary {
    pub id: String,
    pub name: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub step_count: i64,
    /// Steps stored with `reversible = 0`, matching
    /// [`CompiledPlaybook::irreversible_count`](super::CompiledPlaybook::irreversible_count).
    ///
    /// `reversible` is nullable in the schema, and a NULL is neither 0 nor 1,
    /// so it counts towards neither total. That is the intended reading: NULL
    /// means no classification was recorded, which is not the same claim as
    /// "irreversible". Record Mode always assigns the column, so a NULL here
    /// would mean a step arrived by some other route.
    pub irreversible_count: i64,
}

/// Every stored playbook, newest first. No pagination in Phase 1.
pub fn list(conn: &Connection) -> Result<Vec<PlaybookSummary>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.source, p.created_at, p.updated_at,
                (SELECT COUNT(*) FROM playbook_steps s WHERE s.playbook_id = p.id),
                (SELECT COUNT(*) FROM playbook_steps s
                  WHERE s.playbook_id = p.id AND s.reversible = 0)
           FROM playbooks p
          ORDER BY p.created_at DESC, p.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PlaybookSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            source: r.get(2)?,
            created_at: r.get(3)?,
            updated_at: r.get(4)?,
            step_count: r.get(5)?,
            irreversible_count: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Delete a playbook and, through the schema, its steps.
///
/// One statement is enough because migration 20260803000001 already declares
/// the consequences:
///
/// * `playbook_steps.playbook_id` is `ON DELETE CASCADE`, so the steps go with
///   the playbook.
/// * `runs.playbook_id` is `ON DELETE SET NULL`, deliberately -- the Step 1
///   migration comment reads "run history must outlive its playbook". Runs are
///   detached, not deleted, and their `run_steps_log` rows survive with them.
///
/// All of that depends on `PRAGMA foreign_keys = ON`, which `db::open` sets per
/// connection. SQLite ignores foreign-key clauses entirely when it is off, so
/// the cascade is asserted in the tests below rather than trusted.
///
/// Deleting an id that is not there is an error, not a no-op: `DELETE` succeeds
/// while affecting zero rows, and silently reporting success to a caller who
/// asked to remove something nonexistent hides a real mistake.
/// Read a stored template back, if the playbook has one.
///
/// Separate from [`load`] rather than folded into `StoredPlaybook`: replay does
/// not need it, and widening the type every existing caller already uses would
/// make an additive change ripple.
pub fn load_template(
    conn: &Connection,
    playbook_id: &str,
) -> Result<Option<CompiledTemplate>, DbError> {
    let row = conn
        .query_row(
            "SELECT source_id, destination_id, source_step, destination_step, examples
               FROM workflow_templates WHERE playbook_id = ?1",
            [playbook_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;

    let Some((source_id, destination_id, source_step, destination_step, examples)) = row else {
        return Ok(None);
    };

    // Sorted, so a round trip compares equal regardless of insertion order --
    // §4.12's "field order doesn't matter, field identity does", applied to
    // reading as well as to detection.
    let mut stmt = conn.prepare(
        "SELECT source_field, destination_field
           FROM workflow_field_mappings
          WHERE playbook_id = ?1
          ORDER BY source_field, destination_field",
    )?;
    let fields = stmt
        .query_map([playbook_id], |r| {
            Ok(crate::detect::FieldMapping {
                source_field: r.get(0)?,
                destination_field: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Some(CompiledTemplate {
        source_id,
        destination_id,
        source_step,
        destination_step,
        examples: examples as usize,
        fields,
    }))
}

pub fn delete(conn: &Connection, playbook_id: &str) -> Result<(), DbError> {
    let affected = conn.execute("DELETE FROM playbooks WHERE id = ?1", [playbook_id])?;

    if affected == 0 {
        return Err(DbError::NotFound {
            what: "playbook",
            id: playbook_id.to_string(),
        });
    }
    Ok(())
}

/// Parse a stored `control_role` back into the enum. Unknown values become
/// `Other`, matching the compile-time mapping's fallback.
pub fn parse_control_role(raw: &str) -> ControlRole {
    match raw {
        "button" => ControlRole::Button,
        "textbox" => ControlRole::Textbox,
        "dropdown" => ControlRole::Dropdown,
        "checkbox" => ControlRole::Checkbox,
        "radio" => ControlRole::Radio,
        "link" => ControlRole::Link,
        _ => ControlRole::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{ActionCandidate, ActionKind, CapturedAction, CapturedStream, ExclusionList};
    use crate::compile::{compile, ReversibilityPolicy};
    use crate::labeling::RedactionPolicy;
    use tempfile::TempDir;

    /// Clicks on the named buttons, gated the only way actions can be.
    ///
    /// Button names drive reversibility here: the placeholder policy treats
    /// "Send"/"Delete" as irreversible keywords and has no keyword matching
    /// "Cancel"/"Back"/"Next", so the fixture controls the split without
    /// touching the stored rows by hand.
    fn clicks(names: &[&str]) -> Vec<CapturedAction> {
        let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
        for (i, name) in names.iter().enumerate() {
            stream.admit(ActionCandidate {
                kind: ActionKind::Click,
                identifiers: vec!["app.exe".into()],
                process_name: None,
                element_role: Some("Button".into()),
                element_name: Some((*name).to_string()),
                payload: None,
                detail: None,
                timestamp_ms: i as u64,
            });
        }
        stream.actions().to_vec()
    }

    /// Store one playbook in a fresh encrypted database and list it back.
    ///
    /// Goes through the real `store`/`list` pair rather than asserting on the
    /// `CompiledPlaybook`: the point is that the SQL counts correctly, which an
    /// in-memory check would not exercise at all.
    fn stored_summary(names: &[&str], label: &str) -> (PlaybookSummary, CompiledPlaybook) {
        let dir = TempDir::new().expect("temp dir");
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        let mut conn = crate::db::open(&db_path, &key_path).expect("open encrypted db");

        let playbook = compile(
            &clicks(names),
            label,
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        store(&mut conn, &playbook).expect("store playbook");

        let mut rows = list(&conn).expect("list playbooks");
        assert_eq!(rows.len(), 1, "expected exactly one stored playbook");
        (rows.remove(0), playbook)
    }

    #[test]
    fn a_mixed_playbook_reports_only_the_irreversible_steps() {
        let (summary, playbook) =
            stored_summary(&["Send", "Cancel", "Delete", "Back"], "Mixed");

        assert_eq!(summary.step_count, 4);
        assert_eq!(
            summary.irreversible_count, 2,
            "should count Send and Delete, not the reversible steps"
        );

        // The SQL and the in-memory definition must not drift apart.
        assert_eq!(
            summary.irreversible_count as usize,
            playbook.irreversible_count(),
            "list() disagrees with CompiledPlaybook::irreversible_count"
        );
    }

    #[test]
    fn a_fully_reversible_playbook_reports_zero() {
        let (summary, playbook) = stored_summary(&["Cancel", "Back", "Next"], "All Reversible");

        assert_eq!(summary.step_count, 3);
        assert_eq!(
            summary.irreversible_count, 0,
            "no step here matches an irreversible keyword"
        );
        assert_eq!(playbook.irreversible_count(), 0, "fixture is wrong, not the SQL");
    }

    /// Open a scratch database the same way the app does, so the connection
    /// carries the same pragmas -- `PRAGMA foreign_keys` in particular, which
    /// the deletion behaviour depends on entirely.
    fn scratch_db(dir: &TempDir) -> Connection {
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        crate::db::open(&db_path, &key_path).expect("open encrypted db")
    }

    fn count(conn: &Connection, sql: &str, id: &str) -> i64 {
        conn.query_row(sql, [id], |r| r.get(0)).expect("count query")
    }

    #[test]
    fn deleting_a_playbook_removes_its_steps_but_detaches_rather_than_deletes_runs() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);

        // The whole cascade rests on this being enforced. SQLite parses
        // ON DELETE clauses and then ignores them when it is off, so a passing
        // test would otherwise prove nothing.
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .expect("read PRAGMA foreign_keys");
        assert_eq!(fk, 1, "foreign keys are OFF; ON DELETE clauses are a no-op");

        let playbook = compile(
            &clicks(&["Send", "Cancel", "Back"]),
            "To Be Deleted",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        {
            let mut conn = scratch_db(&dir);
            store(&mut conn, &playbook).expect("store playbook");
        }
        let conn = scratch_db(&dir);

        // A run against it, so the retention rule has something to act on.
        let run_id = crate::replay::journal::start_run(&conn, &playbook.id).expect("start run");

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM playbook_steps WHERE playbook_id = ?1",
                &playbook.id
            ),
            3,
            "fixture should have stored three steps"
        );

        delete(&conn, &playbook.id).expect("delete playbook");

        // 1. The playbook itself is gone.
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM playbooks WHERE id = ?1", &playbook.id),
            0
        );

        // 2. Its steps cascaded away with it.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM playbook_steps WHERE playbook_id = ?1",
                &playbook.id
            ),
            0,
            "steps should cascade with the playbook"
        );

        // 3. The run SURVIVES, detached -- "run history must outlive its
        //    playbook" (migration 20260803000002). Deleting it instead would be
        //    a silent loss of audit history.
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM runs WHERE id = ?1", &run_id),
            1,
            "the run should survive its playbook"
        );
        let orphaned: Option<String> = conn
            .query_row("SELECT playbook_id FROM runs WHERE id = ?1", [&run_id], |r| {
                r.get(0)
            })
            .expect("read the run back");
        assert_eq!(
            orphaned, None,
            "the surviving run's playbook_id should be NULL, not the dead id"
        );
    }

    fn template() -> CompiledTemplate {
        CompiledTemplate {
            source_id: "orders-doc".into(),
            destination_id: "shipping-doc".into(),
            source_step: 1,
            destination_step: 1,
            examples: 3,
            fields: vec![
                crate::detect::FieldMapping {
                    source_field: "C".into(),
                    destination_field: "B".into(),
                },
                crate::detect::FieldMapping {
                    source_field: "D".into(),
                    destination_field: "E".into(),
                },
            ],
        }
    }

    fn count_all(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).expect("count")
    }

    /// The regression that matters most, and the reason this test exists at all.
    ///
    /// `compile` and `store` are shared by every playbook in the product. A
    /// change here that quietly altered ordinary recordings would be invisible
    /// until a replay went wrong, so "the templated path is additive" is
    /// asserted rather than assumed: an ordinary recording produces no
    /// template, writes no row to either new table, and stores exactly the
    /// steps and payloads it always did.
    #[test]
    fn an_ordinary_recording_is_completely_unaffected() {
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);

        let playbook = compile(
            &clicks(&["Send", "Cancel", "Back"]),
            "Ordinary",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        assert!(
            playbook.template.is_none(),
            "compile must never attach a template on its own"
        );
        store(&mut conn, &playbook).expect("store");

        // Nothing in either new table.
        assert_eq!(count_all(&conn, "SELECT COUNT(*) FROM workflow_templates"), 0);
        assert_eq!(
            count_all(&conn, "SELECT COUNT(*) FROM workflow_field_mappings"),
            0
        );
        assert_eq!(load_template(&conn, &playbook.id).expect("load"), None);

        // And the playbook itself is exactly what it was: three steps, in
        // order, with their payloads intact.
        let loaded = load(&conn, &playbook.id).expect("load");
        assert_eq!(loaded.steps.len(), 3);
        assert_eq!(
            loaded.steps.iter().map(|s| s.step_order).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        for step in &loaded.steps {
            let payload: serde_json::Value =
                serde_json::from_str(&step.action_payload_json).expect("payload is json");
            assert!(payload["target"]["selector"].is_string());
            assert_eq!(payload["target"]["raw_role"].as_str(), Some("Button"));
        }
    }

    #[test]
    fn a_template_survives_compile_store_and_load() {
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);

        let playbook = compile(
            &clicks(&["One", "Two", "Three"]),
            "Templated",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
        .with_template(template());
        store(&mut conn, &playbook).expect("store");

        assert_eq!(
            load_template(&conn, &playbook.id).expect("load"),
            Some(template()),
            "the mapping and advancement rule must round-trip intact"
        );

        // The literal example steps are still there -- §5 item 5's "alongside",
        // not "instead of".
        assert_eq!(load(&conn, &playbook.id).expect("load").steps.len(), 3);
    }

    #[test]
    fn deleting_a_templated_playbook_removes_its_template_and_mappings() {
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);
        let playbook = compile(
            &clicks(&["One"]),
            "Doomed",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
        .with_template(template());
        store(&mut conn, &playbook).expect("store");
        assert_eq!(
            count_all(&conn, "SELECT COUNT(*) FROM workflow_field_mappings"),
            2
        );

        delete(&conn, &playbook.id).expect("delete");

        // Both cascades fire -- the mappings hang off the template, which hangs
        // off the playbook, so this also proves the two-step chain works.
        assert_eq!(count_all(&conn, "SELECT COUNT(*) FROM workflow_templates"), 0);
        assert_eq!(
            count_all(&conn, "SELECT COUNT(*) FROM workflow_field_mappings"),
            0
        );
    }

    #[test]
    fn a_pattern_that_detection_would_refuse_cannot_be_stored() {
        // The two judgements detection makes, enforced again at the schema so
        // they cannot be bypassed by a caller assembling a template by hand.
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);

        // §2: a source that never moved is inconclusive, not a pattern.
        let still = compile(
            &clicks(&["One"]),
            "Still Source",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
        .with_template(CompiledTemplate {
            source_step: 0,
            ..template()
        });
        assert!(
            store(&mut conn, &still).is_err(),
            "a zero source step must be rejected"
        );

        // The Rule of 3.
        let thin = compile(
            &clicks(&["One"]),
            "Two Examples",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
        .with_template(CompiledTemplate {
            examples: 2,
            ..template()
        });
        assert!(
            store(&mut conn, &thin).is_err(),
            "fewer than three examples must be rejected"
        );
    }

    #[test]
    fn a_playbook_can_carry_only_one_pattern() {
        // §4.12's "one pattern per workflow", made unrepresentable by the
        // primary key rather than left to callers to remember.
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);
        let playbook = compile(
            &clicks(&["One"]),
            "Single",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
        .with_template(template());
        store(&mut conn, &playbook).expect("store");

        assert!(
            conn.execute(
                "INSERT INTO workflow_templates
                     (playbook_id, source_id, destination_id, source_step,
                      destination_step, examples)
                 VALUES (?1, 'other', 'other', 1, 1, 3)",
                [&playbook.id],
            )
            .is_err(),
            "a second pattern for one workflow must be impossible"
        );
    }

    /// The retained history is not just retained, it is READABLE.
    ///
    /// The test above proves the run survives its playbook with a NULL
    /// `playbook_id`. Surviving is not the same as being reachable: the only
    /// reader was `load_runs_for_playbook`, which needs an id to ask for, and
    /// after deletion there is no id to pass. So the history was being kept and
    /// could not be got back -- the open item in
    /// `docs/known-issues/no-way-to-delete-playbooks.md`.
    ///
    /// This drives the whole path against a real encrypted database: store a
    /// playbook, run it, log real step events, delete it, then read the run and
    /// its logs back through `load_orphaned_runs`.
    #[test]
    fn a_deleted_playbooks_run_history_is_still_readable_through_the_orphan_path() {
        use crate::replay::journal::{
            self, load_orphaned_runs, load_step_logs, StepLogEntry, EVENT_EXECUTE,
        };

        let dir = TempDir::new().expect("temp dir");
        let playbook = compile(
            &clicks(&["Send", "Cancel"]),
            "Has History",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        {
            let mut conn = scratch_db(&dir);
            store(&mut conn, &playbook).expect("store playbook");
        }
        let conn = scratch_db(&dir);

        // A run with real logged events, so there is history worth keeping.
        let run_id = journal::start_run(&conn, &playbook.id).expect("start run");
        for (order, action) in [(1_i64, "click"), (2, "click")] {
            journal::log_step(
                &conn,
                &run_id,
                &StepLogEntry {
                    playbook_step_id: None,
                    step_order: order,
                    action_type: action.to_string(),
                    target_ui_context_json: "{}".to_string(),
                    data_payload: None,
                    is_sensitive: false,
                    event_type: EVENT_EXECUTE.to_string(),
                    model_source: None,
                    cost: 0.0,
                    foreground_app: "probe".to_string(),
                },
            )
            .expect("log step");
        }
        journal::finish_run(&conn, &run_id, "completed").expect("finish run");

        // Before deletion it is reachable the ordinary way.
        assert_eq!(
            journal::load_runs_for_playbook(&conn, &playbook.id)
                .expect("load runs")
                .len(),
            1
        );
        assert!(
            load_orphaned_runs(&conn).expect("load orphans").is_empty(),
            "nothing is orphaned yet"
        );

        delete(&conn, &playbook.id).expect("delete playbook");

        // The ordinary reader can no longer see it -- this is the gap.
        assert!(
            journal::load_runs_for_playbook(&conn, &playbook.id)
                .expect("load runs")
                .is_empty(),
            "after deletion the run is unreachable by playbook id, which is why \
             the orphan path exists"
        );

        // The new path finds it, and it is the same run.
        let orphans = load_orphaned_runs(&conn).expect("load orphans");
        assert_eq!(orphans.len(), 1, "the detached run should be readable");
        assert_eq!(orphans[0].id, run_id);
        assert_eq!(orphans[0].playbook_id, None);
        assert_eq!(orphans[0].status, "completed");
        assert!(
            orphans[0].started_at.is_some(),
            "the run's timing should survive with it"
        );

        // And the step log survived too -- the part that makes history useful.
        let logs = load_step_logs(&conn, &run_id).expect("load step logs");
        assert_eq!(logs.len(), 2, "both logged events should survive deletion");
        assert_eq!(
            logs.iter().map(|l| l.step_order).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    /// The sheet-qualified cell reference must survive compile and the store.
    ///
    /// It rides in `element_name` precisely so it takes the path the bare cell
    /// reference already took, but "should therefore work" is the kind of claim
    /// this project keeps having to retract. So it is driven through the real
    /// gate, the real compiler and a real encrypted database, and read back.
    ///
    /// Both halves matter. The qualified step must arrive intact *and* still be
    /// recognised as a grid step by `is_cell_editor`, or replay would resolve it
    /// as an ordinary selector instead of going through the Name Box.
    #[test]
    fn a_sheet_qualified_cell_survives_compile_store_and_load() {
        use crate::capture::grid::is_cell_editor;

        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);

        let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
        stream.admit(ActionCandidate {
            kind: ActionKind::Type,
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            element_role: Some("ComboBox".into()),
            element_name: Some("Sheet2!B2".into()),
            payload: Some("hello".into()),
            detail: None,
            timestamp_ms: 0,
        });
        let playbook = compile(
            stream.actions(),
            "Qualified Cell",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        store(&mut conn, &playbook).expect("store playbook");

        let loaded = load(&conn, &playbook.id).expect("load playbook");
        assert_eq!(loaded.steps.len(), 1);
        let step = &loaded.steps[0];

        // The payload is where replay reads the target from, so that is what is
        // asserted -- `StoredStep` exposes no separate name column.
        let payload: serde_json::Value =
            serde_json::from_str(&step.action_payload_json).expect("payload is json");
        assert_eq!(payload["target"]["name"].as_str(), Some("Sheet2!B2"));
        assert_eq!(payload["target"]["raw_role"].as_str(), Some("ComboBox"));

        // Still a grid step after the round trip -- this is the check that
        // decides whether replay uses the Name Box at all.
        assert!(
            is_cell_editor(
                payload["target"]["raw_role"].as_str().unwrap_or_default(),
                payload["target"]["name"].as_str().unwrap_or_default(),
            ),
            "a stored qualified reference must still take the grid path"
        );
    }

    /// Layer 2 of the process-name plumbing: it must survive compile, the
    /// store, and the read back. Verified against a real encrypted database
    /// rather than by inspecting the JSON that `compile` builds, because the
    /// question is whether it survives the round trip.
    #[test]
    fn the_process_name_survives_compile_store_and_load() {
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);

        // Built through the real gate, with a process name attached.
        let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
        stream.admit(ActionCandidate {
            kind: ActionKind::Navigate,
            identifiers: vec!["Untitled - Notepad".into()],
            process_name: Some("notepad.exe".into()),
            element_role: Some("Window".into()),
            element_name: Some("Untitled - Notepad".into()),
            payload: None,
            detail: None,
            timestamp_ms: 0,
        });
        let actions = stream.actions().to_vec();
        assert_eq!(
            actions[0].process_name.as_deref(),
            Some("notepad.exe"),
            "the gate dropped the process name"
        );

        let playbook = compile(
            &actions,
            "Process Name Round Trip",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        store(&mut conn, &playbook).expect("store");

        let loaded = load(&conn, &playbook.id).expect("load");
        let payload: serde_json::Value =
            serde_json::from_str(&loaded.steps[0].action_payload_json).expect("payload json");

        assert_eq!(
            payload["process"].as_str(),
            Some("notepad.exe"),
            "process name did not survive to the stored payload: {payload}"
        );
        // And the display string is still the title, unchanged.
        assert_eq!(payload["app"].as_str(), Some("Untitled - Notepad"));
    }

    /// An action with no process name -- an old recording, or an event that
    /// reported none -- must store a null rather than failing or inventing one.
    #[test]
    fn a_missing_process_name_stores_as_null() {
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);

        let playbook = compile(
            &clicks(&["Send"]),
            "No Process Name",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        store(&mut conn, &playbook).expect("store");

        let loaded = load(&conn, &playbook.id).expect("load");
        let payload: serde_json::Value =
            serde_json::from_str(&loaded.steps[0].action_payload_json).expect("payload json");

        assert!(
            payload["process"].is_null(),
            "expected null, got {}",
            payload["process"]
        );
    }

    #[test]
    fn deleting_an_unknown_playbook_is_an_error_not_a_silent_success() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);

        // DELETE affects zero rows and succeeds at the SQL level, so without an
        // explicit check this would report success and the caller would believe
        // something was removed.
        let err = delete(&conn, "no-such-playbook").expect_err("must not succeed");
        assert!(
            matches!(err, DbError::NotFound { what: "playbook", .. }),
            "unhelpful error: {err}"
        );
    }

    #[test]
    fn deleting_one_playbook_leaves_the_others_alone() {
        let dir = TempDir::new().expect("temp dir");
        let mut conn = scratch_db(&dir);

        let doomed = compile(
            &clicks(&["Send"]),
            "Doomed",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        let keeper = compile(
            &clicks(&["Cancel", "Back"]),
            "Keeper",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        store(&mut conn, &doomed).expect("store doomed");
        store(&mut conn, &keeper).expect("store keeper");

        delete(&conn, &doomed.id).expect("delete");

        let rows = list(&conn).expect("list");
        assert_eq!(rows.len(), 1, "exactly one playbook should remain");
        assert_eq!(rows[0].id, keeper.id);
        assert_eq!(rows[0].step_count, 2, "the survivor keeps its steps");
    }

    #[test]
    fn the_count_is_per_playbook_not_across_the_table() {
        // A correlated subquery missing its WHERE would still pass both tests
        // above when only one playbook exists. Two playbooks catch that.
        let dir = TempDir::new().expect("temp dir");
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        let mut conn = crate::db::open(&db_path, &key_path).expect("open encrypted db");

        let risky = compile(
            &clicks(&["Send", "Delete"]),
            "Risky",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        let safe = compile(
            &clicks(&["Cancel", "Back"]),
            "Safe",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        );
        store(&mut conn, &risky).expect("store risky");
        store(&mut conn, &safe).expect("store safe");

        let rows = list(&conn).expect("list playbooks");
        let by_id = |id: &str| {
            rows.iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("playbook {id} missing from list"))
                .clone()
        };

        assert_eq!(by_id(&risky.id).irreversible_count, 2);
        assert_eq!(by_id(&safe.id).irreversible_count, 0);
    }
}
