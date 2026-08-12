//! Deterministic verification that a redaction halt is recorded as `aborted`.
//!
//! The replay probe can only demonstrate this when the recorder happens to
//! capture a password field, and that turned out to be flaky -- it fired once
//! in four live runs. This test builds the same playbook through the real
//! compile path, stores it, replays it, and reads the outcome back from SQLite,
//! with no dependency on capture.
//!
//! It also proves the schema actually ACCEPTS `'aborted'` for both
//! `runs.status` and `run_steps_log.event_type`. Those values are permitted by
//! the CHECK constraints in migration 20260803000002, but permitted and
//! successfully written are different claims.

use paradigm_lib::capture::{ActionCandidate, ActionKind, CapturedAction, CapturedStream, ExclusionList};
use paradigm_lib::compile::{compile, store, ReversibilityPolicy};
use paradigm_lib::db;
use paradigm_lib::labeling::RedactionPolicy;
use paradigm_lib::replay::{self, journal, StepResult};
use tempfile::TempDir;
use terminator::Desktop;

/// A session whose FIRST step types into a password field. Replay must halt
/// there, so no Terminator action is ever attempted and the test has no effect
/// on the desktop it runs on.
fn actions_with_leading_secret() -> Vec<CapturedAction> {
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));

    stream.admit(ActionCandidate {
        kind: ActionKind::Type,
        identifiers: vec!["msedge.exe".into()],
        process_name: Some("msedge.exe".into()),
        element_role: Some("Edit".into()),
        element_name: Some("Password".into()),
        payload: Some("hunter2-not-a-real-secret".into()),
        detail: None,
        timestamp_ms: 1,
    });

    // A second step that must NOT be attempted once the first halts.
    stream.admit(ActionCandidate {
        kind: ActionKind::Click,
        identifiers: vec!["msedge.exe".into()],
        process_name: Some("msedge.exe".into()),
        element_role: Some("Button".into()),
        element_name: Some("Cancel".into()),
        payload: None,
        detail: None,
        timestamp_ms: 2,
    });

    stream.actions().to_vec()
}

#[tokio::test]
async fn redaction_halt_is_recorded_as_aborted_not_failed() {
    let dir = TempDir::new().expect("temp dir");
    let (db_path, key_path) = db::paths_in(dir.path());
    let mut conn = db::open(&db_path, &key_path).expect("open encrypted db");

    let playbook = compile(
        &actions_with_leading_secret(),
        "Sign In",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    assert!(
        playbook.steps[0].payload_redacted,
        "fixture must produce a redacted first step"
    );
    store::store(&mut conn, &playbook).expect("store playbook");

    let desktop = Desktop::new_default().expect("accessibility engine");
    let report = replay::replay(&mut conn, &desktop, &playbook.id)
        .await
        .expect("replay ran");

    // -- the outcome itself --
    assert_eq!(report.outcomes.len(), 1, "replay must stop at the halt");
    assert_eq!(report.outcomes[0].result, StepResult::HaltedRedacted);
    assert_eq!(report.status, "aborted", "halt is a decline, not a failure");

    // -- as recorded in the database --
    let run = journal::load_run(&conn, &report.run_id).expect("read runs row");
    assert_eq!(run.status, "aborted");
    assert!(run.completed_at.is_some(), "completed_at must be set");
    assert!(!run.billable, "local replay is never billable");

    let logs = journal::load_step_logs(&conn, &report.run_id).expect("read log rows");
    assert_eq!(logs.len(), 1, "only the halted step should be logged");
    assert_eq!(logs[0].event_type, "aborted");
    assert!(logs[0].is_sensitive);
    assert!(
        logs[0].data_payload.is_none(),
        "a sensitive row must carry no payload"
    );
    assert_eq!(logs[0].cost, 0.0);

    // -- the secret must not be anywhere in the journal --
    let dumped = format!("{:?}", logs);
    assert!(
        !dumped.contains("hunter2-not-a-real-secret"),
        "secret leaked into run_steps_log: {dumped}"
    );
}

#[tokio::test]
async fn a_genuine_failure_is_still_recorded_as_failed() {
    // The counterpart: `aborted` must not swallow real failures. A step whose
    // selector cannot match anything is a failure, not a decline.
    let dir = TempDir::new().expect("temp dir");
    let (db_path, key_path) = db::paths_in(dir.path());
    let mut conn = db::open(&db_path, &key_path).expect("open encrypted db");

    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
    stream.admit(ActionCandidate {
        kind: ActionKind::Click,
        identifiers: vec!["nosuchapp.exe".into()],
        process_name: Some("nosuchapp.exe".into()),
        element_role: Some("Button".into()),
        element_name: Some("PARADIGM-NONEXISTENT-CONTROL-9f3a2b".into()),
        payload: None,
        detail: None,
        timestamp_ms: 1,
    });

    let playbook = compile(
        &stream.actions().to_vec(),
        "Missing Target",
        &ReversibilityPolicy::placeholder(),
        &RedactionPolicy::placeholder(),
    );
    store::store(&mut conn, &playbook).expect("store playbook");

    let desktop = Desktop::new_default().expect("accessibility engine");
    let report = replay::replay(&mut conn, &desktop, &playbook.id)
        .await
        .expect("replay ran");

    assert_eq!(report.outcomes[0].result, StepResult::FailedNotFound);
    assert_eq!(report.status, "failed", "a missing element is a real failure");

    let run = journal::load_run(&conn, &report.run_id).expect("read runs row");
    assert_eq!(run.status, "failed");

    let logs = journal::load_step_logs(&conn, &report.run_id).expect("read log rows");
    assert_eq!(logs[0].event_type, "failure");
}
