//! §3 as its own binary: no captured value may reach a candidate.
//!
//! Split out from `candidate_axes` because this machine's Application Control
//! policy refuses newly-produced binaries, and a smaller separate file is one
//! more roll of that dice. The assertion is the one from
//! `detect::candidates::tests::a_candidate_carries_no_captured_value`, which the
//! lib test binary has been unable to run since the `window` field was added.

use paradigm_lib::capture::exclusion::ExclusionList;
use paradigm_lib::capture::stream::{ActionCandidate, ActionKind, CapturedStream};
use paradigm_lib::detect::candidates::candidates;

#[test]
fn a_candidate_carries_no_captured_value() {
    const W: &str = "win/test";
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(Vec::<String>::new()));
    for (name, y) in [
        ("Harbor Point Traders", 321.0),
        ("Ashgrove Manufacturing", 535.0),
        ("Windmere Consulting", 749.0),
    ] {
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
    for value in [
        "Harbor Point Traders",
        "Ashgrove Manufacturing",
        "Windmere Consulting",
        "a secret value",
    ] {
        assert!(!rendered.contains(value), "a captured value reached the output: {value:?}");
    }
}
