//! IPC-level tests: dispatch real command invocations through Tauri's mock
//! runtime rather than calling the command functions directly.
//!
//! Calling `db::health::check()` from a test proves the *function* works. It
//! says nothing about whether the command is actually reachable from the
//! frontend, because it bypasses `invoke_handler` entirely.
//!
//! The specific bug this guards against: a second `.invoke_handler()` call in
//! the builder chain typechecks, passes `cargo check`, and passes every other
//! test in this crate -- while silently discarding the first registration and
//! leaving those commands unreachable at runtime. These tests go through
//! `paradigm_lib::configure()`, the same function `run()` uses, so a dropped
//! registration fails here instead of in a shipped build.

use std::sync::Mutex;

use paradigm_lib::capture::{
    ActionCandidate, ActionKind, CapturedAction, CapturedStream, ExclusionList,
};
use paradigm_lib::compile::store;
use paradigm_lib::AppState;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime,
                  INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{App, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tempfile::TempDir;

/// `PARADIGM_DATA_DIR` is process-global, so setting it and building the app
/// must not interleave across the tests in this binary.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Build the real app configuration on the mock runtime, pointed at a scratch
/// data directory. The `TempDir` is returned so the caller keeps it alive --
/// and so it is dropped *after* the app closes the database.
fn mock_app() -> (App<MockRuntime>, TempDir) {
    let dir = TempDir::new().expect("temp dir");

    let app = {
        // The lock MUST span set_var through run_iteration, not just through
        // build(). `build()` does not run the setup hook -- Tauri runs it from
        // the event loop -- so PARADIGM_DATA_DIR is read inside run_iteration,
        // not inside build. Releasing the lock in between let a second test
        // overwrite the variable before this test's setup consumed it, pointing
        // both apps at one database. That raced intermittently.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PARADIGM_DATA_DIR", dir.path());

        let mut app = paradigm_lib::configure(mock_builder())
            .build(mock_context(noop_assets()))
            .expect("build mock app");

        // Runs setup (which opens the database and calls `.manage()`).
        // `MockRuntime::run_iteration` is a no-op, so this returns immediately.
        #[allow(deprecated)]
        app.run_iteration(|_, _| {});

        app
    };

    (app, dir)
}

fn webview(app: &App<MockRuntime>) -> WebviewWindow<MockRuntime> {
    WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
        .build()
        .expect("build webview")
}

/// Dispatch a command exactly as the frontend's `invoke()` would.
fn invoke(
    webview: &WebviewWindow<MockRuntime>,
    cmd: &str,
    body: InvokeBody,
) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.to_string(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "http://tauri.localhost".parse().unwrap(),
            body,
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
}

#[test]
fn db_health_check_is_reachable_over_ipc() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);

    // Do not claim "not registered" here: an unregistered command and a
    // registered one whose arguments fail to inject both surface as Err, with
    // different messages. Print the message and let it say which it was.
    let response = invoke(&webview, "db_health_check", InvokeBody::default())
        .unwrap_or_else(|e| panic!("db_health_check failed over IPC: {e}"));

    let report: serde_json::Value = response.deserialize().expect("deserialize report");

    assert_eq!(
        report["healthy"], true,
        "command was reachable but reported unhealthy: {report}"
    );
    assert_eq!(report["round_trip_ok"], true);
    assert_eq!(report["header_is_plaintext_sqlite"], false);
    assert_eq!(
        report["applied_migrations"].as_array().unwrap().len(),
        paradigm_lib::db::migrations::MIGRATIONS.len()
    );
}

#[test]
fn greet_is_still_reachable_over_ipc() {
    // A second `.invoke_handler()` registering only db_health_check would drop
    // greet while leaving the test above passing. Assert both survive.
    //
    // greet takes no `State`, so this test alone cannot distinguish a properly
    // initialised app from one whose setup hook never ran. It goes through the
    // same `mock_app()` helper as the test above so both run against a fully
    // set-up app; what this test adds is coverage of the *second* registration.
    let (app, _dir) = mock_app();
    let webview = webview(&app);

    let response = invoke(
        &webview,
        "greet",
        InvokeBody::Json(serde_json::json!({ "name": "paradigm" })),
    )
    .unwrap_or_else(|e| panic!("greet failed over IPC: {e}"));

    let greeting: String = response.deserialize().expect("deserialize greeting");
    assert!(
        greeting.contains("paradigm"),
        "unexpected greeting: {greeting}"
    );
}

