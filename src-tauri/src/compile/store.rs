//! Write a validated playbook to the encrypted local store, and read it back.

use rusqlite::Connection;

use crate::db::DbError;

use super::validate::{validate, ValidationError};
use super::{CompiledPlaybook, ControlRole};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("playbook failed validation and was NOT stored:\n  - {}", .0.iter().map(|e| e.describe()).collect::<Vec<_>>().join("\n  - "))]
    Invalid(Vec<ValidationError>),

    #[error(transparent)]
    Db(#[from] DbError),

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Validate, then store the playbook and all its steps in one transaction.
///
/// Validation runs inside this function rather than being left to the caller:
/// there is no code path that stores an unvalidated playbook.
pub fn store(conn: &mut Connection, playbook: &CompiledPlaybook) -> Result<(), StoreError> {
    let errors = validate(playbook);
    if !errors.is_empty() {
        return Err(StoreError::Invalid(errors));
    }

    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO playbooks (id, name, source) VALUES (?1, ?2, ?3)",
        (&playbook.id, &playbook.name, &playbook.source),
    )?;

    for step in &playbook.steps {
        tx.execute(
            "INSERT INTO playbook_steps
                 (id, playbook_id, step_order, action_type, control_role, reversible,
                  action_payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                &step.id,
                &playbook.id,
                step.step_order,
                &step.action_type,
                step.control_role.as_str(),
                i64::from(step.reversible),
                &step.action_payload_json,
            ),
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// A row as it exists in the database, read back with no compile-time state.
#[derive(Debug, Clone)]
pub struct StoredStep {
    pub id: String,
    pub step_order: i64,
    pub action_type: String,
    pub control_role: String,
    /// Nullable in the schema; Record Mode always assigns it, so a NULL here
    /// would mean something wrote a step by another route.
    pub reversible: Option<bool>,
    pub action_payload_json: String,
}

#[derive(Debug, Clone)]
pub struct StoredPlaybook {
    pub id: String,
    pub name: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub steps: Vec<StoredStep>,
}

/// Read a playbook and its steps back, ordered.
pub fn load(conn: &Connection, playbook_id: &str) -> Result<StoredPlaybook, DbError> {
    let (name, source, created_at, updated_at) = conn.query_row(
        "SELECT name, source, created_at, updated_at FROM playbooks WHERE id = ?1",
        [playbook_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;

    let mut stmt = conn.prepare(
        "SELECT id, step_order, action_type, control_role, reversible, action_payload_json
           FROM playbook_steps
          WHERE playbook_id = ?1
          ORDER BY step_order",
    )?;
    let steps = stmt
        .query_map([playbook_id], |r| {
            Ok(StoredStep {
                id: r.get(0)?,
                step_order: r.get(1)?,
                action_type: r.get(2)?,
                control_role: r.get(3)?,
                reversible: r.get::<_, Option<i64>>(4)?.map(|v| v != 0),
                action_payload_json: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(StoredPlaybook {
        id: playbook_id.to_string(),
        name,
        source,
        created_at,
        updated_at,
        steps,
    })
}

/// Summary row for listing stored playbooks.
#[derive(Debug, Clone)]
pub struct PlaybookSummary {
    pub id: String,
    pub name: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub step_count: i64,
}

/// Every stored playbook, newest first. No pagination in Phase 1.
pub fn list(conn: &Connection) -> Result<Vec<PlaybookSummary>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.source, p.created_at, p.updated_at,
                (SELECT COUNT(*) FROM playbook_steps s WHERE s.playbook_id = p.id)
           FROM playbooks p
          ORDER BY p.created_at DESC, p.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PlaybookSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            source: r.get(2)?,
            created_at: r.get(3)?,
            updated_at: r.get(4)?,
            step_count: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Parse a stored `control_role` back into the enum. Unknown values become
/// `Other`, matching the compile-time mapping's fallback.
pub fn parse_control_role(raw: &str) -> ControlRole {
    match raw {
        "button" => ControlRole::Button,
        "textbox" => ControlRole::Textbox,
        "dropdown" => ControlRole::Dropdown,
        "checkbox" => ControlRole::Checkbox,
        "radio" => ControlRole::Radio,
        "link" => ControlRole::Link,
        _ => ControlRole::Other,
    }
}
