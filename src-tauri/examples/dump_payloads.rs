//! Print FULL payloads for a playbook's steps -- no truncation.
//!
//! `dump_playbook` truncates, which hides the one field that matters when a
//! recording is suspected of capturing the wrong value.
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(id), Some(dir)) = (args.next(), args.next()) else {
        eprintln!("usage: dump_payloads <playbook_id> <app_data_dir>");
        return ExitCode::FAILURE;
    };
    let (db_path, key_path) = paradigm_lib::db::paths_in(&PathBuf::from(dir));
    let conn = match paradigm_lib::db::open(&db_path, &key_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("open: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut stmt = conn
        .prepare(
            "SELECT step_order, action_type, action_payload_json
               FROM playbook_steps WHERE playbook_id = ?1 ORDER BY step_order",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([&id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .expect("query");
    for row in rows {
        let (order, kind, payload) = row.expect("row");
        
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
        println!(
            "step {order:>3} {kind:<9} target={:<28} app={}",
            format!("{:?}", v["target"]["name"].as_str().unwrap_or("-")),
            v["app"].as_str().unwrap_or("-").chars().take(60).collect::<String>()
        );
    }

    // Is it templated at all?
    let state: String = conn
        .query_row(
            "SELECT template_state FROM playbooks WHERE id = ?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "?".into());
    println!("\ntemplate_state = {state}");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM workflow_templates WHERE playbook_id = ?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    println!("workflow_templates rows = {n}");
    if let Ok(t) = paradigm_lib::compile::store::load_template(&conn, &id) {
        if let Some(t) = t {
            println!("  source      = {}", t.source_id);
            println!("  destination = {}", t.destination_id);
            println!("  steps       = source +{} / destination +{}", t.source_step, t.destination_step);
            for f in &t.fields {
                println!("  map         {} -> {}", f.source_field, f.destination_field);
            }
        }
    }
    ExitCode::SUCCESS
}
