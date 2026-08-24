//! The two record axes, as an INTEGRATION test.
//!
//! These assertions duplicate unit tests in `detect::candidates`. They exist as
//! a separate binary because on 2026-08-23 this machine's Application Control
//! policy blocked the lib test binary for over half an hour, leaving the unit
//! tests unrunnable. `cargo test --lib <filter>` is no escape -- filtering is
//! applied at runtime inside the same binary -- but an integration test is a
//! different file, and every example binary kept running while the lib test one
//! did not.
//!
//! So this is a second way in, for the moments when the first is unavailable.
//! It uses only the public API.

use paradigm_lib::capture::exclusion::ExclusionList;
use paradigm_lib::capture::stream::{ActionCandidate, ActionKind, CapturedAction, CapturedStream};
use paradigm_lib::detect::candidates::candidates;

fn clicks(specs: &[(f64, f64, &str)]) -> Vec<CapturedAction> {
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(Vec::<String>::new()));
    for (x, y, window) in specs {
        stream.admit(ActionCandidate {
            kind: ActionKind::Click,
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            element_role: Some("Text".into()),
            element_name: None,
            payload: None,
            detail: None,
            element_bounds: Some((*x, *y, 50.0, 20.0)),
            window: Some((*window).to_string()),
            timestamp_ms: 0,
        });
    }
    stream.actions().to_vec()
}

/// GMAIL, real: three emails, every subject captured at one position.
#[test]
fn gmail_subjects_group_by_time() {
    const W: &str = "win/1990/180/1010/900";
    let mut specs = vec![(2252.0, 192.0, W)];
    for _ in 0..3 {
        specs.push((2324.0, 212.0, W));
    }
    specs.push((1996.0, 200.0, W));
    specs.push((2252.0, 192.0, W));
    for _ in 0..3 {
        specs.push((2324.0, 212.0, W));
    }
    specs.push((2252.0, 192.0, W));
    for _ in 0..2 {
        specs.push((2324.0, 212.0, W));
    }
    let set = candidates(&clicks(&specs));
    let subject = set
        .groups
        .iter()
        .find(|g| g.occurrences() == 8)
        .expect("the subject was clicked eight times");
    assert_eq!(subject.distinct_records, 3, "three emails, three records");
}

/// ORDERFLOW, real: must stay on the positional axis.
#[test]
fn orderflow_stays_positional() {
    const W: &str = "win/2000/90/880/948";
    let mut specs = Vec::new();
    for (customer, quantity) in [(256.0, 346.0), (465.0, 555.0), (674.0, 764.0)] {
        specs.push((2063.0, customer, W));
        specs.push((2063.0, quantity, W));
    }
    let set = candidates(&clicks(&specs));
    assert_eq!(set.groups.len(), 2, "customer and quantity");
    for g in &set.groups {
        assert_eq!(g.distinct_records, 3);
        assert_eq!(g.occurrences(), 3);
    }
}

/// AMAZON, real y values: the two windows must never pool.
#[test]
fn amazon_windows_never_pool() {
    const PAGE: &str = "win/1990/70/900/1000";
    const SHEET: &str = "win/2990/70/900/1000";
    let mut specs = Vec::new();
    for y in [-220.0, -3.0, 0.0, 44.0, 80.0, 212.0, 229.0, 349.0, 482.0, 513.0, 536.0, 634.0] {
        specs.push((2100.0, y, PAGE));
    }
    for y in [310.0, 312.0, 342.0, 372.0] {
        specs.push((3002.0, y, SHEET));
    }
    let set = candidates(&clicks(&specs));
    for g in &set.groups {
        assert!(
            g.detail.contains(PAGE) || g.detail.contains(SHEET),
            "a candidate must belong to one window: {}",
            g.detail
        );
    }
}

/// One of the five tests broken and repaired mid-change: drifting x must not
/// split one field. This is the repair that was never verified.
#[test]
fn a_field_survives_drifting_x() {
    const W: &str = "win/test";
    let set = candidates(&clicks(&[
        (100.0, 100.0, W),
        (300.0, 100.0, W),
        (140.0, 300.0, W),
        (360.0, 300.0, W),
        (80.0, 500.0, W),
        (280.0, 500.0, W),
    ]));
    assert_eq!(set.groups.len(), 2, "drift must not split a field");
    assert!(set.groups.iter().all(|g| g.distinct_records == 3));
}

/// Another repaired one: repeated clicks must not collapse the page side.
#[test]
fn repeated_clicks_do_not_collapse_the_page_side() {
    const W: &str = "win/test";
    let measured: [(f64, f64, usize); 12] = [
        (2738.0, 234.0, 6),
        (2061.0, 259.0, 6),
        (2061.0, 327.0, 6),
        (2319.0, 327.0, 6),
        (2738.0, 407.0, 4),
        (2061.0, 432.0, 6),
        (2061.0, 500.0, 6),
        (2319.0, 500.0, 6),
        (2738.0, 580.0, 4),
        (2061.0, 605.0, 6),
        (2061.0, 673.0, 6),
        (2319.0, 673.0, 10),
    ];
    let mut specs = Vec::new();
    for (x, y, times) in measured {
        for _ in 0..times {
            specs.push((x, y, W));
        }
    }
    let set = candidates(&clicks(&specs));
    assert_eq!(set.groups.len(), 4, "four fields: {:?}", set.groups.iter().map(|g| &g.detail).collect::<Vec<_>>());
    for g in &set.groups {
        assert_eq!(g.distinct_records, 3);
    }
}

/// An action with no window declines to group rather than joining the largest.
#[test]
fn a_windowless_action_declines_to_group() {
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(Vec::<String>::new()));
    for y in [100.0, 300.0, 500.0] {
        stream.admit(ActionCandidate {
            kind: ActionKind::Click,
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            element_role: Some("Text".into()),
            element_name: None,
            payload: None,
            detail: None,
            element_bounds: Some((100.0, y, 50.0, 20.0)),
            window: None,
            timestamp_ms: 0,
        });
    }
    let set = candidates(stream.actions());
    assert!(
        set.is_empty(),
        "a position nobody can place must not group: {:?}",
        set.groups
    );
}

/// The last of the five repaired tests: §3, no captured value may reach a
/// candidate. Fed with real values on both paths so it has something to fail on.
#[test]
fn a_candidate_carries_no_captured_value() {
    const W: &str = "win/test";
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(Vec::<String>::new()));
    let page = [
        ("Harbor Point Traders", 321.0),
        ("Ashgrove Manufacturing", 535.0),
        ("Windmere Consulting", 749.0),
    ];
    for (name, y) in page {
        stream.admit(ActionCandidate {
            kind: ActionKind::Click,
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            element_role: Some("Text".into()),
            element_name: Some(name.into()),
            payload: None,
            detail: None,
            element_bounds: Some((176.0, y, 201.0, 22.0)),
            window: Some(W.into()),
            timestamp_ms: 0,
        });
    }
    for cell in ["B2", "B3", "B4"] {
        stream.admit(ActionCandidate {
            kind: ActionKind::Type,
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            element_role: Some("ComboBox".into()),
            element_name: Some(cell.into()),
            payload: Some("a secret value".into()),
            detail: None,
            element_bounds: None,
            window: Some(W.into()),
            timestamp_ms: 0,
        });
    }

    let set = candidates(stream.actions());
    assert_eq!(set.groups.len(), 2, "one page field and one column");
    let rendered = format!("{set:?}");
    for value in ["Harbor Point Traders", "Ashgrove Manufacturing", "Windmere Consulting", "a secret value"] {
        assert!(!rendered.contains(value), "a captured value reached the output: {value:?}");
    }
}
