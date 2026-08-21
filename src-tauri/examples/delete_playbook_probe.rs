//! Temporary verification tool -- NOT part of the product.
//!
//! Deletes one stored playbook through exactly the call the `delete_playbook`
//! IPC command makes. Kept rather than recreated each time: every diagnostic
//! round trip since 2026-08-19 has needed it, because bounds and `detail` are
//! only readable from the database, and the UI route is not drivable --
//! `clickname` reports a successful click on Delete while React never opens the
//! confirmation dialog.
//!
//! Usage: cargo run --example delete_playbook_probe -- <playbook_id> <app_data_dir>
//!
//! `<app_data_dir>` must be passed explicitly, same rule as `dump_playbook`, so
//! forgetting it fails loudly instead of opening production data.
use std::path::PathBuf;
use paradigm_lib::compile::store;
use paradigm_lib::db;

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().expect("playbook id");
    let dir: PathBuf = args.next().map(PathBuf::from).expect("app data dir");
    let (db_path, key_path) = db::paths_in(&dir);
    let conn = db::open(&db_path, &key_path).expect("open db");
    store::delete(&conn, &id).expect("delete");
    println!("deleted {id}");
    println!("remaining: {}", store::list(&conn).expect("list").len());
}
