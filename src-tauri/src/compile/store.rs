//! Write a validated playbook to the encrypted local store, and read it back.

use rusqlite::Connection;

use crate::db::DbError;

use super::validate::{validate, ValidationError};
use super::{CompiledPlaybook, ControlRole};

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
