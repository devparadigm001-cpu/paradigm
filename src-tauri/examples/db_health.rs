//! Open the *real* on-device database -- the same path the Tauri app uses --
//! and print its health report.
//!
//!     cargo run --example db_health
//!
//! On Windows Tauri's `app_data_dir()` is %APPDATA%\<bundle identifier>, so
//! this touches the same files the shipped app would.

use std::path::PathBuf;

use paradigm_lib::db;

/// Must match `identifier` in tauri.conf.json.
const BUNDLE_IDENTIFIER: &str = "com.amitj.paradigm";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app_data_dir = PathBuf::from(std::env::var("APPDATA")?).join(BUNDLE_IDENTIFIER);
    let (db_path, key_path) = db::paths_in(&app_data_dir);

    println!("app data dir : {}", app_data_dir.display());
    println!("database     : {}", db_path.display());
    println!("key blob     : {}\n", key_path.display());

    let mut conn = db::open(&db_path, &key_path)?;
    let report = db::health::check(&mut conn, &db_path)?;

    println!("{}", serde_json::to_string_pretty(&report)?);

    if !report.healthy {
        std::process::exit(1);
    }
    Ok(())
}
