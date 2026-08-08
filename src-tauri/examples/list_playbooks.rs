//! List every stored playbook in the real on-device database — same rows the
//! `list_playbooks` IPC command returns.
//!
//!     cargo run --example list_playbooks

use std::path::PathBuf;

use paradigm_lib::compile::store;
use paradigm_lib::db;

const BUNDLE_IDENTIFIER: &str = "com.amitj.paradigm";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app_data_dir = PathBuf::from(std::env::var("APPDATA")?).join(BUNDLE_IDENTIFIER);
    let (db_path, key_path) = db::paths_in(&app_data_dir);
    let conn = db::open(&db_path, &key_path)?;

    let playbooks = store::list(&conn)?;
    println!("playbook_count={}", playbooks.len());
    for playbook in playbooks {
        println!(
            "{} | {} | {} steps | {} irreversible",
            playbook.id,
            playbook.name,
            playbook.step_count,
            playbook.irreversible_count,
        );
    }
    Ok(())
}
