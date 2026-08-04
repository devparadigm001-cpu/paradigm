//! Local, on-device SQLite store for the Paradigm desktop app.
//!
//! Encrypted with SQLCipher from the very first migration -- there is no
//! plaintext-then-upgrade path, because a database that was ever written in the
//! clear leaves recoverable pages behind forever. The key comes from Windows
//! DPAPI at runtime (see [`crypto`]).
//!
//! This is deliberately *not* sqlx, even though `backend-service/` is: sqlx's
//! SQLite driver has no SQLCipher support.

pub mod crypto;
pub mod health;
pub mod migrations;

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0} failed: {1}")]
    Dpapi(&'static str, std::io::Error),

    #[error("key material is {got} bytes, expected {expected}")]
    KeyLength { got: usize, expected: usize },

    #[error("could not generate random key material: {0}")]
    Rand(String),

    #[error(
        "migration {version} changed after it was applied \
         (recorded {recorded}, computed {computed}); write a new migration instead"
    )]
    MigrationChanged {
        version: &'static str,
        recorded: String,
        computed: String,
    },

    #[error(
        "database did not decrypt -- the key file does not match this database, \
         or the file is not a SQLCipher database"
    )]
    BadKey,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Filenames inside the app data directory.
pub const DB_FILENAME: &str = "paradigm.db";
pub const KEY_FILENAME: &str = "paradigm.db.key";

/// Standard on-disk locations for a given app data directory.
pub fn paths_in(app_data_dir: &Path) -> (PathBuf, PathBuf) {
    (
        app_data_dir.join(DB_FILENAME),
        app_data_dir.join(KEY_FILENAME),
    )
}

/// Open (creating if needed) the encrypted database and bring it up to date.
///
/// Ordering matters: `PRAGMA key` must be the first statement on the
/// connection, before any other pragma or query, or SQLCipher will treat the
/// file as plaintext.
pub fn open(db_path: &Path, key_path: &Path) -> Result<Connection, DbError> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let key = crypto::load_or_create_key(key_path)?;
    let mut conn = Connection::open(db_path)?;

    apply_key(&conn, &key)?;
    verify_readable(&conn)?;

    // Only meaningful once the connection is decrypted.
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let journal: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    debug_assert_eq!(journal, "wal");

    migrations::apply_all(&mut conn)?;
    Ok(conn)
}

/// Feed the raw 256-bit key to SQLCipher.
///
/// The `x'...'` form tells SQLCipher to use these bytes directly as the key
/// rather than running them through PBKDF2 as a passphrase. The hex string is
/// built from key bytes we generated, so there is nothing to escape, and it is
/// zeroized as soon as the pragma has run.
fn apply_key(conn: &Connection, key: &Zeroizing<Vec<u8>>) -> Result<(), DbError> {
    use std::fmt::Write;

    let mut hex = Zeroizing::new(String::with_capacity(key.len() * 2));
    for byte in key.iter() {
        // Writing into a String cannot fail.
        let _ = write!(&mut *hex, "{byte:02x}");
    }

    let mut pragma = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", &*hex));
    let result = conn.execute_batch(&pragma);
    pragma.clear();
    result?;
    Ok(())
}

/// Prove the key actually decrypted the file.
///
/// SQLCipher accepts `PRAGMA key` unconditionally; a wrong key only surfaces on
/// the first real read, as "file is not a database".
fn verify_readable(conn: &Connection) -> Result<(), DbError> {
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::NotADatabase =>
        {
            Err(DbError::BadKey)
        }
        Err(e) => Err(e.into()),
    }
}
