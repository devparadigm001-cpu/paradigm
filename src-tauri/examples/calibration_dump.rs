//! Debug tool: dump `confidence_calibration` and enough surrounding counts to
//! interpret it. NOT part of the product.
//!
//! Usage: cargo run --example calibration_dump -- <app_data_dir>
//!
//! `<app_data_dir>` must be passed explicitly -- there is deliberately no
//! fallback to the real app-data path (e.g. `%APPDATA%\com.amitj.paradigm`),
//! so forgetting the argument fails loudly instead of silently opening
//! production data. Same rule as `dump_playbook`.
//!
//! ## Why the extra counts
//!
//! An empty calibration table is ambiguous on its own: it means either that
//! nothing ran, or that what ran wrote somewhere else. `clean_tag_probe`,
//! `compile_probe`, and `replay_probe` all pass a temp dir to `db::paths_in`,
//! so "the probe ran tonight" and "the real store has rows" are separate
//! claims. The `playbooks` / `runs` / `run_steps_log` counts and the
//! per-`model_source` breakdown distinguish the two.
//!
//! Reads only. It opens through `db::open`, which applies migrations, but those
//! are already applied and idempotent; no statement here writes a row.

use std::path::PathBuf;

use paradigm_lib::db;

fn usage_error() -> ! {
    eprintln!("usage: cargo run --example calibration_dump -- <app_data_dir>");
    eprintln!(
        "  <app_data_dir> must be passed explicitly -- there is no default, so \
         this cannot open the real app-data directory by accident."
    );
    std::process::exit(1);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app_data_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| usage_error());

    let (db_path, key_path) = db::paths_in(&app_data_dir);

    println!("app data dir : {}", app_data_dir.display());
    println!("database     : {}", db_path.display());
    println!(
        "db size      : {} bytes",
        std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)
    );
    println!(
        "wal size     : {} bytes\n",
        std::fs::metadata(app_data_dir.join("paradigm.db-wal"))
            .map(|m| m.len())
            .unwrap_or(0)
    );

    let conn = db::open(&db_path, &key_path)?;

    // ---- the table itself -------------------------------------------------
    let mut stmt = conn.prepare(
        "SELECT id, model_source, raw_score_min, raw_score_max,
                sample_count, success_count, normalized_score, updated_at
           FROM confidence_calibration
          ORDER BY model_source, raw_score_min",
    )?;

    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, f64, f64, i64, i64, Option<f64>, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    println!("confidence_calibration: {} row(s)", rows.len());

    if rows.is_empty() {
        println!("  (EMPTY -- no calibration samples recorded in this database)");
    } else {
        println!(
            "\n  {:<30} {:<11} {:>7} {:>8} {:>11}  updated_at",
            "model_source", "bin", "samples", "success", "normalized"
        );
        println!("  {}", "-".repeat(100));
        for (_id, model, min, max, samples, success, normalized, updated) in &rows {
            println!(
                "  {:<30} [{:.1},{:.1}) {:>7} {:>8} {:>11}  {}",
                model,
                min,
                max,
                samples,
                success,
                match normalized {
                    Some(v) => format!("{v:.4}"),
                    None => "NULL".to_string(),
                },
                updated
            );
        }

        let total_samples: i64 = rows.iter().map(|r| r.4).sum();
        let total_success: i64 = rows.iter().map(|r| r.5).sum();
        let non_null = rows.iter().filter(|r| r.6.is_some()).count();
        println!(
            "\n  totals: {total_samples} sample(s), {total_success} success(es), \
             {non_null} row(s) with a non-NULL normalized_score (Phase 1 expects 0)"
        );
        println!("\n  ids (deterministic, model|bin_min):");
        for (id, ..) in &rows {
            println!("    {id}");
        }
    }

    // ---- corroborating context -------------------------------------------
    println!("\n-- context from the same database --");
    for (table, sql) in [
        ("playbooks", "SELECT COUNT(*) FROM playbooks"),
        ("playbook_steps", "SELECT COUNT(*) FROM playbook_steps"),
        ("runs", "SELECT COUNT(*) FROM runs"),
        ("run_steps_log", "SELECT COUNT(*) FROM run_steps_log"),
    ] {
        let n: i64 = conn.query_row(sql, [], |r| r.get(0))?;
        println!("  {table:<16} {n}");
    }

    let mut stmt = conn.prepare(
        "SELECT COALESCE(model_source, '(null)'), COUNT(*), MIN(timestamp), MAX(timestamp)
           FROM run_steps_log
          GROUP BY model_source
          ORDER BY COUNT(*) DESC",
    )?;
    let by_model: Vec<(String, i64, Option<String>, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    println!("\n  run_steps_log by model_source:");
    if by_model.is_empty() {
        println!("    (no rows)");
    } else {
        for (model, n, first, last) in by_model {
            println!(
                "    {:<30} {:>5}  first={}  last={}",
                model,
                n,
                first.unwrap_or_else(|| "-".into()),
                last.unwrap_or_else(|| "-".into())
            );
        }
    }

    let mut stmt = conn.prepare(
        "SELECT id, name, source, created_at FROM playbooks ORDER BY created_at DESC LIMIT 10",
    )?;
    let pbs: Vec<(String, String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    println!("\n  most recent playbooks:");
    if pbs.is_empty() {
        println!("    (none)");
    } else {
        for (id, name, source, created) in pbs {
            println!("    {created}  {source:<12} {name:<34} {id}");
        }
    }

    Ok(())
}
