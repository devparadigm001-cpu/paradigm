//! Which axis separates RECORDS? Tested against two real layouts of opposite
//! orientation.
//!
//!     cargo run --example layout_axis
//!
//! `row_clustering` hardcodes y as the record axis and x as the field axis,
//! validated on OrderFlow's vertical card list. This asks whether that
//! generalises, using real bounds from a horizontal table where records run
//! left-to-right.
//!
//! Both fixtures are REAL measurements. OrderFlow's come from session
//! record-379e7898 via `confirmation_against_real`; the table's from
//! `page_bounds_probe` against a rendered page. Only OrderFlow carries step
//! numbers, because only it came from a recording.


/// (x, y, name, steps). OrderFlow: a VERTICAL card list, 3 records stacked.
fn orderflow() -> Vec<(f64, f64, &'static str, Vec<i64>)> {
    vec![
        (176.0, 321.0, "Harbor Point Traders", vec![2, 3, 4]),
        (176.0, 405.0, "Ceramic Mug Set", vec![9, 10, 11]),
        (176.0, 535.0, "Ashgrove Manufacturing", vec![48, 51, 52, 53, 54, 55, 56, 57, 58]),
        (176.0, 620.0, "Steel Bracket", vec![61, 62, 63]),
        (176.0, 750.0, "Windmere Consulting", vec![100, 103, 104]),
        (176.0, 834.0, "Ergonomic Office Chair", vec![109, 110, 111]),
        (346.0, 405.0, "12", vec![18, 19, 20]),
        (404.0, 620.0, "40", vec![68, 69, 70]),
        (394.0, 834.0, "2", vec![116, 117]),
        (451.0, 405.0, "$8.50", vec![24, 25, 26]),
        (509.0, 620.0, "$3.25", vec![75, 76, 77]),
        (499.0, 834.0, "$145.00", vec![122, 123, 124]),
        (563.0, 405.0, "$102.00", vec![33, 34, 35]),
        (620.0, 620.0, "$130.00", vec![85, 86, 89, 90, 91]),
        (610.0, 834.0, "$290.00", vec![131, 132, 133, 136, 137, 138]),
        (784.0, 289.0, "Pending", vec![41, 42]),
        (784.0, 504.0, "Pending", vec![94, 95]),
        (784.0, 718.0, "Pending", vec![142, 143]),
    ]
}

/// HZTEST: a HORIZONTAL comparison table, 3 records side by side.
/// No steps -- measured from a rendered page, not a recording.
fn horizontal() -> Vec<(f64, f64, &'static str, Vec<i64>)> {
    let rows = [
        (183.0, "VENDOR", "Harbor Point Traders", "Ashgrove Manufacturing", "Windmere Consulting"),
        (234.0, "PRODUCT", "Ceramic Mug Set", "Steel Bracket", "Ergonomic Office Chair"),
        (284.0, "QUANTITY", "12", "40", "2"),
        (334.0, "UNITPRICE", "8.50", "3.25", "145.00"),
        (385.0, "TOTAL", "102.00", "130.00", "290.00"),
    ];
    let mut v = Vec::new();
    for (y, label, a, b, c) in rows {
        v.push((115.0, y, label, vec![]));
        v.push((239.0, y, a, vec![]));
        v.push((429.0, y, b, vec![]));
        v.push((649.0, y, c, vec![]));
    }
    v
}

