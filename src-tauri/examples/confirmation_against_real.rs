//! Run the filtered post-hoc confirmation pipeline against a REAL stored
//! recording, with REAL captured bounds.
//!
//!     cargo run --example confirmation_against_real -- <playbook_id> <app_data_dir>
//!
//! `docs/planning/Filtered-Post-Hoc-Confirmation.md` reports 13.0% survival and
//! 3.7% genuinely-ambiguous decisions, both from a MODELLED fixture, because
//! the only real recording available held one record and its bounds predated
//! `element_bounds` existing. This runs the identical pipeline against captured
//! data to confirm or correct those numbers.
//!
//! Reading from the database rather than over IPC is not a preference. Bounds
//! are absent from `CapturedActionView`, so the review screen cannot show them
//! and no command returns them; `compile/mod.rs:238` writes them into
//! `action_payload_json` and that is the only place they can be read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use paradigm_lib::compile::store;
use paradigm_lib::db;

fn usage() -> ! {
    eprintln!("usage: cargo run --example confirmation_against_real -- <playbook_id> <app_data_dir>");
    std::process::exit(1);
}

#[derive(Debug)]
struct Step {
    order: i64,
    kind: String,
    name: Option<String>,
    role: Option<String>,
    detail: Option<String>,
    bounds: Option<(f64, f64, f64, f64)>,
}