/// Every command the app registers. If a command is added to
/// `generate_handler!` it must be added here too -- that is the point.
const REGISTERED_COMMANDS: &[(&str, &str)] = &[
    ("greet", r#"{"name":"x"}"#),
    ("db_health_check", "{}"),
    ("start_record_session", "{}"),
    ("stop_record_session", "{}"),
    ("compile_and_store_playbook", r#"{"nameHint":"x"}"#),
    ("list_playbooks", "{}"),
    ("delete_playbook", r#"{"playbookId":"does-not-exist"}"#),
    ("replay_playbook", r#"{"playbookId":"does-not-exist"}"#),
    ("get_run_history", r#"{"playbookId":"does-not-exist"}"#),
    ("get_orphaned_run_history", "{}"),
    // §4.5's two correction scopes. The permanent one reaches a real UPDATE
    // against a playbook that does not exist; the one-off errors with "no
    // workflow run is in progress", which is the command working.
    (
        "read_selected_column",
        r#"{"playbookId":"x","side":"source"}"#,
    ),
    (
        "apply_permanent_correction",
        r#"{"playbookId":"x","side":"source","oldLocator":"C","newLocator":"D","newLabel":"L"}"#,
    ),
    (
        "apply_one_off_correction",
        r#"{"sourceRow":"2","side":"source","oldLocator":"C","newLocator":"D"}"#,
    ),
    // §4.8's batch scan. Reaches the "not a templated workflow" error, which
    // is the command working -- reachability is what this list checks.
    (
        "check_for_new_records",
        r#"{"playbookId":"does-not-exist"}"#,
    ),
    // §4.3's preview and the run start it gates. `start_workflow_run` errors
    // here with "no confirmed first-record preview", which is the gate working
    // rather than a fault -- reachability is what this test checks.
    (
        "preview_workflow_run",
        r#"{"playbookId":"does-not-exist"}"#,
    ),
    ("cancel_workflow_preview", "{}"),
    ("start_workflow_run", "{}"),
    // §4.6's run controls. Each legitimately errors here with "no workflow run
    // is in progress", which is exactly the kind of error this test ignores --
    // what it checks is reachability, not success.
    ("pause_workflow_run", "{}"),
    ("resume_workflow_run", "{}"),
    ("stop_workflow_run", "{}"),
    ("get_workflow_run_status", "{}"),
    ("get_workflow_run_report", "{}"),
];

/// The Step 1 regression test, extended to every registered command.
///
/// A command can compile, be listed in `generate_handler!`, and still be
/// unreachable if a second `.invoke_handler()` call discards the registration.
/// This asserts only that each command is FOUND -- several legitimately return
/// errors here (no session active, no such playbook), and that is fine. What
/// must never happen is "Command X not found".
#[test]
fn every_registered_command_is_reachable_over_ipc() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);

    let mut unreachable = Vec::new();
    for (cmd, args) in REGISTERED_COMMANDS {
        let body: serde_json::Value = serde_json::from_str(args).expect("valid test args");
        let result = invoke(&webview, cmd, InvokeBody::Json(body));

        if let Err(e) = &result {
            let msg = e.to_string();
            if msg.contains("not found") {
                unreachable.push(format!("{cmd}: {msg}"));
            }
        }
    }

    assert!(
        unreachable.is_empty(),
        "commands registered but unreachable over IPC: {unreachable:#?}"
    );

    // Recording actually starts a real recorder above; make sure it is stopped
    // so it does not outlive the test.
    let _ = invoke(&webview, "stop_record_session", InvokeBody::default());
}

// ------------------------------------------- compile_and_store_playbook ----
//
// These drive the real command over the real IPC boundary, but seed the
// pending capture directly instead of recording one: the reorder/subset
// behaviour is independent of where the actions came from, and `ipc_pipeline`
// already covers the recorded path end to end.

/// Build gated actions the only way they can be built -- by putting candidates
/// through the real exclusion gate.
///
/// A test cannot construct a `CapturedAction` itself, and deliberately so
/// (`capture::stream`). Going through `admit` means these are the same kind of
/// actions a recording produces, so seeding them is not a way to sidestep the
/// gate.
fn gated_actions(names: &[&str]) -> Vec<CapturedAction> {
    let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
    for (i, name) in names.iter().enumerate() {
        stream.admit(ActionCandidate {
            kind: ActionKind::Click,
            identifiers: vec!["paradigm-ipc-test.exe".into()],
            process_name: Some("paradigm-ipc-test.exe".into()),
            element_role: Some("Button".into()),
            element_name: Some((*name).to_string()),
            payload: None,
            detail: None,
            timestamp_ms: i as u64,
        });
    }

    let actions = stream.actions().to_vec();
    assert_eq!(
        actions.len(),
        names.len(),
        "the exclusion gate refused a test action; the fixture is wrong, not the code"
    );
    actions
}

fn seed_pending(app: &App<MockRuntime>, names: &[&str]) {
    let state = app.state::<AppState>();
    let mut pending = state.pending_actions.lock().expect("pending_actions lock");
    *pending = Some(gated_actions(names));
}

/// How many actions are still awaiting a compile decision, if any.
fn pending_count(app: &App<MockRuntime>) -> Option<usize> {
    let state = app.state::<AppState>();
    let pending = state.pending_actions.lock().expect("pending_actions lock");
    pending.as_ref().map(Vec::len)
}

/// Invoke the command the way the frontend would. `indices` of `None` omits the
/// argument entirely rather than sending null, which is what an existing caller
/// that predates `stepIndices` actually does.
fn compile(
    webview: &WebviewWindow<MockRuntime>,
    name_hint: &str,
    indices: Option<&[i64]>,
) -> Result<serde_json::Value, String> {
    let mut body = serde_json::json!({ "nameHint": name_hint });
    if let Some(indices) = indices {
        body["stepIndices"] = serde_json::json!(indices);
    }

    invoke(webview, "compile_and_store_playbook", InvokeBody::Json(body))
        .map(|r| r.deserialize::<serde_json::Value>().expect("deserialize info"))
        .map_err(|e| e.to_string())
}

/// The target names of a stored playbook's steps, in stored order.
///
/// Reads the database back rather than trusting the command's return value:
/// `step_count` alone cannot tell a reorder from a no-op.
fn stored_step_names(app: &App<MockRuntime>, playbook_id: &str) -> Vec<String> {
    let state = app.state::<AppState>();
    let conn = state.db.blocking_lock();
    let playbook = store::load(&conn, playbook_id).expect("load stored playbook");

    // Whatever was selected, the stored order must still be dense and 1-based --
    // a subset that left holes would replay in the wrong shape.
    let orders: Vec<i64> = playbook.steps.iter().map(|s| s.step_order).collect();
    assert_eq!(
        orders,
        (1..=playbook.steps.len() as i64).collect::<Vec<_>>(),
        "stored step_order is not dense and 1-based"
    );

    playbook
        .steps
        .iter()
        .map(|s| {
            let payload: serde_json::Value =
                serde_json::from_str(&s.action_payload_json).expect("payload json");
            payload["target"]["name"]
                .as_str()
                .expect("target name")
                .to_string()
        })
        .collect()
}

#[test]
fn omitting_step_indices_compiles_every_action_in_captured_order() {
    // The pre-existing behaviour. It must not change for a caller that has
    // never heard of stepIndices.
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo", "charlie", "delta"]);

    let info = compile(&webview, "Unchanged", None).expect("compile with no selection");

    assert_eq!(info["step_count"].as_u64(), Some(4));
    assert_eq!(info["label"], "Unchanged");
    assert_eq!(info["label_generated"], false);

    let id = info["playbook_id"].as_str().expect("playbook_id");
    assert_eq!(
        stored_step_names(&app, id),
        vec!["alpha", "bravo", "charlie", "delta"]
    );
    assert_eq!(pending_count(&app), None, "the capture should be consumed");
}

#[test]
fn step_indices_reorder_the_stored_playbook() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo", "charlie", "delta"]);

    let info = compile(&webview, "Reordered", Some(&[3, 1, 0, 2])).expect("compile a reorder");

    assert_eq!(info["step_count"].as_u64(), Some(4), "a reorder drops nothing");

    let id = info["playbook_id"].as_str().expect("playbook_id");
    assert_eq!(
        stored_step_names(&app, id),
        vec!["delta", "bravo", "alpha", "charlie"],
        "steps were not stored in the requested order"
    );
}

#[test]
fn step_indices_can_drop_actions() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo", "charlie", "delta"]);

    // Keep two of the four, and not in captured order either.
    let info = compile(&webview, "Subset", Some(&[2, 0])).expect("compile a subset");

    assert_eq!(info["step_count"].as_u64(), Some(2));

    let id = info["playbook_id"].as_str().expect("playbook_id");
    assert_eq!(stored_step_names(&app, id), vec!["charlie", "alpha"]);

    // The dropped actions are gone, not merely hidden.
    let names = stored_step_names(&app, id).join(",");
    assert!(!names.contains("bravo"), "a deselected action was stored: {names}");
    assert!(!names.contains("delta"), "a deselected action was stored: {names}");
}

#[test]
fn an_out_of_range_step_index_is_rejected_and_the_capture_survives() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo", "charlie"]);

    let err = compile(&webview, "Bad", Some(&[0, 3]))
        .expect_err("an out-of-range index must be rejected");
    assert!(err.contains("out of range"), "unhelpful error: {err}");

    // Rejected before any work, so the user can correct the selection rather
    // than having to record the session again.
    assert_eq!(
        pending_count(&app),
        Some(3),
        "a rejected selection consumed the pending capture"
    );

    let list = invoke(&webview, "list_playbooks", InvokeBody::default())
        .expect("list_playbooks")
        .deserialize::<serde_json::Value>()
        .expect("deserialize list");
    assert!(
        list.as_array().expect("array").is_empty(),
        "a rejected selection stored a playbook anyway: {list}"
    );

    // And the corrected call still works.
    let info = compile(&webview, "Corrected", Some(&[0, 2])).expect("retry with valid indices");
    let id = info["playbook_id"].as_str().expect("playbook_id");
    assert_eq!(stored_step_names(&app, id), vec!["alpha", "charlie"]);
}

