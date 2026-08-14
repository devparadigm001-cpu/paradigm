//! Minimal reproduction attempt for the cross-connection visibility issue.
//!
//! Two plain processes. No Tauri, no webview, no accessibility tree, no async.
//! One process holds a connection; another process opens the same database and
//! writes a playbook; the first is then asked whether it can see it.
//!
//! The question the app-level observation raised was whether visibility
//! depends on which process CREATED the database, so both cases are run:
//!
//!   * `created`  -- the holding connection creates the database itself
//!   * `opened`   -- the database is created and closed first, then opened
//!
//! ## The third connection is the point
//!
//! Each case also opens a FRESH connection after the write. That is what
//! separates two very different failures:
//!
//!   * fresh connection sees it, holder does not  -> the write landed, and the
//!     holder is stuck on a stale snapshot. A real visibility bug.
//!   * neither sees it                            -> the write never landed,
//!     and the whole framing was wrong.
//!
//! Without that, a failure is ambiguous and the reproduction proves nothing.
//!
//! Usage:
//!   db_visibility_probe            -- run both cases
//!   db_visibility_probe write DIR  -- child: write one playbook, exit

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use paradigm_lib::capture::{ActionCandidate, ActionKind, CapturedStream, ExclusionList};
use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
use paradigm_lib::db;
use paradigm_lib::labeling::RedactionPolicy;

fn a_playbook(conn: &mut rusqlite::Connection, name: &str) -> String {
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never!"]));
    stream.admit(ActionCandidate {
        kind: ActionKind::Click,
        identifiers: vec!["app.exe".into()],
        process_name: None,
        element_role: Some("Button".into()),
        element_name: Some("Next".into()),
        payload: None,
        detail: None,
        timestamp_ms: 0,
    });
    let pb = compile(
        stream.actions(),
        name,
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    store::store(conn, &pb).expect("store playbook");
    pb.id
}

fn count(conn: &rusqlite::Connection) -> usize {
    store::list(conn).expect("list").len()
}

/// `PRAGMA data_version` changes when ANOTHER connection has committed. If the
/// holder's copy never moves, it never noticed the write at all.
fn data_version(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap_or(-1)
}

fn run_case(label: &str, dir: &Path, pre_create: bool) -> bool {
    println!("\n================ case: {label} ================");
    let (db_path, key_path) = db::paths_in(dir);

    if pre_create {
        // Create and CLOSE, so the holder below opens an existing database.
        let mut first = db::open(&db_path, &key_path).expect("create");
        let _ = a_playbook(&mut first, "seed, written before the holder opened");
        drop(first);
        println!("database created and closed first (holder will OPEN it)");
    } else {
        println!("holder will CREATE the database itself");
    }

    let holder = db::open(&db_path, &key_path).expect("holder open");
    let before = count(&holder);
    let dv_before = data_version(&holder);
    println!("holder sees {before} playbook(s) before the external write (data_version {dv_before})");

    // A genuinely separate process.
    let exe = std::env::current_exe().expect("current exe");
    let status = std::process::Command::new(exe)
        .arg("write")
        .arg(dir)
        .status()
        .expect("spawn writer");
    println!("writer process exited: {status}");

    let after = count(&holder);
    let dv_after = data_version(&holder);
    println!("holder sees {after} playbook(s) after  (data_version {dv_after})");

    // The discriminator.
    let fresh = db::open(&db_path, &key_path).expect("fresh open");
    let fresh_count = count(&fresh);
    println!("a FRESH connection sees {fresh_count} playbook(s)");

    let holder_saw_it = after > before;
    let write_landed = fresh_count > before;

    println!(
        "\n  write actually landed        : {write_landed}\n  \
           holder connection saw it     : {holder_saw_it}\n  \
           data_version moved for holder: {}",
        dv_after != dv_before
    );
    if write_landed && !holder_saw_it {
        println!("  => REPRODUCED: the write is on disk and the holder cannot see it.");
    } else if !write_landed {
        println!("  => the write never landed; the framing was wrong.");
    } else {
        println!("  => no problem here: the holder saw the write.");
    }
    write_landed && holder_saw_it
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("write") {
        let dir = PathBuf::from(args.get(2).expect("write needs a directory"));
        let (db_path, key_path) = db::paths_in(&dir);
        let mut conn = db::open(&db_path, &key_path).expect("writer open");
        let id = a_playbook(&mut conn, "written by the second process");
        // Reading it back through the writer's OWN connection rules out the
        // store having silently done nothing.
        let n = count(&conn);
        println!("  [writer] stored {id}, and sees {n} playbook(s) itself");
        return ExitCode::SUCCESS;
    }

    println!("== cross-connection visibility, two plain processes ==");
    println!("no Tauri, no webview, no accessibility tree, no async");

    let created_dir = tempfile::TempDir::new().expect("temp dir");
    let opened_dir = tempfile::TempDir::new().expect("temp dir");

    let created_ok = run_case("holder CREATED the database", created_dir.path(), false);
    let opened_ok = run_case("holder OPENED an existing database", opened_dir.path(), true);

    println!("\n================ VERDICT ================");
    println!("  holder created the db -> saw the write: {created_ok}");
    println!("  holder opened  the db -> saw the write: {opened_ok}");
    if created_ok && opened_ok {
        println!("\n  Does NOT reproduce at this level. Both cases work, so plain");
        println!("  SQLite/rusqlite/SQLCipher across two processes is not the cause,");
        println!("  and something about the app's own setup is.");
    } else if !created_ok && opened_ok {
        println!("\n  REPRODUCED, and it matches the app-level pattern exactly.");
    } else {
        println!("\n  Reproduced, but NOT in the shape the app-level runs suggested.");
    }
    ExitCode::SUCCESS
}
