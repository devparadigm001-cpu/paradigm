-- Phase 1: Shared Engine execution history.
--
-- `runs` is one execution attempt; `run_steps_log` is the append-only event
-- stream for that attempt. A run may have more log rows than the playbook has
-- steps (retries and drift repairs each append their own row), and a run with
-- no playbook_id is an ad-hoc execution that was never recorded as a playbook.

CREATE TABLE IF NOT EXISTS runs (
    id           TEXT PRIMARY KEY,
    -- NULL for ad-hoc runs, and set to NULL rather than cascading if the
    -- playbook is later deleted -- run history must outlive its playbook.
    playbook_id  TEXT REFERENCES playbooks (id) ON DELETE SET NULL,
    feature      TEXT NOT NULL
                     CHECK (feature IN ('ghost_mode', 'record_mode',
                                        'chat_mode', 'form_memory')),
    status       TEXT NOT NULL
                     CHECK (status IN ('queued', 'running', 'completed',
                                       'failed', 'aborted')),
    billable     INTEGER NOT NULL DEFAULT 0 CHECK (billable IN (0, 1)),
    -- NULL while queued; completed_at stays NULL until the run leaves 'running'.
    started_at   TEXT,
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_runs_playbook ON runs (playbook_id)
    WHERE playbook_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_runs_status ON runs (status);
-- Drives the cost rollup that gets shipped to the cloud ledger.
CREATE INDEX IF NOT EXISTS idx_runs_feature_started ON runs (feature, started_at);

CREATE TABLE IF NOT EXISTS run_steps_log (
    id                     TEXT PRIMARY KEY,
    run_id                 TEXT NOT NULL
                               REFERENCES runs (id) ON DELETE CASCADE,
    -- NULL when the event has no originating playbook step (ad-hoc run, or a
    -- drift repair that synthesised a step that was never compiled).
    playbook_step_id       TEXT REFERENCES playbook_steps (id) ON DELETE SET NULL,
    step_order             INTEGER NOT NULL,
    action_type            TEXT NOT NULL
                               CHECK (action_type IN ('click', 'type', 'navigate', 'read')),
    target_ui_context_json TEXT NOT NULL DEFAULT '{}'
                               CHECK (json_valid(target_ui_context_json)),
    -- NULL whenever is_sensitive = 1; the CHECK below makes that structural
    -- rather than a convention every caller has to remember.
    data_payload           TEXT,
    is_sensitive           INTEGER NOT NULL DEFAULT 0 CHECK (is_sensitive IN (0, 1)),
    event_type             TEXT NOT NULL
                               CHECK (event_type IN ('execute', 'retry', 'drift_repair',
                                                     'failure', 'aborted')),
    model_source           TEXT,
    cost                   REAL NOT NULL DEFAULT 0.0 CHECK (cost >= 0.0),

    -- FIXED SHAPE: exactly {"foreground_app": "<string>"}. Nothing else goes in
    -- here. feature, playbook_id, step_order and action_type are real columns
    -- on this table or reachable via run_id -> runs; duplicating them into this
    -- blob was a mistake in an earlier draft of this schema and was corrected
    -- deliberately. The triggers below enforce it -- a CHECK constraint cannot,
    -- because counting JSON keys needs a subquery over json_each().
    system_state_json      TEXT NOT NULL
                               CHECK (json_valid(system_state_json)),
    timestamp              TEXT NOT NULL
                               DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    CHECK (is_sensitive = 0 OR data_payload IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_run_steps_log_run_order
    ON run_steps_log (run_id, step_order, timestamp);
-- Feeds confidence_calibration: sweep every logged event for a given model.
CREATE INDEX IF NOT EXISTS idx_run_steps_log_model_source
    ON run_steps_log (model_source, timestamp)
    WHERE model_source IS NOT NULL;

CREATE TRIGGER IF NOT EXISTS trg_run_steps_log_system_state_shape_ins
BEFORE INSERT ON run_steps_log
FOR EACH ROW
WHEN json_type(NEW.system_state_json, '$.foreground_app') IS NOT 'text'
  OR (SELECT count(*) FROM json_each(NEW.system_state_json)) <> 1
BEGIN
    SELECT RAISE(ABORT,
        'system_state_json must be exactly {"foreground_app": "<string>"}');
END;

CREATE TRIGGER IF NOT EXISTS trg_run_steps_log_system_state_shape_upd
BEFORE UPDATE OF system_state_json ON run_steps_log
FOR EACH ROW
WHEN json_type(NEW.system_state_json, '$.foreground_app') IS NOT 'text'
  OR (SELECT count(*) FROM json_each(NEW.system_state_json)) <> 1
BEGIN
    SELECT RAISE(ABORT,
        'system_state_json must be exactly {"foreground_app": "<string>"}');
END;
