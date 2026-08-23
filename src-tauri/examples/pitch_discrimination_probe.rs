//! Can a record pitch be found WITHOUT a fixed pixel floor?
//!
//!     cargo run --example pitch_discrimination_probe
//!
//! `RECORD_PITCH_FLOOR_PX = 120` is structurally unable to do its job. Proven
//! 2026-08-22: on any list denser than the floor it returns `ceil(F/p)*p`, a
//! harmonic, deterministically -- and it cannot simply be lowered, because it
//! exists to stop within-record field offsets winning the vote. The two failures
//! pull in opposite directions and one magnitude cannot separate them.
//!
//! This tries a different discriminator and scores it against REAL measured
//! layouts, including the one the floor was introduced for.
//!
//! ## The idea
//!
//! A repeating list's y values are a union of arithmetic progressions that all
//! share one common difference `p` -- field `i` of record `n` sits at
//! `base_i + n*p`. So the SET is periodic with period `p`.
//!
//! A within-record offset is not a period of the set. Shifting every y by 90px
//! does not land the set on itself; shifting by the record pitch does.
//!
//! **coverage(p)** = of the points that could map (`y + p <= max`), the fraction
//! whose image `y + p` is also a point.
//!
//! Then take the SMALLEST p whose coverage clears a threshold. Smallest defeats
//! harmonics for free: coverage(p) >= coverage(k*p), so a fundamental that
//! passes is always preferred, and **a harmonic can never be returned while its
//! fundamental passes**. That is the proven defect, gone by construction.
//!
//! ## Why this is a better KIND of constant
//!
//! The floor is a length. Page densities vary by a factor of ten, so no length
//! is right for all of them. Coverage is a ratio in [0, 1] and is scale-free: it
//! does not change when the same layout is rendered twice as large.

use paradigm_lib::detect::candidates::RECORD_PITCH_TOLERANCE_PX;

/// The magnitude floor as it stood before 2026-08-23, kept LOCAL to this probe
/// so the comparison against the old behaviour survives its removal from the
/// library.
const RECORD_PITCH_FLOOR_PX: f64 = 120.0;

/// Today's rule, mirrored, so both run against the same fixtures.
fn shipped_pitch(ys: &[f64]) -> Option<f64> {
    let mut diffs: Vec<f64> = Vec::new();
    for (i, a) in ys.iter().enumerate() {
        for b in ys.iter().skip(i + 1) {
            let d = (b - a).abs();
            if d >= RECORD_PITCH_FLOOR_PX {
                diffs.push(d);
            }
        }
    }
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut clusters: Vec<Vec<f64>> = Vec::new();
    for d in diffs {
        match clusters.last_mut() {
            Some(c) if d - c[c.len() - 1] <= RECORD_PITCH_TOLERANCE_PX => c.push(d),
            _ => clusters.push(vec![d]),
        }
    }
    let best = clusters.iter().filter(|c| c.len() >= 2).map(|c| c.len()).max()?;
    clusters
        .iter()
        .filter(|c| c.len() == best)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .min_by(|a, b| a.partial_cmp(b).unwrap())
}

/// Tolerance for "this y is that y shifted by p".
///
/// **Scaled to p, not fixed.** A flat 10px is a third of a 30px pitch and would
/// let a small candidate match almost anything, which is how a fixed tolerance
/// reintroduces the fixed-magnitude problem through the back door.
fn tol(p: f64) -> f64 {
    (0.15 * p).min(RECORD_PITCH_TOLERANCE_PX).max(1.0)
}

/// Of the points that could map under a shift of `p`, the fraction that do.
fn coverage(sorted: &[f64], p: f64) -> f64 {
    let max = *sorted.last().unwrap_or(&0.0);
    let t = tol(p);
    let mut eligible = 0usize;
    let mut mapped = 0usize;
    for y in sorted {
        if y + p > max + t {
            continue;
        }
        eligible += 1;
        if sorted.iter().any(|z| (z - (y + p)).abs() <= t) {
            mapped += 1;
        }
    }
    if eligible == 0 {
        0.0
    } else {
        mapped as f64 / eligible as f64
    }
}