#[test]
fn an_empty_step_indices_list_is_rejected() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo"]);

    let err = compile(&webview, "Empty", Some(&[]))
        .expect_err("selecting nothing must not store an empty playbook");
    assert!(
        err.contains("no actions to compile"),
        "should reuse the existing nothing-to-compile error: {err}"
    );
    assert_eq!(pending_count(&app), Some(2));
}

#[test]
fn a_negative_step_index_is_rejected() {
    // usize deserialisation refuses this at the IPC boundary. Asserted so a
    // future change to a signed index type cannot silently start wrapping.
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo"]);

    let err = compile(&webview, "Negative", Some(&[-1]))
        .expect_err("a negative index must be rejected");
    println!("negative index rejected with: {err}");
    assert_eq!(pending_count(&app), Some(2));
}

#[test]
fn delete_playbook_removes_it_from_the_list_over_ipc() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo"]);

    let info = compile(&webview, "Doomed", None).expect("compile");
    let id = info["playbook_id"].as_str().expect("playbook_id").to_string();

    let deleted = invoke(
        &webview,
        "delete_playbook",
        InvokeBody::Json(serde_json::json!({ "playbookId": id })),
    );
    assert!(deleted.is_ok(), "delete_playbook failed: {:?}", deleted.err());

    let list = invoke(&webview, "list_playbooks", InvokeBody::default())
        .expect("list_playbooks")
        .deserialize::<serde_json::Value>()
        .expect("deserialize list");
    assert!(
        list.as_array().expect("array").is_empty(),
        "the deleted playbook is still listed: {list}"
    );
}

