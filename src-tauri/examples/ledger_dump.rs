//! What each templated playbook has actually recorded as processed.
//!
//! Read-only. Opens the same encrypted store the app uses and prints the
//! `workflow_processed_rows` ledger alongside each playbook's template, because
//! the question "why was a row re-offered" is answered by comparing the ledger
//! key against the key the scan looks up -- and that key is
//! `(playbook_id, source_id, row_key)`, all three.
//!
//! Usage: cargo run --example ledger_dump [data-dir]

use std::path::{Path, PathBuf};

fn main() {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = std::env::var("APPDATA").expect("APPDATA");
            Path::new(&base).join("com.amitj.paradigm")
        });
    println!("data dir: {}\n", dir.display());

    let (db_path, key_path) = paradigm_lib::db::paths_in(&dir);
    let conn = match paradigm_lib::db::open(&db_path, &key_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not open the store: {e}");
            std::process::exit(1);
        }
    };

    // ---- playbooks ---------------------------------------------------------
    println!("== playbooks ==");
    let mut stmt = conn
        .prepare(
            "SELECT id, name, template_state, template_declined_at, created_at
               FROM playbooks ORDER BY created_at",
        )
        .expect("prepare playbooks");
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .expect("query playbooks");
    for row in rows {
        let (id, name, state, declined, created) = row.expect("row");
        println!("  {id}");
        println!("    name    : {name:?}");
        println!(
            "    template: {state}{}",
            match declined {
                Some(at) => format!("  (a pattern was offered and declined at {at})"),
                None => String::new(),
            }
        );
        println!("    created : {created}");

        // The source the scan will look rows up under.
        let template: Option<(String, String, i64)> = conn
            .query_row(
                "SELECT source_id, destination_id, source_step
                   FROM workflow_templates WHERE playbook_id = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        match &template {
            Some((source, destination, step)) => {
                println!("    source  : {source:?}");
                println!("    dest    : {destination:?}   step {step}");
            }
            None => println!("    (no template -- an ordinary playbook)"),
        }

        // The ledger for THIS playbook.
        let mut led = conn
            .prepare(
                "SELECT source_id, row_key, processed_at
                   FROM workflow_processed_rows
                  WHERE playbook_id = ?1
                  ORDER BY processed_at, row_key",
            )
            .expect("prepare ledger");
        let entries: Vec<(String, String, String)> = led
            .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query ledger")
            .map(|e| e.expect("ledger row"))
            .collect();
        if entries.is_empty() {
            println!("    ledger  : EMPTY -- every row in the source counts as new");
        } else {
            println!("    ledger  : {} row(s)", entries.len());
            for (source_id, row_key, at) in &entries {
                // The mismatch that would re-offer a processed row: the ledger
                // is keyed by source_id too, so an entry recorded under a
                // different source id is invisible to the scan.
                let matches = template
                    .as_ref()
                    .map(|(s, _, _)| s == source_id)
                    .unwrap_or(false);
                println!(
                    "        row {row_key:<6} source {source_id:?}  {at}{}",
                    if matches {
                        ""
                    } else {
                        "   <- SOURCE ID DOES NOT MATCH THE TEMPLATE"
                    }
                );
            }
        }
        println!();
    }

    // ---- runs --------------------------------------------------------------
    println!("== runs ==");
    let mut stmt = conn
        .prepare("SELECT id, playbook_id, status, started_at FROM runs ORDER BY started_at")
        .expect("prepare runs");
    let runs = stmt
        // `playbook_id` is NULLABLE here on purpose: run history outlives the
        // playbook it describes, so a deleted workflow leaves its runs behind
        // with a NULL. That is exactly the evidence worth seeing -- a NULL
        // means a workflow existed, ran, and was then deleted, taking its
        // CASCADE-linked ledger entries with it.
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .expect("query runs");
    let mut any = false;
    for run in runs {
        let (id, playbook, status, started) = run.expect("run row");
        any = true;
        match playbook {
            Some(p) => println!("  {started}  {status:<10} playbook {p}  run {id}"),
            None => println!(
                "  {started}  {status:<10} playbook <DELETED>  run {id}\
                 \n      ^ its ledger entries went with it: workflow_processed_rows \
                 CASCADEs on playbook delete"
            ),
        }
    }
    if !any {
        println!("  (none)");
    }
}