/// How many distinct within-record offsets a candidate period implies.
///
/// The second discriminator, and the one that separates a TRUE period from a
/// near-miss that coverage alone lets through.
///
/// Under the real pitch every record presents the same field offsets, so
/// `(y - y_min) mod p` collapses onto one cluster per field -- seven fields,
/// seven clusters. Under a period that is close but wrong, the offsets drift by
/// the error on every record, so they smear into many more clusters. Fewer
/// clusters means the period explains the layout more economically.
fn offset_clusters(sorted: &[f64], p: f64) -> usize {
    let y_min = sorted[0];
    let mut offs: Vec<f64> = sorted.iter().map(|y| (y - y_min) % p).collect();
    offs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let t = tol(p);
    let mut n = 0usize;
    let mut last: Option<f64> = None;
    for o in offs {
        match last {
            // Wraps around: an offset just under p is the same cluster as 0.
            Some(l) if o - l <= t => {}
            _ => n += 1,
        }
        last = Some(o);
    }
    n
}

/// The proposal: the period that explains the layout most economically.
///
/// Three filters, each rejecting a failure this probe actually measured:
///
/// 1. **coverage** >= threshold -- rejects within-record offsets, which score
///    0.30-0.60 against 1.00 for a true pitch;
/// 2. **at least three blocks** -- rejects a huge period over a handful of
///    scattered points, which trivially "covers" because almost nothing is
///    eligible to map. The Rule of 3 wants three records anyway, so a period
///    that cannot produce three is useless even when it is real;
/// 3. **fewest offset clusters**, then smallest p -- rejects a near-miss period
///    that coverage lets through.
fn proposed_pitch(ys: &[f64], threshold: f64) -> Option<f64> {
    let mut sorted = ys.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sorted.dedup();
    if sorted.len() < 3 {
        return None;
    }
    let span = sorted[sorted.len() - 1] - sorted[0];

    // Candidates are the observed differences: nothing else can be a period.
    let mut cands: Vec<f64> = Vec::new();
    for (i, a) in sorted.iter().enumerate() {
        for b in sorted.iter().skip(i + 1) {
            cands.push(b - a);
        }
    }
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap());
    cands.dedup_by(|a, b| (*a - *b).abs() <= 1.0);

    let mut viable: Vec<(usize, f64)> = cands
        .into_iter()
        .filter(|&p| p > 0.0 && p <= span / 2.0)
        .filter(|&p| coverage(&sorted, p) >= threshold)
        .map(|p| (offset_clusters(&sorted, p), p))
        .collect();
    // Fewest clusters first, then smallest period. Smallest is what keeps a
    // harmonic from ever winning against its own fundamental.
    viable.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap()));
    viable.first().map(|(_, p)| *p)
}

struct Case {
    name: &'static str,
    ys: Vec<f64>,
    truth: f64,
    note: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        // REAL, page_bounds_probe: every named element of three order cards.
        // The layout the floor was introduced for -- within-record spread 135px
        // EXCEEDS the 120px floor, so its own offsets already clear it.
        Case {
            name: "OrderFlow, all elements",
            ys: vec![
                211.0, 235.0, 256.0, 286.0, 301.0, 331.0, 346.0, 420.0, 444.0, 465.0, 495.0,
                510.0, 540.0, 555.0, 629.0, 653.0, 674.0, 704.0, 719.0, 749.0, 764.0,
            ],
            truth: 209.0,
            note: "the hard case: within-record spread 135px, pitch 209px",
        },
        // REAL, the two fields a user actually clicked in that session.
        Case {
            name: "OrderFlow, clicked fields only",
            ys: vec![256.0, 346.0, 465.0, 555.0, 674.0, 764.0],
            truth: 209.0,
            note: "sparse observation of the same layout",
        },
        // REAL, pitch_candidate_probe against File Explorer.
        Case {
            name: "File Explorer, Projects",
            ys: (0..12).map(|i| 300.0 + 32.0 * i as f64).collect(),
            truth: 32.0,
            note: "dense list, 32px -- the harmonic case",
        },
        // REAL, ftp.gnu.org directory index.
        Case {
            name: "gnu.org directory index",
            ys: (0..20).map(|i| 250.0 + 26.0 * i as f64).collect(),
            truth: 26.0,
            note: "dense list, 26px",
        },
        // REAL, the Wikipedia population table.
        Case {
            name: "Wikipedia table rows",
            ys: vec![315.0, 349.0, 382.0, 415.0, 449.0, 482.0, 515.0],
            truth: 33.0,
            note: "real <table>, 33px",
        },
        // SYNTHETIC, and deliberately adversarial: two-line rows, the lines 16px
        // apart and the rows 32px apart. The y set is then uniformly 16px and
        // genuinely periodic at BOTH 16 and 32. Nothing in y alone can choose.
        Case {
            name: "two-line rows (ambiguous)",
            ys: (0..16).map(|i| 200.0 + 16.0 * i as f64).collect(),
            truth: 32.0,
            note: "y alone cannot decide; needs x",
        },
        // Not a list at all. The honest answer is nothing.
        Case {
            name: "not a repeating list",
            ys: vec![100.0, 137.0, 611.0, 1290.0],
            truth: 0.0,
            note: "must return None",
        },
    ]
}

