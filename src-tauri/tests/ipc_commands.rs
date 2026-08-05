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

use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime,
                  INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{App, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
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
