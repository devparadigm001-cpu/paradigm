//! Temporary verification tool (Step 11a proof) -- NOT part of the product.
//!
//! Opens the real, running app's encrypted local database read-only-in-spirit
//! (same `db::open` path the app itself uses) and dumps the stored step
//! sequence for a given playbook id, so the actual persisted order can be
//! compared against what the review screen showed before saving.
//!
//! Usage: cargo run --example dump_playbook -- <playbook_id>

use std::path::PathBuf;

use paradigm_lib::compile::store;
use paradigm_lib::db;

fn main() {
    let playbook_id = std::env::args()
        .nth(1)
        .expect("usage: dump_playbook <playbook_id>");

    let app_data_dir: PathBuf = PathBuf::from(std::env::var("APPDATA").expect("no APPDATA"))
        .join("com.amitj.paradigm");
    let (db_path, key_path) = db::paths_in(&app_data_dir);

    println!("db_path:  {}", db_path.display());
    println!("key_path: {}", key_path.display());

    let conn = db::open(&db_path, &key_path).expect("failed to open db");

    println!("\n-- all stored playbooks --");
    for p in store::list(&conn).expect("list failed") {
        println!(
            "  id={} name={:?} step_count={} created_at={}",
            p.id, p.name, p.step_count, p.created_at
        );
    }

    println!("\n-- steps for playbook {playbook_id} (stored order) --");
    let loaded = store::load(&conn, &playbook_id).expect("load failed");
    println!(
        "name={:?} source={:?} created_at={} updated_at={}",
        loaded.name, loaded.source, loaded.created_at, loaded.updated_at
    );
    for step in &loaded.steps {
        // action_payload_json carries element_name/source_app etc -- print
        // enough of it to identify the concrete captured action.
        let payload_snippet: String = step.action_payload_json.chars().take(160).collect();
        println!(
            "  step_order={:>3} action_type={:<12} control_role={:<10} reversible={:?}\n            payload={payload_snippet}",
            step.step_order, step.action_type, step.control_role, step.reversible
        );
    }
}