fn main() {
    println!("== pitch discrimination, against real measured layouts ==\n");
    println!("threshold on coverage: 0.80\n");
    println!(
        "  {:<32} {:>7}  {:>12}  {:>12}",
        "layout", "truth", "shipped", "proposed"
    );
    println!("  {}", "-".repeat(68));

    let mut shipped_right = 0;
    let mut proposed_right = 0;
    let all = cases();
    for c in &all {
        let s = shipped_pitch(&c.ys);
        let p = proposed_pitch(&c.ys, 0.80);
        let ok = |v: Option<f64>| -> (String, bool) {
            match (v, c.truth) {
                (None, t) if t == 0.0 => ("None".into(), true),
                (None, _) => ("None".into(), false),
                (Some(v), t) if t == 0.0 => (format!("{v:.0}px"), false),
                (Some(v), t) => (format!("{v:.0}px"), (v - t).abs() <= 2.0),
            }
        };
        let (sd, sok) = ok(s);
        let (pd, pok) = ok(p);
        if sok {
            shipped_right += 1;
        }
        if pok {
            proposed_right += 1;
        }
        println!(
            "  {:<32} {:>7}  {:>10} {}  {:>10} {}",
            c.name,
            if c.truth == 0.0 {
                "none".to_string()
            } else {
                format!("{:.0}px", c.truth)
            },
            sd,
            if sok { "ok" } else { "XX" },
            pd,
            if pok { "ok" } else { "XX" }
        );
    }
    println!("\n  shipped  {shipped_right}/{} correct", all.len());
    println!("  proposed {proposed_right}/{} correct", all.len());

    println!("\n== why each shipped answer is what it is ==");
    for c in &all {
        if let Some(s) = shipped_pitch(&c.ys) {
            if c.truth > 0.0 && (s - c.truth).abs() > 2.0 {
                println!(
                    "  {:<32} {:.0}px = {:.0}x the true {:.0}px  ({})",
                    c.name,
                    s,
                    s / c.truth,
                    c.truth,
                    c.note
                );
            }
        }
    }

    println!("\n== coverage curves, to show the discrimination is real ==");
    for c in &all {
        if c.truth == 0.0 {
            continue;
        }
        let mut sorted = c.ys.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sorted.dedup();
        println!("\n  {}  ({})", c.name, c.note);
        let mut probes: Vec<f64> = vec![c.truth, c.truth * 2.0, c.truth * 4.0];
        // Add the within-record offsets, which are the things that must score low.
        for w in sorted.windows(2).take(4) {
            let d = w[1] - w[0];
            if (d - c.truth).abs() > 2.0 {
                probes.push(d);
            }
        }
        probes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        probes.dedup_by(|a, b| (*a - *b).abs() < 1.0);
        for p in probes {
            let cov = coverage(&sorted, p);
            let label = if (p - c.truth).abs() <= 2.0 {
                "<- the true pitch"
            } else if (p / c.truth).fract() < 0.05 && p > c.truth {
                "<- harmonic"
            } else {
                "<- within-record offset"
            };
            println!("    p={p:>6.0}px  coverage {:.2}  {label}", cov);
        }
    }
}
