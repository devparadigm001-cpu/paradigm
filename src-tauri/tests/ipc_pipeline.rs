//! End-to-end Phase 1 pipeline over the real IPC boundary.
//!
//! Every stage is driven by dispatching an actual command through Tauri's mock
//! runtime -- `get_ipc_response`, not a direct library call -- so this proves
//! the wiring, not just the logic underneath it.
//!
//! Lives in its own test binary because it drives the real desktop and starts a
//! real input recorder. Cargo runs test binaries sequentially, so this cannot
//! race the fast tests in `ipc_commands.rs`.
//!
//! WARNING: this test performs real clicks and typing.

use std::sync::Mutex;
use std::time::Duration;

use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{App, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tempfile::TempDir;
use terminator::Desktop;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The string this test types. Shared by the driver and the assertion so the
/// two cannot drift -- the whole point of the check is that what came out of
/// capture equals what went in.
const TYPED_TEXT: &str = "ipc-pipeline-test";

const LOGIN_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Paradigm IPC Pipeline Test</title></head>
<body style="font-family:sans-serif;padding:2rem">
<h2>Paradigm IPC pipeline test (not a real service)</h2>
<label for="u">Username</label><br>
<input id="u" name="Username" aria-label="Username" placeholder="Username"
       style="font-size:1.2rem;padding:.4rem"><br><br>
<button aria-label="Cancel" style="font-size:1.1rem;padding:.4rem 1rem">Cancel</button>
</body></html>
"#;

fn mock_app() -> (App<MockRuntime>, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let app = {
        // Must span set_var through run_iteration: setup reads the env var
        // inside run_iteration, not inside build().
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PARADIGM_DATA_DIR", dir.path());
        let mut app = paradigm_lib::configure(mock_builder())
            .build(mock_context(noop_assets()))
            .expect("build mock app");
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

fn invoke(
    webview: &WebviewWindow<MockRuntime>,
    cmd: &str,
    body: InvokeBody,
) -> Result<serde_json::Value, String> {
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
    .map(|b| b.deserialize::<serde_json::Value>().unwrap_or(serde_json::Value::Null))
    .map_err(|e| e.to_string())
}

fn json(v: serde_json::Value) -> InvokeBody {
    InvokeBody::Json(v)
}

/// Drive a couple of real actions so the capture has something in it.
async fn drive(desktop: &Desktop) {
    let field = desktop.locator("role:Edit|name:Username");
    if let Ok(f) = field.first(Some(Duration::from_secs(15))).await {
        // click() is refused for elements on a secondary monitor; fall back to
        // a real coordinate click, as replay itself does.
        if f.click().is_err() {
            if let Ok((x, y, w, h)) = f.bounds() {
                let _ = desktop.click_at_coordinates(x + w / 2.0, y + h / 2.0);
            }
        }
        let _ = f.type_text(TYPED_TEXT, false);
        tokio::time::sleep(Duration::from_millis(700)).await;
    }

    let cancel = desktop.locator("role:Button|name:Cancel");
    if let Ok(b) = cancel.first(Some(Duration::from_secs(8))).await {
        if b.click().is_err() {
            if let Ok((x, y, w, h)) = b.bounds() {
                let _ = desktop.click_at_coordinates(x + w / 2.0, y + h / 2.0);
            }
        }
    }
}

#[tokio::test]
async fn full_pipeline_over_ipc() {
    paradigm_lib::replay::ensure_dpi_aware();

    let (app, _dir) = mock_app();
    let webview = webview(&app);

    // ---- lifecycle guards, before any real recording -----------------------
    let err = invoke(&webview, "stop_record_session", InvokeBody::default())
        .expect_err("stopping with no session must fail");
    assert!(
        err.contains("no recording session is active"),
        "unhelpful error: {err}"
    );

    let err = invoke(&webview, "compile_and_store_playbook", json(serde_json::json!({})))
        .expect_err("compiling with nothing captured must fail");
    assert!(err.contains("no captured session"), "unhelpful error: {err}");

    // ---- put a target on screen -------------------------------------------
    let page = std::env::temp_dir().join("paradigm-ipc-pipeline.html");
    std::fs::write(&page, LOGIN_HTML).expect("write test page");
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    for browser in ["msedge", "chrome", "firefox"] {
        if let Ok(mut c) = std::process::Command::new("cmd")
            .args(["/C", "start", "", browser, &url])
            .spawn()
        {
            let _ = c.wait();
            break;
        }
    }
    tokio::time::sleep(Duration::from_secs(10)).await;

    // ---- 1. start ----------------------------------------------------------
    let started = invoke(&webview, "start_record_session", InvokeBody::default())
        .expect("start_record_session");
    let session_name = started.as_str().expect("session name").to_string();
    assert!(session_name.starts_with("record-"), "got {session_name:?}");

    // Starting twice must be rejected, not silently ignored.
    let err = invoke(&webview, "start_record_session", InvokeBody::default())
        .expect_err("second start must fail");
    assert!(err.contains("already active"), "unhelpful error: {err}");

    // ---- drive real actions -----------------------------------------------
    let desktop = Desktop::new_default().expect("accessibility engine");
    tokio::time::sleep(Duration::from_secs(2)).await;
    drive(&desktop).await;
    tokio::time::sleep(Duration::from_secs(6)).await;

    // ---- 2. stop -----------------------------------------------------------
    let summary = invoke(&webview, "stop_record_session", InvokeBody::default())
        .expect("stop_record_session");
    let action_count = summary["action_count"].as_u64().expect("action_count");
    println!("captured {action_count} action(s): {summary}");
    assert!(
        action_count > 0,
        "capture produced nothing, so the rest of the pipeline cannot be proven"
    );

    // Secrets must not cross the IPC boundary even for display.
    for a in summary["actions"].as_array().expect("actions array") {
        if a["would_redact"].as_bool() == Some(true) {
            assert!(
                a["payload_preview"].is_null(),
                "a redactable payload was sent to the frontend: {a}"
            );
        }
    }

    // The typed text must survive capture intact.
    //
    // This assertion exists because its absence hid a real defect: the test
    // previously checked only that SOME actions were captured, so it passed
    // green while typing was recorded as "ip", and again later while typing was
    // not recorded at all. A test that cannot fail on the bug it covers is
    // worse than no test. See docs/known-issues/text-input-capture-truncation.md.
    let actions = summary["actions"].as_array().expect("actions array");
    let typed: Vec<&serde_json::Value> = actions
        .iter()
        .filter(|a| a["action_type"] == "type")
        .collect();

    assert!(
        !typed.is_empty(),
        "no `type` action was captured, so the typed text was lost entirely. \
         Captured actions were: {actions:#?}"
    );

    let payloads: Vec<&str> = typed
        .iter()
        .filter_map(|a| a["payload_preview"].as_str())
        .collect();
    assert!(
        payloads.iter().any(|p| *p == TYPED_TEXT),
        "captured typed text does not match what was typed.\n  \
         expected : {TYPED_TEXT:?}\n  \
         captured : {payloads:?}\n\
         A truncated prefix here is the capture defect, not a replay problem."
    );

    // ---- 3. compile + store ------------------------------------------------
    let stored = invoke(
        &webview,
        "compile_and_store_playbook",
        json(serde_json::json!({ "nameHint": "IPC Pipeline Test" })),
    )
    .expect("compile_and_store_playbook");
    let playbook_id = stored["playbook_id"].as_str().expect("playbook_id").to_string();
    assert_eq!(stored["label"], "IPC Pipeline Test");
    assert_eq!(stored["label_generated"], false);
    assert_eq!(
        stored["step_count"].as_u64(),
        Some(action_count),
        "every captured action should compile to a step"
    );

    // The pending capture is consumed, so a second compile must fail.
    let err = invoke(&webview, "compile_and_store_playbook", json(serde_json::json!({})))
        .expect_err("second compile must fail");
    assert!(err.contains("no captured session"), "unhelpful error: {err}");

    // ---- 4. list -----------------------------------------------------------
    let list = invoke(&webview, "list_playbooks", InvokeBody::default()).expect("list_playbooks");
    let rows = list.as_array().expect("array");
    let found = rows
        .iter()
        .find(|p| p["id"] == playbook_id.as_str())
        .unwrap_or_else(|| panic!("stored playbook missing from list: {list}"));
    assert_eq!(found["source"], "record_mode");
    assert_eq!(found["step_count"].as_u64(), Some(action_count));

    // ---- 5. replay ---------------------------------------------------------
    let replayed = invoke(
        &webview,
        "replay_playbook",
        json(serde_json::json!({ "playbookId": playbook_id })),
    )
    .expect("replay_playbook");
    let run_id = replayed["run_id"].as_str().expect("run_id").to_string();
    let status = replayed["status"].as_str().expect("status").to_string();
    println!("replay status={status}: {replayed}");
    assert!(
        ["completed", "failed", "aborted"].contains(&status.as_str()),
        "unexpected run status {status:?}"
    );
    assert!(
        !replayed["outcomes"].as_array().expect("outcomes").is_empty(),
        "replay reported no step outcomes"
    );

    // ---- 6. run history ----------------------------------------------------
    let history = invoke(
        &webview,
        "get_run_history",
        json(serde_json::json!({ "playbookId": playbook_id })),
    )
    .expect("get_run_history");
    let runs = history.as_array().expect("array");
    let run = runs
        .iter()
        .find(|r| r["run_id"] == run_id.as_str())
        .unwrap_or_else(|| panic!("replay run missing from history: {history}"));

    assert_eq!(run["feature"], "record_mode");
    assert_eq!(run["status"], status.as_str(), "history must agree with replay");
    assert_eq!(run["billable"], false, "local replay is never billable");
    assert!(
        !run["steps"].as_array().expect("steps").is_empty(),
        "run history has no step logs"
    );

    // Sensitive log rows must carry no payload, all the way out to the frontend.
    for s in run["steps"].as_array().unwrap() {
        if s["is_sensitive"].as_bool() == Some(true) {
            assert!(
                s["data_payload"].is_null(),
                "sensitive log row exposed a payload: {s}"
            );
        }
    }
}
