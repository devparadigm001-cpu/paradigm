//! Proof -- not assertion -- that the local store is encrypted at rest and
//! usable through a full write/read cycle.
//!
//! These tests link against the SQLCipher-enabled rusqlite the app itself uses.
//! A SQLCipher build with no `PRAGMA key` applied behaves exactly like a stock
//! sqlite3 binary, so "open it without the key" here is the same test as
//! pointing the sqlite3 CLI at the file.

use std::path::{Path, PathBuf};

use paradigm_lib::db;
use rusqlite::Connection;
use tempfile::TempDir;

/// A string we can hunt for in the raw file bytes.
const MARKER: &str = "TOTALLY-PLAINTEXT-MARKER-9f3a2b";

fn temp_paths() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let (db_path, key_path) = db::paths_in(dir.path());
    (dir, db_path, key_path)
}

fn insert_playbook(conn: &Connection, id: &str, name: &str) {
    conn.execute(
        "INSERT INTO playbooks (id, name, source) VALUES (?1, ?2, 'record_mode')",
        (id, name),
    )
    .expect("insert playbook");
}

/// Every byte of the database, including any sidecar WAL still on disk.
fn all_bytes(db_path: &Path) -> Vec<u8> {
    let mut bytes = std::fs::read(db_path).expect("read db file");
    for suffix in ["-wal", "-journal"] {
        let sidecar = PathBuf::from(format!("{}{}", db_path.display(), suffix));
        if sidecar.exists() {
            bytes.extend(std::fs::read(&sidecar).expect("read sidecar"));
        }
    }
    bytes
}

#[test]
fn creates_database_and_key_file() {
    let (_dir, db_path, key_path) = temp_paths();
    assert!(!db_path.exists());

    let conn = db::open(&db_path, &key_path).expect("open db");
    drop(conn);

    assert!(db_path.exists(), "database file was not created");
    assert!(key_path.exists(), "DPAPI key blob was not created");
    assert!(
        std::fs::metadata(&db_path).unwrap().len() > 0,
        "database file is empty"
    );

    // The key blob on disk must not be the raw key: DPAPI output carries its
    // own header and is substantially longer than 32 bytes.
    let blob = std::fs::read(&key_path).unwrap();
    assert!(
        blob.len() > db::crypto::KEY_LEN,
        "key file looks like raw key material ({} bytes)",
        blob.len()
    );
}

#[test]
fn file_header_is_not_plaintext_sqlite() {
    let (_dir, db_path, key_path) = temp_paths();
    let conn = db::open(&db_path, &key_path).expect("open db");
    drop(conn);

    assert!(
        !db::health::header_is_plaintext_sqlite(&db_path).unwrap(),
        "database begins with the plaintext 'SQLite format 3' magic -- it is NOT encrypted"
    );
}

#[test]
fn inserted_text_does_not_appear_in_the_raw_file() {
    let (_dir, db_path, key_path) = temp_paths();
    {
        let conn = db::open(&db_path, &key_path).expect("open db");
        insert_playbook(&conn, "pb-marker", MARKER);
    } // drop checkpoints the WAL back into the main file

    let bytes = all_bytes(&db_path);
    let found = bytes
        .windows(MARKER.len())
        .any(|w| w == MARKER.as_bytes());

    assert!(
        !found,
        "the string {MARKER:?} was written to the database and is readable \
         verbatim in the file bytes -- the data is not encrypted"
    );
}

