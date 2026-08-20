//! Prototype: filtered post-hoc confirmation.
//!
//! No marker during recording. After Stop, filter the raw action stream down to
//! real candidates and present those for a yes/no.
//!
//! FIXTURE A is REAL: the 37 steps of playbook "d test", transcribed from
//! `dump_playbook` output. Names, roles and step order exactly as stored.
//!
//! FIXTURE B is MODELLED, not captured: the same action shape and the same
//! noise ratio measured from A, extended to three records, because A contains
//! only one record and so cannot exercise the Rule of 3 at all. Labelled
//! throughout so the two are never confused.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Click,
    Type,
    Navigate,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Truth {
    Meaningful,
    Incidental,
    Context,
}

#[derive(Debug, Clone)]
struct Action {
    kind: Kind,
    role: &'static str,
    name: Option<&'static str>,
    /// Positional identity, as CapturedAction::element_bounds now records.
    /// MODELLED for fixture B; fixture A predates bounds capture, so None.
    bounds: Option<(f64,f64,f64,f64)>,
    /// Ground truth, for scoring only. Never an input to the filter.
    truth: Truth,
}

fn a(kind: Kind, role: &'static str, name: Option<&'static str>, truth: Truth) -> Action {
    Action { kind, role, name, bounds: None, truth }
}

fn ab(kind: Kind, role: &'static str, name: Option<&'static str>, b: (f64,f64,f64,f64), truth: Truth) -> Action {
    Action { kind, role, name, bounds: Some(b), truth }
}

/// The real recording, step for step.
fn fixture_a() -> Vec<Action> {
    let mut v = Vec::new();
    v.push(a(Kind::Navigate, "Window", Some("Untitled spreadsheet"), Truth::Context));
    for _ in 0..9 {
        v.push(a(Kind::Click, "group", None, Truth::Incidental));
    }
    v.push(a(Kind::Click, "combobox", Some("A2"), Truth::Meaningful));
    v.push(a(Kind::Type, "combobox", Some("A2"), Truth::Meaningful));
    v.push(a(Kind::Click, "text", Some("\u{feff}Harbor Point"), Truth::Incidental));
    v.push(a(Kind::Click, "text", Some("Harbor Point T"), Truth::Incidental));
    v.push(a(Kind::Type, "combobox", Some("A2"), Truth::Meaningful));
    v.push(a(Kind::Navigate, "Window", Some("Paradigm"), Truth::Context));
    v.push(a(Kind::Navigate, "Window", Some("OrderFlow Dashboard"), Truth::Context));
    for _ in 0..4 {
        v.push(a(Kind::Click, "group", None, Truth::Incidental));
    }
    v.push(a(Kind::Click, "combobox", Some("B2"), Truth::Meaningful));
    v.push(a(Kind::Click, "combobox", Some("B2"), Truth::Meaningful));
    v.push(a(Kind::Type, "combobox", Some("B2"), Truth::Meaningful));
    v.push(a(Kind::Type, "combobox", Some("B2"), Truth::Meaningful));
    v.push(a(Kind::Type, "combobox", Some("B2"), Truth::Meaningful));
    for _ in 0..3 {
        v.push(a(Kind::Click, "text", Some("12"), Truth::Meaningful));
    }
    v.push(a(Kind::Navigate, "Window", Some("OrderFlow Dashboard"), Truth::Context));
    v.push(a(Kind::Click, "text", Some("12"), Truth::Meaningful));
    v.push(a(Kind::Navigate, "Window", Some("Untitled spreadsheet"), Truth::Context));
    for _ in 0..4 {
        v.push(a(Kind::Click, "group", None, Truth::Incidental));
    }
    v.push(a(Kind::Type, "combobox", Some("C2"), Truth::Meaningful));
    v
}

/// Three records, same shape and noise ratio as A. MODELLED.
/// Includes the case proven fatal to structural inference: a systematic
/// incidental click, the customer checked on every record.
fn fixture_b() -> Vec<Action> {
    let mut v = Vec::new();
    let rows = [("A2", "B2", "C2"), ("A3", "B3", "C3"), ("A4", "B4", "C4")];
    let customers = ["Harbor Point Traders", "Ashgrove Manufacturing", "Windmere Consulting"];
    let products = ["Ceramic Mug Set", "Steel Bracket", "Ergonomic Chair"];
    for (i, (ca, cb, cc)) in rows.iter().enumerate() {
        v.push(a(Kind::Navigate, "Window", Some("OrderFlow Dashboard"), Truth::Context));
        for _ in 0..5 {
            v.push(a(Kind::Click, "group", None, Truth::Incidental));
        }
        // SYSTEMATIC INCIDENTAL: the customer, checked on every record.
        let y = 100.0 + (i as f64) * 60.0;
        v.push(ab(Kind::Click, "text", Some(customers[i]), (100.0, y, 300.0, y+20.0), Truth::Incidental));
        v.push(ab(Kind::Click, "text", Some(products[i]), (320.0, y, 520.0, y+20.0), Truth::Meaningful));
        v.push(a(Kind::Navigate, "Window", Some("Untitled spreadsheet"), Truth::Context));
        for _ in 0..4 {
            v.push(a(Kind::Click, "group", None, Truth::Incidental));
        }
        v.push(a(Kind::Click, "combobox", Some(ca), Truth::Meaningful));
        v.push(a(Kind::Type, "combobox", Some(ca), Truth::Meaningful));
        v.push(a(Kind::Click, "combobox", Some(cb), Truth::Meaningful));
        v.push(a(Kind::Type, "combobox", Some(cb), Truth::Meaningful));
        v.push(a(Kind::Type, "combobox", Some(cc), Truth::Meaningful));
    }
    v
}

