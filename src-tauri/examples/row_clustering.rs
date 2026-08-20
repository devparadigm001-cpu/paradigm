//! Two-dimensional field clustering: fix for the x-banding failure measured on
//! 2026-08-20 against session record-379e7898.
//!
//!     cargo run --example row_clustering
//!
//! x-banding alone failed in two opposite ways on real layout:
//!
//!   * MERGING -- every left-column value shares x=176, customer and product
//!     alike, because a card stacks its fields vertically. One group claimed
//!     "6 records" for what is 3 records x 2 fields.
//!   * SPLITTING -- numeric columns drift with content. Quantity sat at
//!     x=346/404/394 across three records, price at 451/509/499, total at
//!     563/620/610. Intra-column drift ~58px, inter-column gap ~47-54px, so no
//!     1-D tolerance separates them.
//!
//! The fix uses the structure the same capture revealed:
//!
//!   1. RECORD PITCH from the modal pairwise y-difference. Naive gap-clustering
//!      does NOT work here -- the within-record spread (116px) is larger than
//!      the between-record gap (99px) -- but 214/215 recurs six times and is
//!      unmistakable.
//!   2. RECORD INDEX = floor((y - y_min) / pitch).
//!   3. ROW within the record = y - record_top, banded. This separates customer
//!      (offset ~32) from product (offset ~116) at identical x.
//!   4. FIELD = the element's x RANK within its row, not its absolute x. Within
//!      any single record the columns are cleanly separated (~105-170px apart);
//!      only their absolute positions drift between records. Rank is immune to
//!      that drift.
//!
//! Step 4 is the same idea `an-element-identity-mark-records-no-field-label.md`
//! named as the real direction: identify by position within the record, and
//! treat the name as cosmetic.

use std::collections::BTreeMap;

/// Real captured bounds from session record-379e7898, transcribed from the
/// `confirmation_against_real` output. (x, y, width, name, truth).
fn real_page_elements() -> Vec<(f64, f64, f64, &'static str, &'static str)> {
    vec![
        (176.0, 321.0, 201.0, "Harbor Point Traders", "customer"),
        (176.0, 405.0, 137.0, "Ceramic Mug Set", "product"),
        (176.0, 535.0, 241.0, "Ashgrove Manufacturing", "customer"),
        (176.0, 620.0, 195.0, "Steel Bracket (box of 50)", "product"),
        (176.0, 750.0, 212.0, "Windmere Consulting", "customer"),
        (176.0, 834.0, 185.0, "Ergonomic Office Chair", "product"),
        (346.0, 405.0, 19.0, "12", "quantity"),
        (404.0, 620.0, 22.0, "40", "quantity"),
        (394.0, 834.0, 12.0, "2", "quantity"),
        (451.0, 405.0, 45.0, "$8.50", "unit price"),
        (509.0, 620.0, 45.0, "$3.25", "unit price"),
        (499.0, 834.0, 62.0, "$145.00", "unit price"),
        (563.0, 405.0, 67.0, "$102.00", "total"),
        (620.0, 620.0, 67.0, "$130.00", "total"),
        (610.0, 834.0, 68.0, "$290.00", "total"),
        (784.0, 289.0, 60.0, "Pending", "status"),
        (784.0, 504.0, 60.0, "Pending", "status"),
        (784.0, 718.0, 60.0, "Pending", "status"),
        (1403.0, 1020.0, 56.0, "paradigm - 1 running window", "OUTLIER (taskbar)"),
    ]
}

/// The record pitch: the most common pairwise y-difference above a floor.
///
/// Returns `None` when no difference recurs, which is the honest answer for a
/// page that is not a repeating list.
fn record_pitch(ys: &[f64], tolerance: f64, floor: f64) -> Option<f64> {
    let mut diffs: Vec<f64> = Vec::new();
    for (i, a) in ys.iter().enumerate() {
        for b in ys.iter().skip(i + 1) {
            let d = (b - a).abs();
            if d >= floor {
                diffs.push(d);
            }
        }
    }
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Proximity clustering, NOT fixed-width buckets. Rounding 214 and 215 into
    // adjacent buckets split the true pitch in half and let its own 2x harmonic
    // tie with it and win -- measured, 2026-08-20.
    let mut clusters: Vec<Vec<f64>> = Vec::new();
    for d in diffs {
        match clusters.last_mut() {
            Some(c) if d - c[c.len() - 1] <= tolerance => c.push(d),
            _ => clusters.push(vec![d]),
        }
    }

    let best = clusters.iter().filter(|c| c.len() >= 2).map(|c| c.len()).max()?;
    // Among equally-supported clusters take the SMALLEST. A repeating list
    // always produces harmonics -- with N records, the pitch appears N-1 times
    // and 2x the pitch N-2 times -- and the fundamental is the one that is the
    // actual record spacing.
    clusters
        .iter()
        .filter(|c| c.len() == best)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .min_by(|a, b| a.partial_cmp(b).unwrap())
}

