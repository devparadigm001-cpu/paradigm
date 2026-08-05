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
