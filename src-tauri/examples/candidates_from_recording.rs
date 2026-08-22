//! Re-run `detect::candidates` over a SAVED recording, and print the funnel.
//!
//!     cargo run --example candidates_from_recording -- <playbook_id> <app_data_dir>
//!
//! `stop_record_session` logs how many candidates it found and not where the
//! rest went, so a session that surfaces two candidates from 231 actions cannot
//! be explained from the log alone. This reconstructs the action stream from the
//! stored steps and runs the same function, so a zero can be attributed to a
//! stage instead of guessed at.
//!
//! **What is reconstructed, and what is not.** The stored payload carries the
//! action type, the element name and the bounds -- everything `candidates`
//! reads. It does not carry the identifiers or the process name, which the
//! filter never looks at. So the funnel is exact for this purpose and the
//! rebuilt actions are not a faithful copy of what capture held.
//!
//! Only sound when the recording was saved with every step kept. A review that
//! deleted steps stores fewer than were captured, and the funnel then describes
//! what survived review rather than what was recorded.

use std::path::PathBuf;

use paradigm_lib::capture::exclusion::ExclusionList;
use paradigm_lib::capture::stream::{ActionCandidate, ActionKind, CapturedStream};
use paradigm_lib::compile::store;
use paradigm_lib::db;
use paradigm_lib::detect::candidates::candidates;

fn usage() -> ! {
    eprintln!("usage: cargo run --example candidates_from_recording -- <playbook_id> <app_data_dir>");
    std::process::exit(1);
}

fn kind_of(s: &str) -> ActionKind {
    match s {
        "type" => ActionKind::Type,
        "navigate" => ActionKind::Navigate,
        "read" => ActionKind::Read,
        _ => ActionKind::Click,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| usage());
    let dir: PathBuf = args.next().map(PathBuf::from).unwrap_or_else(|| usage());

    let (db_path, key_path) = db::paths_in(&dir);
    let conn = db::open(&db_path, &key_path).expect("failed to open db");
    let loaded = store::load(&conn, &id).expect("load failed");

    let mut stream = CapturedStream::new(ExclusionList::from_patterns(Vec::<String>::new()));
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    let mut with_bounds = 0usize;
    let mut with_name = 0usize;

    for s in &loaded.steps {
        let v: serde_json::Value =
            serde_json::from_str(&s.action_payload_json).expect("payload is json");
        let target = &v["target"];
        let name = target["name"].as_str().filter(|n| !n.trim().is_empty());
        let bounds = target["bounds"].as_array().and_then(|a| {
            (a.len() == 4).then(|| {
                (
                    a[0].as_f64().unwrap_or(0.0),
                    a[1].as_f64().unwrap_or(0.0),
                    a[2].as_f64().unwrap_or(0.0),
                    a[3].as_f64().unwrap_or(0.0),
                )
            })
        });
        *kinds.entry(s.action_type.clone()).or_default() += 1;
        if bounds.is_some() {
            with_bounds += 1;
        }
        if name.is_some() {
            with_name += 1;
        }

        stream.admit(ActionCandidate {
            kind: kind_of(&s.action_type),
            identifiers: vec!["reconstructed".into()],
            process_name: Some("reconstructed".into()),
            element_role: target["role"].as_str().map(str::to_string),
            element_name: name.map(str::to_string),
            payload: None,
            detail: None,
            element_bounds: bounds,
            timestamp_ms: s.step_order as u64,
        });
    }

    let actions = stream.actions();
    println!("playbook {:?}  {} step(s) reconstructed\n", loaded.name, actions.len());
    println!("== what the recording holds ==");
    for (k, n) in &kinds {
        println!("  {k:<10} {n}");
    }
    println!("  with a name    {with_name}");
    println!("  with bounds    {with_bounds}");

    let set = candidates(actions);
    let f = set.funnel;
    println!("\n== the funnel ==");
    println!("  stage 0  raw actions                  : {}", f.raw);
    println!(
        "  stage 1  after dropping Navigate      : {}  (-{})",
        f.after_navigate,
        f.raw - f.after_navigate
    );
    println!(
        "  stage 2  with a cell ref or a position: {}  (-{})",
        f.with_identity,
        f.after_navigate - f.with_identity
    );
    println!("  stage 3  distinct field groups        : {}", f.field_groups);
    println!("  stage 4  surviving the Rule of 3      : {}", f.surviving);

    println!("\n== candidates ==");
    for c in &set.groups {
        println!(
            "  {:<8} {:<56} {} record(s), {} action(s)",
            c.id,
            c.detail,
            c.distinct_records,
            c.occurrences()
        );
    }
    if f.raw > 0 {
        println!(
            "\n  survival: {}/{} = {:.2}%",
            set.groups.len(),
            f.raw,
            100.0 * set.groups.len() as f64 / f.raw as f64
        );
    }

    // The page side is the interesting half: spreadsheet columns are grouped by
    // cell reference and cannot fail, so a page-side zero is where the
    // positional rule either worked or declined.
    let page: Vec<&&str> = Vec::new();
    let _ = page;
    let positional = set
        .groups
        .iter()
        .filter(|c| !c.detail.contains("column"))
        .count();
    println!(
        "  of which positional (page-side): {positional}; spreadsheet columns: {}",
        set.groups.len() - positional
    );
    page_half(actions);
}

// Appended diagnostic: the page half on its own.
//
// Spreadsheet columns cannot fail to group -- a cell reference is exact -- so a
// combined funnel hides whether the POSITIONAL rule worked or declined. Running
// the clicks alone separates the two.
#[allow(dead_code)]
fn page_half(actions: &[paradigm_lib::capture::stream::CapturedAction]) {
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(Vec::<String>::new()));
    // Excludes clicks whose name is a CELL REFERENCE: those take the
    // spreadsheet branch and would hide a positional decline behind two column
    // groups, which is exactly what the first run of this diagnostic did.
    for a in actions.iter().filter(|a| {
        a.kind == ActionKind::Click
            && a
                .element_name
                .as_deref()
                .and_then(paradigm_lib::source::spreadsheet::parse_cell_ref)
                .is_none()
    }) {
        stream.admit(ActionCandidate {
            kind: a.kind,
            identifiers: vec!["reconstructed".into()],
            process_name: None,
            element_role: a.element_role.clone(),
            element_name: a.element_name.clone(),
            payload: None,
            detail: None,
            element_bounds: a.element_bounds,
            timestamp_ms: a.timestamp_ms,
        });
    }
    // WHY it declined. Two candidates: no recurring pitch at all, or the
    // collision check firing. Repeated clicks on one position are the obvious
    // suspect, because a person clicking the same thing twice is ordinary.
    let mut xy: std::collections::BTreeMap<(i64, i64), usize> = Default::default();
    for a in stream.actions() {
        if let Some((x, y, _, _)) = a.element_bounds {
            *xy.entry((x as i64, y as i64)).or_default() += 1;
        }
    }
    println!(
        "\n  distinct click positions {}, repeated positions {}, most-clicked hit {}x",
        xy.len(),
        xy.values().filter(|n| **n > 1).count(),
        xy.values().max().copied().unwrap_or(0)
    );

    let set = candidates(stream.actions());
    println!(
        "\n== clicks alone ==\n  {} click(s) -> stage 3 groups {}, surviving {}",
        stream.actions().len(),
        set.funnel.field_groups,
        set.funnel.surviving
    );
    if set.funnel.field_groups == 0 {
        println!("  ZERO groups from clicks: assign_records declined for the whole page side.");
    }
}
