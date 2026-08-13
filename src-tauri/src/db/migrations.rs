//! Forward-only migration runner.
//!
//! rusqlite has no migration framework (that is a sqlx feature, and sqlx's
//! SQLite driver cannot speak SQLCipher), so the .sql files in `migrations/`
//! are compiled into the binary with `include_str!` and applied in filename
//! order. Embedding them means a shipped desktop build cannot drift from the
//! migrations it was built against.

use rusqlite::{Connection, OptionalExtension};

use super::DbError;

pub struct Migration {
    pub version: &'static str,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Applied in this order. Append only -- never edit or reorder an entry that
/// has shipped; the checksum guard below will reject it.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "20260803000001",
        name: "init_playbooks",
        sql: include_str!("../../migrations/20260803000001_init_playbooks.sql"),
    },
    Migration {
        version: "20260803000002",
        name: "init_runs",
        sql: include_str!("../../migrations/20260803000002_init_runs.sql"),
    },
    Migration {
        version: "20260803000003",
        name: "init_confidence_calibration",
        sql: include_str!("../../migrations/20260803000003_init_confidence_calibration.sql"),
    },
    Migration {
        version: "20260813000004",
        name: "templated_workflow_state",
        sql: include_str!("../../migrations/20260813000004_templated_workflow_state.sql"),
    },
];

/// FNV-1a over the migration text, ignoring `\r` so a git checkout with CRLF
/// line endings produces the same value as one with LF. This detects an
/// already-applied migration being edited after the fact; it is not a security
/// hash and is not meant to resist a deliberate collision.
fn checksum(sql: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in sql.bytes().filter(|b| *b != b'\r') {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Apply every migration not yet recorded in `schema_migrations`.
/// Returns how many were applied this call.
pub fn apply_all(conn: &mut Connection) -> Result<usize, DbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    TEXT PRIMARY KEY,
             name       TEXT NOT NULL,
             checksum   TEXT NOT NULL,
             applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         );",
    )?;

    let mut applied = 0usize;
    for migration in MIGRATIONS {
        let expected = checksum(migration.sql);
        let recorded: Option<String> = conn
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get(0),
            )
            .optional()?;

        match recorded {
            Some(found) if found == expected => continue,
            Some(found) => {
                return Err(DbError::MigrationChanged {
                    version: migration.version,
                    recorded: found,
                    computed: expected,
                })
            }
            None => {}
        }

        // Each migration lands atomically: either the DDL and its ledger row
        // both commit, or neither does.
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, checksum) VALUES (?1, ?2, ?3)",
            (migration.version, migration.name, &expected),
        )?;
        tx.commit()?;
        applied += 1;
    }

    Ok(applied)
}

