//! Dump one run and the playbook it belongs to, raw.
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(run_id), Some(dir)) = (args.next(), args.next()) else {
        eprintln!("usage: dump_run <run_id> <app_data_dir>");
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

    // Which table is it in? `runs` means Phase-1 replay: the templated run
    // loop never journals a run row.
    let found: Result<(String, String, Option<String>, Option<String>), _> = conn.query_row(
        "SELECT playbook_id, status, started_at, completed_at FROM runs WHERE id = ?1",
        [&run_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    );
    let (playbook_id, status, started, completed) = match found {
        Ok(v) => v,
        Err(e) => {
            println!("NOT FOUND in `runs`: {e}");
            println!("(a templated workflow run does not journal a `runs` row)");
            return ExitCode::SUCCESS;
        }
    };
    println!("run      {run_id}");
    println!("  in table `runs` -> this is a Phase-1 replay_playbook run");
    println!("  playbook  {playbook_id}");
    println!("  status    {status}");
    println!("  started   {started:?}  completed {completed:?}");

    let (name, tstate): (String, String) = conn
        .query_row(
            "SELECT name, template_state FROM playbooks WHERE id = ?1",
            [&playbook_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or_else(|_| ("<gone>".into(), "?".into()));
    println!("  playbook name   {name:?}");
    println!("  template_state  {tstate}");

    println!("\n-- steps logged for this run --");
    let mut stmt = conn
        .prepare(
            "SELECT step_order, action_type, event_type, target_ui_context_json
               FROM run_steps_log WHERE run_id = ?1 ORDER BY step_order",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([&run_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .expect("query");
    for row in rows {
        let (order, kind, event, ctx) = row.expect("row");
        let v: serde_json::Value = serde_json::from_str(&ctx).unwrap_or_default();
        println!(
            "  step {order:>2} {kind:<9} {event:<8} selector={} result={}",
            v["selector"],
            v["result"]
        );
        if let Some(d) = v["detail"].as_str() {
            if !d.is_empty() {
                println!("      detail: {d}");
            }
        }
    }
    ExitCode::SUCCESS
}