/// Cluster 1-D positions by proximity. Returns cluster means.
fn cluster(vals: &[f64], tolerance: f64) -> Vec<f64> {
    let mut v: Vec<f64> = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.dedup();
    let mut clusters: Vec<Vec<f64>> = Vec::new();
    for x in v {
        match clusters.last_mut() {
            Some(c) if x - c[c.len() - 1] <= tolerance => c.push(x),
            _ => clusters.push(vec![x]),
        }
    }
    clusters
        .iter()
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

/// How regular are the gaps between consecutive cluster centres?
/// Returns (mean gap, max deviation as a fraction of the mean).
fn regularity(centres: &[f64]) -> Option<(f64, f64)> {
    if centres.len() < 3 {
        return None;
    }
    let gaps: Vec<f64> = centres.windows(2).map(|w| w[1] - w[0]).collect();
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let dev = gaps
        .iter()
        .map(|g| (g - mean).abs() / mean)
        .fold(0.0f64, f64::max);
    Some((mean, dev))
}

fn report_geometry(label: &str, els: &[(f64, f64, &'static str, Vec<i64>)]) {
    println!("\n================ {label} ================");
    let xs: Vec<f64> = els.iter().map(|e| e.0).collect();
    let ys: Vec<f64> = els.iter().map(|e| e.1).collect();
    let xc = cluster(&xs, 60.0);
    let yc = cluster(&ys, 20.0);
    println!("  distinct x clusters: {} -> {:?}", xc.len(), xc.iter().map(|v| *v as i64).collect::<Vec<_>>());
    println!("  distinct y clusters: {} -> {:?}", yc.len(), yc.iter().map(|v| *v as i64).collect::<Vec<_>>());
    for (axis, c) in [("x", &xc), ("y", &yc)] {
        match regularity(c) {
            Some((mean, dev)) => println!(
                "  {axis} gaps: mean {mean:.0}px, max deviation {:.0}%  {}",
                dev * 100.0,
                if dev < 0.10 { "<- REGULAR" } else { "irregular" }
            ),
            None => println!("  {axis}: too few clusters to judge regularity"),
        }
    }
}

/// Do the clusters on `axis` partition the action stream into CONTIGUOUS
/// blocks of time? Records are the outer loop of what a user does; fields are
/// the inner loop.
fn contiguity(els: &[(f64, f64, &'static str, Vec<i64>)], use_y: bool, tolerance: f64) -> Option<f64> {
    let vals: Vec<f64> = els.iter().map(|e| if use_y { e.1 } else { e.0 }).collect();
    let centres = cluster(&vals, tolerance);
    let mut ranges: Vec<(i64, i64)> = Vec::new();
    for c in &centres {
        let mut steps: Vec<i64> = Vec::new();
        for e in els {
            let v = if use_y { e.1 } else { e.0 };
            if (v - c).abs() <= tolerance {
                steps.extend(e.3.iter().copied());
            }
        }
        if steps.is_empty() {
            continue;
        }
        steps.sort_unstable();
        ranges.push((steps[0], steps[steps.len() - 1]));
    }
    if ranges.len() < 2 {
        return None;
    }
    ranges.sort();
    // Fraction of cluster PAIRS whose step ranges overlap. 0.0 = perfectly
    // contiguous blocks; higher = clusters interleaved through time.
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
    Some(overlaps as f64 / pairs as f64)
}

fn main() {
    let of = orderflow();
    let hz = horizontal();

    report_geometry("OrderFlow -- VERTICAL cards (records stacked)", &of);
    println!("  ground truth: records separate on Y, fields on X");

    report_geometry("HZTEST -- HORIZONTAL table (records side by side)", &hz);
    println!("  ground truth: records separate on X, fields on Y");

    println!("\n================ can GEOMETRY alone pick the record axis? ================");
    println!("  OrderFlow: records are on Y, and Y measured as the IRREGULAR axis (54%).");
    println!("  HZTEST:    records are on X, and X measured as the IRREGULAR axis (30%).");
    println!();
    println!("  So the naive rule -- 'the axis with a regular pitch holds the records'");
    println!("  -- is WRONG on both layouts. Inverting it to 'irregular = records'");
    println!("  fits these two points, but for unrelated reasons: OrderFlow's Y is");
    println!("  irregular because it interleaves record pitch (214) with field offsets");
    println!("  (32, 84), while HZTEST's X is irregular because column widths follow");
    println!("  their content. Two coincidences with different causes is not a rule.");
    println!();
    println!("  Caveat on OrderFlow's 'x regular 3%': at tolerance 60 the drifting");
    println!("  columns (346..620) chain into ONE cluster, so that 3% describes an");
    println!("  over-merge, not real structure. It does not rescue the geometric rule.");

    println!("\n================ can TIME pick it? ================");
    println!("  Records are the outer loop of a task; fields the inner loop. So the");
    println!("  record axis should partition the step sequence into blocks that do");
    println!("  not interleave.\n");
    for (label, use_y) in [("y (ground truth: records)", true), ("x (ground truth: fields)", false)] {
        match contiguity(&of, use_y, if use_y { 20.0 } else { 60.0 }) {
            Some(o) => println!(
                "  OrderFlow, grouping by {label:<28} overlapping cluster pairs: {:.0}%  {}",
                o * 100.0,
                if o == 0.0 { "<- CONTIGUOUS" } else { "" }
            ),
            None => println!("  OrderFlow, grouping by {label}: not enough clusters"),
        }
    }
    println!("\n  HZTEST carries no step numbers -- it was measured from a rendered");
    println!("  page, not a recording -- so the temporal test cannot be run on it.");
    println!("  Confirming this on a horizontal layout needs a real recording.");
}
