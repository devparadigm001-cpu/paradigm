//! Does step order identify the record axis on a REAL recording?
//!
//!     cargo run --example axis_from_recording -- <playbook_id> <app_data_dir>
//!
//! The temporal rule -- records are the outer loop of a task, fields the inner
//! loop, so the record axis cuts the step sequence into blocks that do not
//! interleave -- measured 0% overlap on the correct axis and 100% on the wrong
//! one, on OrderFlow's VERTICAL card list.
//!
//! One layout is one geometry. This runs the identical check against a saved
//! recording of whatever surface was captured, so a HORIZONTAL layout can be
//! tested with real step numbers rather than a page measured statically.
//!
//! It reports the numbers and does NOT decide the answer: the physical axis and
//! the ground truth are for the reader to compare. A result that is not clean
//! and binary is the interesting outcome, not a failed run.

use std::collections::BTreeSet;
use std::path::PathBuf;

use paradigm_lib::compile::store;
use paradigm_lib::db;

fn usage() -> ! {
    eprintln!("usage: cargo run --example axis_from_recording -- <playbook_id> <app_data_dir>");
    std::process::exit(1);
}

struct El {
    step: i64,
    x: f64,
    y: f64,
    name: String,
    kind: String,
}

/// A spreadsheet cell reference, which marks the DESTINATION side.
fn is_cell_ref(s: &str) -> bool {
    let t = s.trim().rsplit('!').next().unwrap_or("");
    let letters: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits: String = t.chars().skip(letters.len()).collect();
    !letters.is_empty()
        && letters.len() <= 3
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
}

