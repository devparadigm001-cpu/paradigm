pub mod capture;
pub mod commands;
pub mod compile;
pub mod db;
pub mod detect;
pub mod labeling;
pub mod replay;
pub mod run;
pub mod source;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use capture::{CaptureSession, CapturedAction};

/// Emitted (to every window) when the Record Mode global shortcut fires.
/// The frontend's `useRecordMode` hook listens for this and runs the exact
/// same start/stop flow the main window's button does -- there is no
/// separate "shortcut path" through the backend.
pub const RECORD_MODE_TOGGLE_SHORTCUT_EVENT: &str = "record-mode:toggle-shortcut";

/// The one global shortcut Phase 1 wires up. A reasonable default, not a
/// locked binding -- customization is an explicit later-pass item.
fn record_mode_toggle_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyR)
}

/// Everything the commands need, managed once at startup.
///
/// rusqlite connections are not `Sync`, so the connection is serialised behind
/// a mutex. One is enough for Phase 1; if contention shows up this becomes a
/// small pool, which is a change local to this struct.
///
/// The labeling model is deliberately NOT stored here. `LlamaBackend::init()`
/// can only succeed once per process, so the engine is a process-wide singleton
/// reached through `labeling::shared()`; putting a copy in per-app state would
/// break the moment a second `App` was constructed, which the IPC tests do.
/// `model_path` is kept so state still owns the location.
pub struct AppState {
    /// A `tokio` mutex, not a `std` one: `replay_playbook` holds this across an
    /// `.await`, and a `std::sync::MutexGuard` is not `Send`, which makes the
    /// whole command future non-`Send` and Tauri refuses to register it.
    pub db: tokio::sync::Mutex<rusqlite::Connection>,
    pub db_path: PathBuf,
    pub model_path: PathBuf,
    /// Phase 1 has at most one recording at a time.
    pub session: Mutex<Option<CaptureSession>>,
    /// The last stopped session, awaiting a compile decision. Kept here rather
    /// than round-tripped through the frontend -- see `commands.rs`.
    pub pending_actions: Mutex<Option<Vec<CapturedAction>>>,
    /// The source->destination links from that same session, kept beside it.
    ///
    /// Separate from `pending_actions` rather than folded into it because they
    /// are different kinds of fact: an action is a step to replay, a link is
    /// evidence about where a value came from. Only detection reads these, and
    /// only until the compile decision is made -- §3's transient rule, which is
    /// why they live in memory and never in the database.
    pub pending_links: Mutex<Option<Vec<capture::grid::SourceLink>>>,
}

pub const MODEL_FILE: &str = "models/qwen2.5-0.5b-instruct-q4_k_m.gguf";

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Verify the local database is present, encrypted, migrated, and writable.
#[tauri::command]
async fn db_health_check(
    state: tauri::State<'_, AppState>,
) -> Result<db::health::HealthReport, String> {
    let mut conn = state.db.lock().await;
    db::health::check(&mut conn, &state.db_path).map_err(|e| e.to_string())
}

/// Where the encrypted store lives.
///
/// `PARADIGM_DATA_DIR` overrides the OS app-data location. It exists so the IPC
/// tests can point a mock app at a scratch directory instead of the real user
/// profile; a portable install would use the same hook.
fn data_dir<R: tauri::Runtime>(app: &tauri::App<R>) -> tauri::Result<PathBuf> {
    match std::env::var_os("PARADIGM_DATA_DIR") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => app.path().app_data_dir(),
    }
}

/// Where the local model lives. `PARADIGM_MODEL_PATH` overrides it for tests
/// and for installs that place models elsewhere.
fn model_path() -> PathBuf {
    match std::env::var_os("PARADIGM_MODEL_PATH") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL_FILE),
    }
}

/// The app's entire builder configuration: plugins, setup, and the one and only
/// command registration.
///
/// This is generic over the runtime so `run()` and the IPC tests drive the
/// *same* code rather than two copies that can drift apart.
///
/// EVERY command must be registered in the single handler below. A second
/// registration call anywhere in this chain compiles fine and silently discards
/// the first -- that is the bug Step 1 found by hand, and
/// `tests/ipc_commands.rs` now exists to catch it. Extend the list; never add
/// another call.
pub fn configure<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed
                        && shortcut == &record_mode_toggle_shortcut()
                    {
                        if let Err(e) = app.emit(RECORD_MODE_TOGGLE_SHORTCUT_EVENT, ()) {
                            eprintln!(
                                "[paradigm] failed to emit {RECORD_MODE_TOGGLE_SHORTCUT_EVENT}: {e}"
                            );
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            // Best-effort: a failure here (e.g. another app already holds this
            // combination) should not stop Paradigm from starting -- the main
            // window's Start/Stop button still works either way.
            if let Err(e) = app.global_shortcut().register(record_mode_toggle_shortcut()) {
                eprintln!(
                    "[paradigm] could not register the Record Mode global shortcut \
                     (Ctrl+Shift+R): {e}"
                );
            }

            let app_data_dir = data_dir(app)?;
            let (db_path, key_path) = db::paths_in(&app_data_dir);
            let conn = db::open(&db_path, &key_path)?;
            let model_path = model_path();

            // Load the model once, at startup, so the first labeling call does
            // not pay for it. A failure is reported rather than fatal:
            // recording and replay still work without labeling, and the command
            // that needs it returns the reason.
            match labeling::shared(&model_path) {
                Ok(engine) => eprintln!(
                    "[paradigm] labeling model resident: {} ({:.3}s)",
                    engine.model_source(),
                    engine.load_time().as_secs_f64()
                ),
                Err(e) => eprintln!("[paradigm] labeling model unavailable: {e}"),
            }

            // Replay's coordinate-click fallback -- the workaround for the
            // secondary-monitor click refusal -- silently clicks the wrong
            // place unless this process is per-monitor DPI aware. That coupling
            // was flagged as invisible to callers and easy to break, so the app
            // states it at startup instead of leaving it to be discovered by a
            // misplaced click on someone's second display.
            //
            // Reported, not enforced: forcing an awareness here would change how
            // the app's own window scales, which is a rendering decision rather
            // than an automation one.
            // See docs/known-issues/terminator-multi-monitor-visibility.md.
            eprintln!(
                "[paradigm] per-monitor DPI aware: {}",
                replay::is_per_monitor_dpi_aware()
            );

            app.manage(AppState {
                db: tokio::sync::Mutex::new(conn),
                db_path,
                model_path,
                session: Mutex::new(None),
                pending_actions: Mutex::new(None),
                pending_links: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            db_health_check,
            commands::start_record_session,
            commands::stop_record_session,
            commands::compile_and_store_playbook,
            commands::list_playbooks,
            commands::delete_playbook,
            commands::replay_playbook,
            commands::get_run_history,
            commands::get_orphaned_run_history,
        ])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    configure(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
