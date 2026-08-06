//! Temporary verification tool (Step 11a proof) -- NOT part of the product.
//!
//! Opens the real, running app's encrypted local database read-only-in-spirit
//! (same `db::open` path the app itself uses) and dumps the stored step
//! sequence for a given playbook id, so the actual persisted order can be
//! compared against what the review screen showed before saving.
//!
//! Usage: cargo run --example dump_playbook -- <playbook_id> <app_data_dir>
//!
//! `<app_data_dir>` must be passed explicitly -- there is deliberately no
//! fallback to the real app-data path (e.g. `%APPDATA%\com.amitj.paradigm`),
//! so forgetting the argument fails loudly instead of silently opening
//! production data.

use std::path::PathBuf;

use paradigm_lib::compile::store;
use paradigm_lib::db;

fn usage_error() -> ! {
    eprintln!("usage: cargo run --example dump_playbook -- <playbook_id> <app_data_dir>");
    eprintln!(
        "  <app_data_dir> must be passed explicitly -- there is no default, so \
         this cannot open the real app-data directory by accident."
    );
    std::process::exit(1);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let playbook_id = args.next().unwrap_or_else(|| usage_error());
    let app_data_dir: PathBuf = args.next().map(PathBuf::from).unwrap_or_else(|| usage_error());

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
