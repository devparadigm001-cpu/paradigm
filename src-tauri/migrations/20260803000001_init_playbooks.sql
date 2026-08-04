-- Phase 1: Shared Engine + Record Mode.
-- Playbook definitions and their compiled steps.
--
-- SQLite has no native enum type, so every enum column is constrained with a
-- CHECK. Timestamps are ISO-8601 UTC text ('2026-08-03T14:22:31.417Z') so they
-- sort lexicographically and survive a round-trip through JSON unchanged.

CREATE TABLE IF NOT EXISTS playbooks (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    -- Record Mode is the only Phase 1 writer, but Ghost Mode promotes its
    -- observed patterns into this same table from Phase 2 onward.
    source     TEXT NOT NULL CHECK (source IN ('record_mode', 'ghost_mode')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_playbooks_source_updated
    ON playbooks (source, updated_at);

-- Keep updated_at honest without making every caller remember to set it.
-- The WHEN guard stops the trigger from firing on its own write.
CREATE TRIGGER IF NOT EXISTS trg_playbooks_touch_updated_at
AFTER UPDATE ON playbooks
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE playbooks
       SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = NEW.id;
END;

CREATE TABLE IF NOT EXISTS playbook_steps (
    id                  TEXT PRIMARY KEY,
    playbook_id         TEXT NOT NULL
                            REFERENCES playbooks (id) ON DELETE CASCADE,
    step_order          INTEGER NOT NULL,
    action_type         TEXT NOT NULL
                            CHECK (action_type IN ('click', 'type', 'navigate', 'read')),
    control_role        TEXT NOT NULL
                            CHECK (control_role IN ('button', 'textbox', 'dropdown',
                                                    'checkbox', 'radio', 'link', 'other')),
    -- NULL until the compile step assigns it. Never user-set. The compile step
    -- does not exist yet, so Phase 1 leaves this NULL on every row it writes.
    reversible          INTEGER CHECK (reversible IN (0, 1)),
    action_payload_json TEXT NOT NULL DEFAULT '{}'
                            CHECK (json_valid(action_payload_json)),

    -- Steps within a playbook are densely ordered. Reordering must shift rows
    -- through a temporary offset -- SQLite checks UNIQUE per statement, not
    -- deferred to commit.
    UNIQUE (playbook_id, step_order)
);
