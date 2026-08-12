//! Write and read the `runs` / `run_steps_log` rows for a replay.
//!
//! Every constraint here is enforced by migration 20260803000002, so this
//! module's job is to never construct a row that violates one:
//!
//!   * `system_state_json` must be EXACTLY `{"foreground_app": "<string>"}`.
//!     A trigger rejects any extra key, so the shape is built in one place
//!     rather than by callers.
//!   * `data_payload` must be NULL whenever `is_sensitive = 1`. Expressed here
//!     by taking the payload and the flag together and dropping the payload.
//!   * `cost >= 0.0`. Local replay is free, so it is always 0.0.

use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

use crate::db::DbError;

pub const FEATURE_RECORD_MODE: &str = "record_mode";

pub const STATUS_RUNNING: &str = "running";
pub const STATUS_COMPLETED: &str = "completed";
/// Something went wrong: an element was missing, or an action errored.
pub const STATUS_FAILED: &str = "failed";
/// The run correctly declined to continue. A redaction halt is the system
/// working as designed, not an error, so it is distinguished from `failed` --
/// Phase 2 retry logic keying off `failed` must not treat it as retryable.
pub const STATUS_ABORTED: &str = "aborted";

pub const EVENT_EXECUTE: &str = "execute";
pub const EVENT_FAILURE: &str = "failure";
/// Matches the run status: a deliberate stop, not a fault.
pub const EVENT_ABORTED: &str = "aborted";

/// One row to append to `run_steps_log`.
pub struct StepLogEntry {
    pub playbook_step_id: Option<String>,
    pub step_order: i64,
    pub action_type: String,
    pub target_ui_context_json: String,
    /// Dropped if `is_sensitive` is true -- the schema forbids storing both.
    pub data_payload: Option<String>,
    pub is_sensitive: bool,
    pub event_type: String,
    pub model_source: Option<String>,
    pub cost: f64,
    pub foreground_app: String,
}

/// Create the `runs` row and mark it running.
pub fn start_run(conn: &Connection, playbook_id: &str) -> Result<String, DbError> {
    let run_id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO runs (id, playbook_id, feature, status, billable, started_at)
         VALUES (?1, ?2, ?3, ?4, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        (
            &run_id,
            playbook_id,
            FEATURE_RECORD_MODE,
            STATUS_RUNNING,
        ),
    )?;
    Ok(run_id)
}

/// Append one event to `run_steps_log`.
pub fn log_step(conn: &Connection, run_id: &str, entry: &StepLogEntry) -> Result<(), DbError> {
    // Fixed shape. Building it here is what keeps the trigger satisfied.
    let system_state = json!({ "foreground_app": entry.foreground_app }).to_string();

    // The schema forbids a payload on a sensitive row; enforce it before the
    // CHECK has to.
    let payload: Option<&str> = if entry.is_sensitive {
        None
    } else {
        entry.data_payload.as_deref()
    };

    conn.execute(
        "INSERT INTO run_steps_log
             (id, run_id, playbook_step_id, step_order, action_type,
              target_ui_context_json, data_payload, is_sensitive, event_type,
              model_source, cost, system_state_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            Uuid::new_v4().to_string(),
            run_id,
            entry.playbook_step_id,
            entry.step_order,
            entry.action_type,
            entry.target_ui_context_json,
            payload,
            i64::from(entry.is_sensitive),
            entry.event_type,
            entry.model_source,
            entry.cost,
            system_state,
        ],
    )?;
    Ok(())
}

/// Close out the run.
pub fn finish_run(conn: &Connection, run_id: &str, status: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE runs
            SET status = ?2,
                completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
          WHERE id = ?1",
        (run_id, status),
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct StoredRun {
    pub id: String,
    pub playbook_id: Option<String>,
    pub feature: String,
    pub status: String,
    pub billable: bool,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredStepLog {
    pub step_order: i64,
    pub action_type: String,
    pub event_type: String,
    pub data_payload: Option<String>,
    pub is_sensitive: bool,
    pub model_source: Option<String>,
    pub cost: f64,
    pub target_ui_context_json: String,
    pub system_state_json: String,
    pub timestamp: String,
}

/// The column list every `runs` query selects, in the order `row_to_run` reads.
/// Kept in one place so the three readers cannot drift apart.
const RUN_COLUMNS: &str = "id, playbook_id, feature, status, billable, started_at, completed_at";

fn row_to_run(r: &rusqlite::Row) -> rusqlite::Result<StoredRun> {
    Ok(StoredRun {
        id: r.get(0)?,
        playbook_id: r.get(1)?,
        feature: r.get(2)?,
        status: r.get(3)?,
        billable: r.get::<_, i64>(4)? != 0,
        started_at: r.get(5)?,
        completed_at: r.get(6)?,
    })
}

pub fn load_run(conn: &Connection, run_id: &str) -> Result<StoredRun, DbError> {
    Ok(conn.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1"),
        [run_id],
        row_to_run,
    )?)
}

/// Every run recorded for a playbook, most recent first.
pub fn load_runs_for_playbook(
    conn: &Connection,
    playbook_id: &str,
) -> Result<Vec<StoredRun>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM runs
          WHERE playbook_id = ?1
          ORDER BY started_at DESC, id"
    ))?;
    let rows = stmt.query_map([playbook_id], row_to_run)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Runs that no longer belong to a playbook, most recent first.
///
/// These are unreachable through `load_runs_for_playbook`, which needs an id to
/// ask for: once `playbook_id` is NULL there is no id to pass. That is the whole
/// reason this exists -- the schema deliberately keeps the history and, until
/// this, nothing could read it back. See
/// `docs/known-issues/no-way-to-delete-playbooks.md`.
///
/// **What NULL means here.** `migrations/20260803000002_init_runs.sql` gives
/// `playbook_id` `ON DELETE SET NULL` with the comment "run history must outlive
/// its playbook", and documents NULL as *also* marking an ad-hoc run that was
/// never recorded as a playbook. Those two causes are indistinguishable in the
/// schema. Today only one of them can occur: `start_run` takes `&str`, not
/// `Option<&str>`, so every run is created attached and a NULL can only have
/// come from a deletion. If an ad-hoc run path is ever added, this query starts
/// returning both and telling them apart needs a column that does not exist yet.
pub fn load_orphaned_runs(conn: &Connection) -> Result<Vec<StoredRun>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM runs
          WHERE playbook_id IS NULL
          ORDER BY started_at DESC, id"
    ))?;
    let rows = stmt.query_map([], row_to_run)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn load_step_logs(conn: &Connection, run_id: &str) -> Result<Vec<StoredStepLog>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT step_order, action_type, event_type, data_payload, is_sensitive,
                model_source, cost, target_ui_context_json, system_state_json, timestamp
           FROM run_steps_log
          WHERE run_id = ?1
          ORDER BY timestamp, step_order",
    )?;
    let rows = stmt.query_map([run_id], |r| {
        Ok(StoredStepLog {
            step_order: r.get(0)?,
            action_type: r.get(1)?,
            event_type: r.get(2)?,
            data_payload: r.get(3)?,
            is_sensitive: r.get::<_, i64>(4)? != 0,
            model_source: r.get(5)?,
            cost: r.get(6)?,
            target_ui_context_json: r.get(7)?,
            system_state_json: r.get(8)?,
            timestamp: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