fn cluster(vals: &[f64], tolerance: f64) -> Vec<f64> {
    let mut v: Vec<f64> = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.dedup();
    let mut out: Vec<Vec<f64>> = Vec::new();
    for x in v {
        match out.last_mut() {
            Some(c) if x - c[c.len() - 1] <= tolerance => c.push(x),
            _ => out.push(vec![x]),
        }
    }
    out.iter()
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

/// Fraction of cluster PAIRS whose step ranges overlap.
/// 0.0 = perfectly contiguous blocks in time; 1.0 = fully interleaved.
fn overlap_fraction(els: &[&El], use_y: bool, tolerance: f64) -> Option<(f64, usize)> {
    let vals: Vec<f64> = els.iter().map(|e| if use_y { e.y } else { e.x }).collect();
    let centres = cluster(&vals, tolerance);
    let mut ranges: Vec<(i64, i64)> = Vec::new();
    for c in &centres {
        let mut steps: Vec<i64> = els
            .iter()
            .filter(|e| ((if use_y { e.y } else { e.x }) - c).abs() <= tolerance)
            .map(|e| e.step)
            .collect();
        if steps.is_empty() {
            continue;
        }
        steps.sort_unstable();
        ranges.push((steps[0], steps[steps.len() - 1]));
    }
    if ranges.len() < 2 {
        return None;
    }
    let mut overlaps = 0usize;
    let mut pairs = 0usize;
    for i in 0..ranges.len() {
        for j in (i + 1)..ranges.len() {
            pairs += 1;
            if ranges[i].1 >= ranges[j].0 && ranges[j].1 >= ranges[i].0 {
                overlaps += 1;
            }
        }
    }
    Some((overlaps as f64 / pairs as f64, ranges.len()))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| usage());
    let dir: PathBuf = args.next().map(PathBuf::from).unwrap_or_else(|| usage());

    let (db_path, key_path) = db::paths_in(&dir);
    let conn = db::open(&db_path, &key_path).expect("failed to open db");
    let loaded = store::load(&conn, &id).expect("load failed");

    let mut els: Vec<El> = Vec::new();
    for s in &loaded.steps {
        let v: serde_json::Value =
            serde_json::from_str(&s.action_payload_json).expect("payload is json");
        let t = &v["target"];
        let name = t["name"].as_str().unwrap_or("").to_string();
        if name.trim().is_empty() || s.action_type == "navigate" {
            continue;
        }
        if let Some(a) = t["bounds"].as_array() {
            if a.len() == 4 {
                els.push(El {
                    step: s.step_order,
                    x: a[0].as_f64().unwrap_or(0.0),
                    y: a[1].as_f64().unwrap_or(0.0),
                    name,
                    kind: s.action_type.clone(),
                });
            }
        }
    }

    println!("playbook {:?}  steps {}  usable elements {}", loaded.name, loaded.steps.len(), els.len());

    if std::env::args().any(|a| a == "--all") {
        println!("
== every step, aligned ==");
        for s in &loaded.steps {
            let v: serde_json::Value = serde_json::from_str(&s.action_payload_json).unwrap();
            let t = &v["target"];
            let b = t["bounds"].as_array().map(|a| format!("x{:>6.0} y{:>6.0} w{:>5.0}", a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0), a[2].as_f64().unwrap_or(0.0))).unwrap_or_else(|| "  (no bounds)      ".into());
            println!("  {:>3} {:<9} {}  {}", s.step_order, s.action_type, b, t["name"].as_str().unwrap_or("").chars().take(40).collect::<String>());
        }
        return;
    }

    // SOURCE side only. Spreadsheet cells are the destination and are addressed
    // by cell reference, not geometry -- including them would measure the
    // spreadsheet's layout rather than the page's.
    let source: Vec<&El> = els.iter().filter(|e| !is_cell_ref(&e.name)).collect();
    let dest: Vec<&El> = els.iter().filter(|e| is_cell_ref(&e.name)).collect();
    println!("  source-side elements: {}   destination-side (cell refs): {}", source.len(), dest.len());

    if source.len() < 6 {
        println!("\nToo few source-side elements to judge. Nothing further to report.");
        return;
    }

    println!("\n== source-side geometry ==");
    let xs: Vec<f64> = source.iter().map(|e| e.x).collect();
    let ys: Vec<f64> = source.iter().map(|e| e.y).collect();
    for (axis, vals, tol) in [("x", &xs, 60.0), ("y", &ys, 20.0)] {
        let c = cluster(vals, tol);
        println!(
            "  {axis}: {} clusters at tolerance {tol:.0} -> {:?}",
            c.len(),
            c.iter().map(|v| *v as i64).collect::<Vec<_>>()
        );
    }

    println!("\n== the temporal test ==");
    println!("  Records are the outer loop, fields the inner. The RECORD axis should");
    println!("  cut the step sequence into blocks that do not interleave.\n");
    for (label, use_y, tol) in [("y", true, 20.0), ("x", false, 60.0)] {
        match overlap_fraction(&source, use_y, tol) {
            Some((frac, n)) => println!(
                "  grouping by {label}: {n} clusters, overlapping pairs {:.0}%   {}",
                frac * 100.0,
                if frac == 0.0 {
                    "<- CONTIGUOUS (candidate record axis)"
                } else if frac >= 0.9 {
                    "<- fully interleaved (candidate field axis)"
                } else {
                    "<- NEITHER: not a clean signal"
                }
            ),
            None => println!("  grouping by {label}: fewer than 2 clusters, cannot judge"),
        }
    }

    println!("\n== what was actually clicked, in step order ==");
    println!("  Read this to see whether the task was worked record-by-record or");
    println!("  field-by-field. The rule assumes the former; the latter would invert");
    println!("  the answer and is the honest failure mode.\n");
    let mut ordered: Vec<&&El> = source.iter().collect();
    ordered.sort_by_key(|e| e.step);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for e in ordered {
        let first = seen.insert(e.name.clone());
        if first {
            println!(
                "  step {:>4}  {:<5} x{:>6.0} y{:>6.0}  {}",
                e.step,
                e.kind,
                e.x,
                e.y,
                e.name.chars().take(44).collect::<String>()
            );
        }
    }
}