/// Spreadsheet cell -> (column, row). The existing `parse_cell_ref` rule.
fn parse_cell(s: &str) -> Option<(String, i64)> {
    let t = s.trim();
    // A sheet-qualified reference keeps only the cell half.
    let t = t.rsplit('!').next().unwrap_or(t);
    let letters: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits: String = t.chars().skip(letters.len()).collect();
    if letters.is_empty() || digits.is_empty() || letters.len() > 3 {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().map(|r| (letters.to_ascii_uppercase(), r))
}

struct Group {
    key: String,
    records: BTreeSet<String>,
    steps: Vec<i64>,
    from_text_watcher: usize,
    from_grid_watcher: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let playbook_id = args.next().unwrap_or_else(|| usage());
    let dir: PathBuf = args.next().map(PathBuf::from).unwrap_or_else(|| usage());

    let (db_path, key_path) = db::paths_in(&dir);
    let conn = db::open(&db_path, &key_path).expect("failed to open db");
    let loaded = store::load(&conn, &playbook_id).expect("load failed");

    let steps: Vec<Step> = loaded
        .steps
        .iter()
        .map(|s| {
            let v: serde_json::Value =
                serde_json::from_str(&s.action_payload_json).expect("payload is json");
            let t = &v["target"];
            let bounds = t["bounds"].as_array().and_then(|a| {
                if a.len() == 4 {
                    Some((
                        a[0].as_f64()?,
                        a[1].as_f64()?,
                        a[2].as_f64()?,
                        a[3].as_f64()?,
                    ))
                } else {
                    None
                }
            });
            Step {
                order: s.step_order,
                kind: s.action_type.clone(),
                name: t["name"].as_str().map(|x| x.to_string()),
                role: t["raw_role"].as_str().map(|x| x.to_string()),
                detail: v["detail"].as_str().map(|x| x.to_string()),
                bounds,
            }
        })
        .collect();

    println!("playbook {playbook_id:?}  name={:?}", loaded.name);
    println!("steps: {}\n", steps.len());

    // ---- how much of the stream even carries bounds --------------------
    let with_bounds = steps.iter().filter(|s| s.bounds.is_some()).count();
    println!("== bounds coverage ==");
    println!(
        "   {with_bounds}/{} steps carry element_bounds ({:.0}%)",
        steps.len(),
        100.0 * with_bounds as f64 / steps.len().max(1) as f64
    );

    // ---- the pipeline ---------------------------------------------------
    let raw = steps.len();
    let s1: Vec<&Step> = steps.iter().filter(|s| s.kind != "navigate").collect();
    let s2: Vec<&&Step> = s1
        .iter()
        .filter(|s| s.name.as_deref().map(|n| !n.trim().is_empty()).unwrap_or(false))
        .collect();

    println!("\n== pipeline ==");
    println!("stage 0  raw actions                    : {raw}");
    println!("stage 1  after dropping Navigate        : {}  (-{})", s1.len(), raw - s1.len());
    println!("stage 2  after dropping unidentifiable  : {}  (-{})", s2.len(), s1.len() - s2.len());

    // ---- do real page-element bounds cluster? ---------------------------
    // Reported before any tolerance is applied, so the raw spread is visible.
    let mut page_bands: Vec<(f64, f64, f64, String, i64)> = Vec::new();
    for s in &s2 {
        let name = s.name.as_deref().unwrap_or("");
        if parse_cell(name).is_some() {
            continue;
        }
        if let Some((l, top, w, _)) = s.bounds {
            page_bands.push((l, top, w, name.chars().take(28).collect(), s.order));
        }
    }
    println!("\n== do real page-element bounds cluster into x-bands? ==");
    if page_bands.is_empty() {
        println!("   no page-element steps with bounds -- nothing to cluster");
    } else {
        page_bands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        println!("   {:>7} {:>7} {:>6}  {:>5}  {}", "x", "y", "width", "step", "name");
        for (l, top, w, n, o) in &page_bands {
            println!("   {l:>7.0} {top:>7.0} {w:>6.0}  {o:>5}  {n}");
        }
    }

    // ---- grouping --------------------------------------------------------
    // Page elements cluster by x with a tolerance, because real layout does not
    // repeat a pixel exactly. TOLERANCE is a parameter this test exists to size.
    const TOLERANCE: f64 = 25.0;
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for s in &s2 {
        let name = s.name.as_deref().unwrap_or("");
        let (key, record) = match parse_cell(name) {
            Some((col, row)) => (format!("{} into cell column {col}", s.kind), row.to_string()),
            None => match s.bounds {
                Some((l, t, _w, _h)) => {
                    // Snap the left edge to a TOLERANCE-wide bucket.
                    let band = (l / TOLERANCE).round() * TOLERANCE;
                    (
                        format!("{} on {} at x~{band:.0}", s.kind, s.role.as_deref().unwrap_or("?")),
                        format!("y{:.0}", t),
                    )
                }
                None => (
                    format!("{} on {} {name:?}", s.kind, s.role.as_deref().unwrap_or("?")),
                    name.to_string(),
                ),
            },
        };
        let g = groups.entry(key.clone()).or_insert_with(|| Group {
            key,
            records: BTreeSet::new(),
            steps: Vec::new(),
            from_text_watcher: 0,
            from_grid_watcher: 0,
        });
        g.records.insert(record);
        g.steps.push(s.order);
        match s.detail.as_deref() {
            Some(d) if d.starts_with("read from element") => g.from_text_watcher += 1,
            Some(d) if d.starts_with("grid cell editor") => g.from_grid_watcher += 1,
            _ => {}
        }
    }
    println!("\nstage 3  distinct field groups          : {}", groups.len());

    println!("
  --- ALL groups, including those below the Rule of 3 ---");
    let mut all: Vec<&Group> = groups.values().collect();
    all.sort_by(|a, b| a.key.cmp(&b.key));
    for g in &all {
        println!("    {:<40} {} records, {} actions  steps {:?}", g.key, g.records.len(), g.steps.len(), g.steps);
    }

    let mut candidates: Vec<&Group> = groups.values().filter(|g| g.records.len() >= 3).collect();
    candidates.sort_by(|a, b| a.key.cmp(&b.key));
    println!("stage 4  surviving Rule of 3            : {}", candidates.len());

    println!("\n  --- what the user would be asked to confirm ---");
    if candidates.is_empty() {
        println!("    (nothing)");
    }
    for c in &candidates {
        println!(
            "    [ ] {:<44} {} records, {} actions   steps {:?}",
            c.key,
            c.records.len(),
            c.steps.len(),
            c.steps
        );
        if c.from_text_watcher > 0 && c.from_grid_watcher > 0 {
            println!(
                "        ^ DOUBLE-CAPTURED: {} from capture::text, {} from capture::grid",
                c.from_text_watcher, c.from_grid_watcher
            );
        }
    }

    println!(
        "\n  survival: {} candidates from {raw} raw actions ({:.1}%)",
        candidates.len(),
        100.0 * candidates.len() as f64 / raw.max(1) as f64
    );
    let clicks = candidates.iter().filter(|c| c.key.starts_with("click on")).count();
    println!(
        "  genuinely ambiguous (page-element clicks only): {clicks} ({:.1}%)",
        100.0 * clicks as f64 / raw.max(1) as f64
    );

    // ---- the inflation prediction ---------------------------------------
    println!("\n== known-defect inflation: actions per group vs records per group ==");
    println!("   Prediction (2026-08-19): the double-capture and spurious-restart");
    println!("   defects inflate ACTION counts within a group, never RECORD counts.");
    let mut violated = false;
    for c in groups.values() {
        if c.steps.len() > c.records.len() {
            println!(
                "   {:<44} {} actions / {} records  (+{})",
                c.key,
                c.steps.len(),
                c.records.len(),
                c.steps.len() - c.records.len()
            );
        }
        if c.records.len() > 3 && c.key.contains("cell column") {
            violated = true;
        }
    }
    if violated {
        println!("   NOTE: a cell-column group covers more than 3 records -- check whether");
        println!("   that is a real fourth record or a defect inflating the RECORD count.");
    }
}