/// Spreadsheet cell -> (column, row), the existing parse_cell_ref rule.
fn parse_cell(s: &str) -> Option<(String, i64)> {
    let t = s.trim();
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

struct Candidate {
    key: String,
    records: BTreeSet<String>,
    occurrences: usize,
    truths: Vec<Truth>,
}

fn run(label: &str, actions: &[Action], real: bool) -> usize {
    println!(
        "\n============ {label} ({}) ============",
        if real { "REAL CAPTURE" } else { "MODELLED" }
    );
    let raw = actions.len();
    println!("stage 0  raw actions in the stream         : {raw}");

    let s1: Vec<&Action> = actions.iter().filter(|x| x.kind != Kind::Navigate).collect();
    println!(
        "stage 1  after dropping Navigate (context)  : {}  (-{})",
        s1.len(),
        raw - s1.len()
    );

    let s2: Vec<&&Action> = s1
        .iter()
        .filter(|x| x.name.map(|n| !n.trim().is_empty()).unwrap_or(false))
        .collect();
    println!(
        "stage 2  after dropping unidentifiable      : {}  (-{})",
        s2.len(),
        s1.len() - s2.len()
    );

    let mut groups: BTreeMap<String, Candidate> = BTreeMap::new();
    for act in &s2 {
        let name = act.name.unwrap();
        let (key, record) = match parse_cell(name) {
            Some((col, row)) => (format!("{:?} into cell column {col}", act.kind), row.to_string()),
            None => match act.bounds {
                // Positional identity: the x band is the COLUMN, the y band is
                // the RECORD. Values differ per record by design, so the value
                // cannot be the key -- the position can.
                Some((l, t, r, _)) => (
                    format!("{:?} on {} at x{:.0}-{:.0}", act.kind, act.role, l, r),
                    format!("y{:.0}", t),
                ),
                None => (
                    format!("{:?} on {} {:?}", act.kind, act.role, name),
                    name.to_string(),
                ),
            },
        };
        let e = groups.entry(key.clone()).or_insert_with(|| Candidate {
            key,
            records: BTreeSet::new(),
            occurrences: 0,
            truths: Vec::new(),
        });
        e.records.insert(record);
        e.occurrences += 1;
        e.truths.push(act.truth);
    }
    println!("stage 3  distinct field groups              : {}", groups.len());

    let mut candidates: Vec<&Candidate> = groups.values().filter(|c| c.records.len() >= 3).collect();
    candidates.sort_by(|x, y| x.key.cmp(&y.key));
    println!(
        "stage 4  surviving Rule of 3 (>=3 records)  : {}",
        candidates.len()
    );

    println!("\n  --- what the user would be asked to confirm ---");
    if candidates.is_empty() {
        println!("    (nothing)");
    }
    for c in &candidates {
        let m = c.truths.iter().filter(|t| **t == Truth::Meaningful).count();
        let i = c.truths.iter().filter(|t| **t == Truth::Incidental).count();
        println!(
            "    [ ] {:<38} {} records, {} actions   (truth {}M/{}I)",
            c.key,
            c.records.len(),
            c.occurrences,
            m,
            i
        );
    }
    if raw > 0 {
        println!(
            "\n  tedium: {} decisions from {raw} raw actions  ({:.1}% survive)",
            candidates.len(),
            100.0 * candidates.len() as f64 / raw as f64
        );
    }
    candidates.len()
}

fn main() {
    run("FIXTURE A -- playbook \"d test\", 37 steps", &fixture_a(), true);
    println!("\n  NOTE: fixture A holds ONE record (row 2 only). The Rule of 3 is over");
    println!("  RECORDS, so nothing can survive here -- and detect said exactly that");
    println!("  live: \"only 1 record were copied across\". Correct behaviour, not a");
    println!("  filter failure. It is the reason fixture B exists.");

    run("FIXTURE B -- three records, modelled on A", &fixture_b(), false);
}