/// Versions recorded as applied, in order.
pub fn applied_versions(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;

    /// A real encrypted database with every migration applied, the same way the
    /// product opens one. Testing the DDL against anything else would not be
    /// testing the schema that ships.
    fn scratch_db(dir: &TempDir) -> Connection {
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        crate::db::open(&db_path, &key_path).expect("open encrypted db")
    }

    /// Insert a playbook the way a Phase 1 writer does -- naming only the
    /// columns that existed before this migration.
    fn insert_playbook(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO playbooks (id, name, source) VALUES (?1, ?2, 'record_mode')",
            (id, format!("playbook {id}")),
        )
        .expect("insert playbook");
    }

    fn insert_processed(
        conn: &Connection,
        id: &str,
        playbook: &str,
        source: &str,
        row: &str,
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO workflow_processed_rows (id, playbook_id, source_id, row_key)
             VALUES (?1, ?2, ?3, ?4)",
            (id, playbook, source, row),
        )
    }

    #[test]
    fn the_templated_workflow_migration_is_applied() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);
        let versions = super::applied_versions(&conn).expect("read versions");
        assert!(
            versions.iter().any(|v| v == "20260813000004"),
            "migration should be recorded as applied, got {versions:?}"
        );
    }

    #[test]
    fn an_existing_playbook_is_untouched_by_the_new_columns() {
        // The property that matters for every playbook already on disk: a
        // writer that knows nothing about templated workflows keeps working,
        // and the row it produces is inert -- not a template, not running.
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);
        insert_playbook(&conn, "plain");

        let (state, confirmed_at, run): (String, Option<String>, String) = conn
            .query_row(
                "SELECT template_state, template_confirmed_at, run_state
                   FROM playbooks WHERE id = 'plain'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("read back");

        assert_eq!(state, "none");
        assert_eq!(confirmed_at, None);
        assert_eq!(run, "idle");
    }

    #[test]
    fn the_state_columns_reject_values_outside_their_sets() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);
        insert_playbook(&conn, "p");

        assert!(
            conn.execute(
                "UPDATE playbooks SET template_state = 'maybe' WHERE id = 'p'",
                []
            )
            .is_err(),
            "template_state must be constrained to its three values"
        );
        assert!(
            conn.execute("UPDATE playbooks SET run_state = 'stopped' WHERE id = 'p'", [])
                .is_err(),
            "run_state must be constrained to its three values"
        );
    }

    #[test]
    fn a_confirmation_timestamp_is_required_exactly_when_confirmed() {
        // Structural rather than conventional: "confirmed" without a time, or a
        // time without confirmation, are both nonsense and neither is
        // representable.
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);
        insert_playbook(&conn, "p");

        assert!(
            conn.execute(
                "UPDATE playbooks SET template_state = 'confirmed' WHERE id = 'p'",
                []
            )
            .is_err(),
            "confirming without a timestamp must be rejected"
        );
        assert!(
            conn.execute(
                "UPDATE playbooks SET template_confirmed_at = '2026-08-13T00:00:00.000Z'
                  WHERE id = 'p'",
                []
            )
            .is_err(),
            "a confirmation time without confirmation must be rejected"
        );

        // Both together is the only accepted shape.
        conn.execute(
            "UPDATE playbooks
                SET template_state = 'confirmed',
                    template_confirmed_at = '2026-08-13T00:00:00.000Z'
              WHERE id = 'p'",
            [],
        )
        .expect("confirming with a timestamp should succeed");

        // And revoking must clear it, rather than leaving a stale claim that
        // the user answered.
        assert!(
            conn.execute(
                "UPDATE playbooks SET template_state = 'proposed' WHERE id = 'p'",
                []
            )
            .is_err(),
            "revoking must clear the timestamp in the same statement"
        );
        conn.execute(
            "UPDATE playbooks
                SET template_state = 'proposed', template_confirmed_at = NULL
              WHERE id = 'p'",
            [],
        )
        .expect("revoking and clearing together should succeed");
    }

    #[test]
    fn a_workflow_cannot_record_the_same_source_row_twice() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);
        insert_playbook(&conn, "w1");

        insert_processed(&conn, "r1", "w1", "sheet-A", "row-7").expect("first insert");
        assert!(
            insert_processed(&conn, "r2", "w1", "sheet-A", "row-7").is_err(),
            "the same workflow recording the same row twice must be rejected"
        );
    }

    /// The 4.13 invariant, and the reason this table is keyed the way it is.
    ///
    /// Two saved workflows can read the SAME source for different purposes.
    /// If tracking were keyed by source alone, running one would make the other
    /// believe rows it has never touched were already handled, and it would skip
    /// them permanently. This asserts the two are genuinely independent, in both
    /// directions, rather than merely that the insert succeeds.
    #[test]
    fn two_workflows_tracking_the_same_source_stay_independent() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);
        insert_playbook(&conn, "shipping");
        insert_playbook(&conn, "accounting");

        // The same source row, claimed by both workflows.
        insert_processed(&conn, "s1", "shipping", "orders", "row-1")
            .expect("shipping records row-1");
        insert_processed(&conn, "a1", "accounting", "orders", "row-1")
            .expect("accounting must be able to record the SAME row independently");

        // Shipping gets ahead by one.
        insert_processed(&conn, "s2", "shipping", "orders", "row-2")
            .expect("shipping records row-2");

        let processed = |workflow: &str, row: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM workflow_processed_rows
                  WHERE playbook_id = ?1 AND source_id = 'orders' AND row_key = ?2",
                (workflow, row),
                |r| r.get(0),
            )
            .expect("count")
        };

        // Each sees exactly its own work, and neither is contaminated by the
        // other -- the specific failure 4.13 exists to prevent.
        assert_eq!(processed("shipping", "row-1"), 1);
        assert_eq!(processed("accounting", "row-1"), 1);
        assert_eq!(processed("shipping", "row-2"), 1);
        assert_eq!(
            processed("accounting", "row-2"),
            0,
            "accounting must NOT see row-2 as handled just because shipping did"
        );

        let total = |workflow: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM workflow_processed_rows WHERE playbook_id = ?1",
                [workflow],
                |r| r.get(0),
            )
            .expect("count")
        };
        assert_eq!(total("shipping"), 2);
        assert_eq!(total("accounting"), 1);
    }

    #[test]
    fn deleting_a_workflow_removes_its_processed_rows_but_not_another_workflows() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);

        // The cascade only means anything with foreign keys enforced, and
        // SQLite parses ON DELETE clauses then ignores them when they are off.
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .expect("read PRAGMA foreign_keys");
        assert_eq!(fk, 1, "foreign keys are OFF; ON DELETE is a no-op");

        insert_playbook(&conn, "shipping");
        insert_playbook(&conn, "accounting");
        insert_processed(&conn, "s1", "shipping", "orders", "row-1").expect("insert");
        insert_processed(&conn, "a1", "accounting", "orders", "row-1").expect("insert");

        conn.execute("DELETE FROM playbooks WHERE id = 'shipping'", [])
            .expect("delete playbook");

        let remaining: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT playbook_id FROM workflow_processed_rows ORDER BY playbook_id")
                .expect("prepare");
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect");
            rows
        };
        assert_eq!(
            remaining,
            vec!["accounting".to_string()],
            "the deleted workflow's tracking goes with it; the other's survives"
        );
    }

    #[test]
    fn processed_rows_require_a_real_workflow() {
        let dir = TempDir::new().expect("temp dir");
        let conn = scratch_db(&dir);
        assert!(
            insert_processed(&conn, "x", "no-such-playbook", "orders", "row-1").is_err(),
            "tracking must not accumulate against a workflow that does not exist"
        );
    }
}