fn main() {
    let els = real_page_elements();
    let ys: Vec<f64> = els.iter().map(|e| e.1).collect();

    let mut sorted = ys.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sorted.dedup();
    println!("distinct y values: {sorted:?}");
    let diffs: Vec<f64> = sorted.windows(2).map(|w| w[1] - w[0]).collect();
    println!("consecutive gaps  : {diffs:?}");
    println!("  note: within-record spread (116) EXCEEDS the between-record gap (99),");
    println!("  so naive gap-clustering cannot work. The pitch must come from");
    println!("  periodicity instead.\n");

    let pitch = record_pitch(&ys, 10.0, 120.0).expect("a repeating list has a pitch");
    println!("record pitch (modal pairwise y-difference): {pitch:.1}px\n");

    let y_min = sorted[0];
    const ROW_TOL: f64 = 20.0;

    // record -> row-offset band -> elements, so x can be ranked within a row.
    let mut rows: BTreeMap<(i64, i64), Vec<(f64, &str, &str)>> = BTreeMap::new();
    for (x, y, _w, name, truth) in &els {
        let record = ((y - y_min) / pitch).floor() as i64;
        let record_top = y_min + record as f64 * pitch;
        let offset = y - record_top;
        let band = (offset / ROW_TOL).round();
        rows.entry((record, band as i64))
            .or_default()
            .push((*x, name, truth));
    }

    println!("== records and rows discovered ==");
    for ((record, band), items) in &rows {
        let mut sorted_items = items.clone();
        sorted_items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        println!(
            "  record {record}  row-offset ~{:>3}px : {}",
            (*band as f64 * ROW_TOL) as i64,
            sorted_items
                .iter()
                .map(|(x, n, _)| format!("x{:.0} {:?}", x, n.chars().take(16).collect::<String>()))
                .collect::<Vec<_>>()
                .join("  ")
        );
    }

    // FIELD = (row-offset band, x rank within the row).
    let mut fields: BTreeMap<(i64, usize), Vec<(i64, &str, &str)>> = BTreeMap::new();
    for ((record, band), items) in &rows {
        let mut sorted_items = items.clone();
        sorted_items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (rank, (_x, name, truth)) in sorted_items.iter().enumerate() {
            fields
                .entry((*band, rank))
                .or_default()
                .push((*record, name, truth));
        }
    }

    println!("\n== fields, keyed by (row offset, x rank) ==");
    println!("   {:<26} {:>7}  {:<22} {}", "key", "records", "ground truth", "values");
    let mut survived = 0;
    for ((band, rank), items) in &fields {
        let records: std::collections::BTreeSet<i64> = items.iter().map(|(r, _, _)| *r).collect();
        let truths: std::collections::BTreeSet<&str> =
            items.iter().map(|(_, _, t)| *t).collect();
        let key = format!("row~{}px, x-rank {}", (*band as f64 * ROW_TOL) as i64, rank);
        let vals: Vec<String> = items
            .iter()
            .map(|(_, n, _)| n.chars().take(14).collect())
            .collect();
        let mark = if records.len() >= 3 {
            survived += 1;
            "<- CANDIDATE"
        } else {
            ""
        };
        println!(
            "   {:<26} {:>7}  {:<22} {} {}",
            key,
            records.len(),
            truths.iter().cloned().collect::<Vec<_>>().join("/"),
            vals.join(", "),
            mark
        );
    }

    println!("\n== verdict ==");
    println!("   fields surviving the Rule of 3: {survived}");
    println!("   Every candidate must map to exactly ONE ground-truth field for the");
    println!("   fix to have worked -- a mixed row means the merge is still there.");
}
