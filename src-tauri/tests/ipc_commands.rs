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
];

/// The Step 1 regression test, extended to all eight commands.
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
