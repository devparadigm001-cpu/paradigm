pub mod capture;
pub mod db;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::Manager;

/// The single app-wide connection to the encrypted local store.
///
/// rusqlite connections are not `Sync`, so access is serialised behind a mutex.
/// One connection is enough for Phase 1; if contention shows up later this
/// becomes a small pool, which is a change local to this struct.
pub struct Db {
    pub conn: Mutex<rusqlite::Connection>,
    pub path: PathBuf,
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Verify the local database is present, encrypted, migrated, and writable.
#[tauri::command]
fn db_health_check(state: tauri::State<'_, Db>) -> Result<db::health::HealthReport, String> {
    let mut conn = state.conn.lock().map_err(|e| e.to_string())?;
    db::health::check(&mut conn, &state.path).map_err(|e| e.to_string())
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

/// The app's entire builder configuration: plugins, setup, and the one and only
/// command registration.
///
/// This is generic over the runtime so `run()` and the IPC tests drive the
/// *same* code rather than two copies that can drift apart. Every command must
/// be registered in the single `invoke_handler` below -- a second
/// `.invoke_handler()` call anywhere in this chain compiles fine and silently
/// discards the first, which is exactly what `tests/ipc_commands.rs` exists to
/// catch.
pub fn configure<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data_dir = data_dir(app)?;
            let (db_path, key_path) = db::paths_in(&app_data_dir);
            let conn = db::open(&db_path, &key_path)?;
            app.manage(Db {
                conn: Mutex::new(conn),
                path: db_path,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet, db_health_check])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    configure(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
