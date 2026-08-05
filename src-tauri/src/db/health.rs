//! Runtime self-check for the encrypted store.
//!
//! Answers three questions that "it compiled" does not: does the file exist,
//! is it genuinely encrypted, and can we actually write and read a row back?

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use super::{migrations, DbError};

/// A plaintext SQLite database begins with this exact 16-byte header.
/// A SQLCipher database does not -- page 1 is ciphertext, starting with the
/// random salt. This is the same signal the `file` utility and every SQLite
/// tool use to recognise the format.
const SQLITE_PLAINTEXT_MAGIC: &[u8; 16] = b"SQLite format 3\0";

#[derive(Debug, Serialize)]
pub struct HealthReport {
    pub db_path: String,
    pub db_file_exists: bool,
    pub db_file_bytes: u64,
    pub key_file_exists: bool,
    /// True only if the file starts with the plaintext SQLite magic, i.e. it is
    /// NOT encrypted. This must be false.
    pub header_is_plaintext_sqlite: bool,
    pub applied_migrations: Vec<String>,
    pub tables: Vec<String>,
    /// Insert + read-back on `playbooks` and `playbook_steps`, rolled back.
    pub round_trip_ok: bool,
    /// True when every check above came out the way it should.
    pub healthy: bool,
}

/// Read the first 16 bytes and report whether they are the plaintext magic.
pub fn header_is_plaintext_sqlite(db_path: &Path) -> Result<bool, DbError> {
    let mut buf = [0u8; 16];
    let mut file = File::open(db_path)?;
    match file.read_exact(&mut buf) {
        Ok(()) => Ok(&buf == SQLITE_PLAINTEXT_MAGIC),
        // Too short to be a plaintext SQLite file.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Write a playbook and a step, read them back, then roll the whole thing back
/// so the check leaves no trace in the user's data.
fn round_trip(conn: &mut Connection) -> Result<bool, DbError> {
    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO playbooks (id, name, source) VALUES (?1, ?2, ?3)",
        ("health-check-pb", "health check", "record_mode"),
    )?;
    tx.execute(
        "INSERT INTO playbook_steps
             (id, playbook_id, step_order, action_type, control_role, action_payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            "health-check-st",
            "health-check-pb",
            1,
            "click",
            "button",
            r#"{"probe":true}"#,
        ),
    )?;

    let (name, source): (String, String) = tx.query_row(
        "SELECT name, source FROM playbooks WHERE id = ?1",
        ["health-check-pb"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let (action, role, payload): (String, String, String) = tx.query_row(
        "SELECT action_type, control_role, action_payload_json
           FROM playbook_steps WHERE id = ?1",
        ["health-check-st"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    // reversible is assigned by the (not yet built) compile step, so a freshly
    // written step must come back NULL.
    let reversible: Option<i64> = tx.query_row(
        "SELECT reversible FROM playbook_steps WHERE id = ?1",
        ["health-check-st"],
        |row| row.get(0),
    )?;

    tx.rollback()?;

    Ok(name == "health check"
        && source == "record_mode"
        && action == "click"
        && role == "button"
        && payload == r#"{"probe":true}"#
        && reversible.is_none())
}

/// Full report. `conn` must already be an open, decrypted connection.
pub fn check(conn: &mut Connection, db_path: &Path) -> Result<HealthReport, DbError> {
    let db_file_exists = db_path.exists();
    let db_file_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let header_is_plaintext_sqlite = if db_file_exists {
        header_is_plaintext_sqlite(db_path)?
    } else {
        false
    };

    let applied_migrations = migrations::applied_versions(conn)?;

    let tables = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
              ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let round_trip_ok = round_trip(conn)?;

    let healthy = db_file_exists
        && db_file_bytes > 0
        && !header_is_plaintext_sqlite
        && round_trip_ok
        && applied_migrations.len() == migrations::MIGRATIONS.len();

    Ok(HealthReport {
        db_path: db_path.display().to_string(),
        db_file_exists,
        db_file_bytes,
        key_file_exists: db_path
            .parent()
            .map(|p| p.join(super::KEY_FILENAME).exists())
            .unwrap_or(false),
        header_is_plaintext_sqlite,
        applied_migrations,
        tables,
        round_trip_ok,
        healthy,
    })
}