#[test]
fn opening_without_the_key_fails() {
    let (_dir, db_path, key_path) = temp_paths();
    {
        let conn = db::open(&db_path, &key_path).expect("open db");
        insert_playbook(&conn, "pb1", "secret playbook");
    }

    // Exactly what a stock sqlite3 CLI does: open the file, read the schema.
    let plain = Connection::open(&db_path).expect("open handle");
    let err = plain
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect_err("reading an encrypted database without a key must fail");

    match err {
        rusqlite::Error::SqliteFailure(e, _) => assert_eq!(
            e.code,
            rusqlite::ErrorCode::NotADatabase,
            "expected 'file is not a database', got {e:?}"
        ),
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn opening_with_the_wrong_key_fails() {
    let (_dir, db_path, key_path) = temp_paths();
    {
        let conn = db::open(&db_path, &key_path).expect("open db");
        insert_playbook(&conn, "pb1", "secret playbook");
    }

    let wrong = Connection::open(&db_path).expect("open handle");
    wrong
        .execute_batch(&format!("PRAGMA key = \"x'{}'\";", "ab".repeat(32)))
        .expect("pragma key is accepted unconditionally");

    let err = wrong
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect_err("a wrong key must not decrypt the database");

    assert!(matches!(err, rusqlite::Error::SqliteFailure(..)));
}

#[test]
fn round_trip_survives_close_and_reopen() {
    let (_dir, db_path, key_path) = temp_paths();

    {
        let conn = db::open(&db_path, &key_path).expect("open db");
        insert_playbook(&conn, "pb1", "Quarterly invoice run");
        conn.execute(
            "INSERT INTO playbook_steps
                 (id, playbook_id, step_order, action_type, control_role, action_payload_json)
             VALUES (?1, 'pb1', ?2, ?3, ?4, ?5)",
            ("st1", 1, "click", "button", r#"{"selector":"Submit"}"#),
        )
        .expect("insert step");
        conn.execute(
            "INSERT INTO playbook_steps
                 (id, playbook_id, step_order, action_type, control_role, action_payload_json)
             VALUES (?1, 'pb1', ?2, ?3, ?4, ?5)",
            ("st2", 2, "type", "textbox", r#"{"text":"hello"}"#),
        )
        .expect("insert step");
    }

    // Reopen with the DPAPI-derived key and read it all back.
    let conn = db::open(&db_path, &key_path).expect("reopen db");

    let (name, source): (String, String) = conn
        .query_row(
            "SELECT name, source FROM playbooks WHERE id = 'pb1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read playbook back");
    assert_eq!(name, "Quarterly invoice run");
    assert_eq!(source, "record_mode");

    let mut stmt = conn
        .prepare(
            "SELECT step_order, action_type, control_role, reversible
               FROM playbook_steps WHERE playbook_id = 'pb1' ORDER BY step_order",
        )
        .unwrap();
    let steps: Vec<(i64, String, String, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0], (1, "click".into(), "button".into(), None));
    assert_eq!(steps[1], (2, "type".into(), "textbox".into(), None));
}

#[test]
fn migrations_are_recorded_and_idempotent() {
    let (_dir, db_path, key_path) = temp_paths();

    let conn = db::open(&db_path, &key_path).expect("open db");
    let first = db::migrations::applied_versions(&conn).unwrap();
    assert_eq!(first.len(), db::migrations::MIGRATIONS.len());
    drop(conn);

    // Reopening runs apply_all again; it must be a no-op.
    let mut conn = db::open(&db_path, &key_path).expect("reopen db");
    let applied_now = db::migrations::apply_all(&mut conn).unwrap();
    assert_eq!(applied_now, 0, "migrations re-ran on an up-to-date database");
    assert_eq!(db::migrations::applied_versions(&conn).unwrap(), first);
}

#[test]
fn health_check_reports_healthy() {
    let (_dir, db_path, key_path) = temp_paths();
    let mut conn = db::open(&db_path, &key_path).expect("open db");

    let report = db::health::check(&mut conn, &db_path).expect("health check");

    assert!(report.db_file_exists);
    assert!(!report.header_is_plaintext_sqlite);
    assert!(report.round_trip_ok);
    assert!(report.healthy, "report was not healthy: {report:?}");

    // The health round-trip must roll itself back.
    let leftovers: i64 = conn
        .query_row(
            "SELECT count(*) FROM playbooks WHERE id = 'health-check-pb'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0, "health check left rows behind");
}