#[test]
fn deleting_an_unknown_playbook_reports_an_error_over_ipc() {
    // A silent success here would tell the frontend something was removed when
    // nothing was, which is the specific behaviour store::delete guards against.
    let (app, _dir) = mock_app();
    let webview = webview(&app);

    let result = invoke(
        &webview,
        "delete_playbook",
        InvokeBody::Json(serde_json::json!({ "playbookId": "no-such-playbook" })),
    );

    let err = result.expect_err("deleting a nonexistent playbook must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("no playbook with id"),
        "unhelpful error: {msg}"
    );
}

// ------------------------------------------------- calibration recording ----
//
// `confidence_calibration` accumulated nothing from real use because
// `calibration::record` had one caller, in a probe writing to a temp directory.
// These cover the wiring into the real command path.

fn calibration_rows(app: &App<MockRuntime>) -> i64 {
    let state = app.state::<AppState>();
    let conn = state.db.blocking_lock();
    conn.query_row("SELECT COUNT(*) FROM confidence_calibration", [], |r| {
        r.get(0)
    })
    .expect("count calibration rows")
}

#[test]
fn model_labelling_records_a_calibration_sample() {
    // An empty name hint is what routes through the local model, and the model
    // running is the only thing that produces a confidence score to calibrate.
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo"]);

    assert_eq!(calibration_rows(&app), 0, "should start empty");

    let info = match compile(&webview, "", None) {
        Ok(i) => i,
        Err(e) => {
            // The local model must be present for this to mean anything. Fail
            // loudly rather than passing vacuously.
            panic!("compile with model labelling failed: {e}");
        }
    };

    assert_eq!(
        info["label_generated"], true,
        "the model should have named this playbook"
    );
    assert!(
        calibration_rows(&app) > 0,
        "the model ran but no calibration sample was recorded"
    );

    // A row existing is not enough -- check it carries real values, so a
    // degenerate write would not pass this.
    let state = app.state::<AppState>();
    let conn = state.db.blocking_lock();
    let (model, min, max, samples, success, normalized): (
        String,
        f64,
        f64,
        i64,
        i64,
        Option<f64>,
    ) = conn
        .query_row(
            "SELECT model_source, raw_score_min, raw_score_max, sample_count,
                    success_count, normalized_score
               FROM confidence_calibration",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .expect("read the calibration row back");

    assert!(!model.trim().is_empty(), "model_source is blank");
    assert!(
        (0.0..=1.0).contains(&min) && (0.0..=1.0).contains(&max) && min < max,
        "bin bounds are not a sane half-open tenth: [{min}, {max})"
    );
    assert_eq!(samples, 1, "one save should record exactly one sample");
    assert!(
        success == 0 || success == 1,
        "success_count out of range: {success}"
    );
    assert!(
        normalized.is_none(),
        "normalized_score should stay NULL -- populating it is Phase 2's job"
    );
}

#[test]
fn supplying_a_name_records_no_calibration_sample() {
    // Explicitly asserted rather than left as an absence someone might notice:
    // with a caller-supplied name no model runs, so there is nothing to
    // calibrate. Samples only ever come from unnamed sessions.
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha"]);

    let info = compile(&webview, "Named By Hand", None).expect("compile");

    assert_eq!(info["label_generated"], false, "no model should have run");
    assert_eq!(
        calibration_rows(&app),
        0,
        "a calibration row appeared without the model having run"
    );
}

#[test]
fn a_calibration_failure_does_not_prevent_saving_the_playbook() {
    // Calibration is bookkeeping. Refusing to save a user's recording because a
    // statistics row could not be written would trade something they care about
    // for something they have never heard of.
    let (app, _dir) = mock_app();
    let webview = webview(&app);
    seed_pending(&app, &["alpha", "bravo"]);

    // Remove the table so `calibration::record` genuinely fails.
    {
        let state = app.state::<AppState>();
        let conn = state.db.blocking_lock();
        conn.execute("DROP TABLE confidence_calibration", [])
            .expect("drop the calibration table");
    }

    let info = compile(&webview, "", None)
        .expect("the playbook must still save when calibration recording fails");
    let id = info["playbook_id"].as_str().expect("playbook_id").to_string();

    // And it really is stored, not merely reported.
    let list = invoke(&webview, "list_playbooks", InvokeBody::default())
        .expect("list_playbooks")
        .deserialize::<serde_json::Value>()
        .expect("deserialize list");
    assert!(
        list.as_array()
            .expect("array")
            .iter()
            .any(|p| p["id"] == id.as_str()),
        "the playbook was reported saved but is not in the list: {list}"
    );
}

#[test]
fn unregistered_command_is_rejected() {
    // Without this, the tests above could pass against a harness that never
    // actually consults the command registry.
    let (app, _dir) = mock_app();
    let webview = webview(&app);

    let result = invoke(&webview, "no_such_command", InvokeBody::default());

    assert!(
        result.is_err(),
        "an unregistered command returned success -- these tests cannot detect \
         a missing registration"
    );
}

/// §4.3's gate, at the IPC boundary.
///
/// The type system already makes `run::background::spawn` unreachable without a
/// `RunAuthorization`, and a `RunAuthorization` unobtainable without a preview.
/// This checks the other end of the same rule: the command a frontend can
/// actually call refuses, with an error that says what to do instead.
///
/// Worth testing separately from the compile-time guarantee because the two
/// fail differently. A missing type would not compile; a command that quietly
/// started an unpreviewed run would compile perfectly.
#[test]
fn a_run_cannot_be_started_without_a_confirmed_preview() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);

    let err = invoke(&webview, "start_workflow_run", InvokeBody::default())
        .expect_err("starting a run with no preview must be refused");

    let msg = err.to_string();
    assert!(
        msg.contains("preview"),
        "the refusal should name what is missing, got: {msg}"
    );
    assert!(
        !msg.contains("not found"),
        "the command must exist -- this is about the gate, not registration: {msg}"
    );
}

/// Declining is not an error, and does not need a preview to have existed.
///
/// §4.10: rejecting "cancels cleanly". A cancel that errored when there was
/// nothing to cancel would make the frontend's tidy-up path conditional on
/// state it should not have to track.
#[test]
fn cancelling_a_preview_that_was_never_shown_is_not_an_error() {
    let (app, _dir) = mock_app();
    let webview = webview(&app);

    invoke(&webview, "cancel_workflow_preview", InvokeBody::default())
        .expect("cancelling with nothing pending should succeed quietly");
}
