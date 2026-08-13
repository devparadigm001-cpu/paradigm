-- Phase 2: templated workflows -- ongoing per-workflow state, and durable
-- per-workflow tracking of which source rows have already been processed.
--
-- Two things a templated workflow needs that a recorded playbook does not:
--
--   * state that OUTLIVES a run. Whether the user has confirmed this is a
--     repeating pattern is asked once and then never again (4.2/4.8), which
--     only works if the answer is durable.
--   * a record of which source rows it has already handled, so re-running with
--     nothing new is recognised and reported rather than silently reprocessing
--     everything or silently doing nothing (4.7).

-- ---------------------------------------------------------------- state ----
--
-- Deliberately NOT layered onto `playbooks.source`. That column answers "which
-- subsystem authored this" (`record_mode` vs `ghost_mode`), and its own comment
-- in migration 20260803000001 says Ghost Mode will write into the same table
-- from Phase 2 onward. Overloading it would collide with exactly that, and
-- would make "recorded by Ghost Mode" and "confirmed as a repeating pattern"
-- inexpressible at the same time.
--
-- `template_state` is one lifecycle column rather than two booleans
-- (`is_templated`, `is_confirmed`). Two booleans admit the combination
-- "not a template, but confirmed", which has no meaning; a lifecycle makes that
-- unrepresentable instead of relying on every caller to avoid it.
--
--   none      an ordinary recorded playbook. The default, so every existing
--             row keeps behaving exactly as it does today.
--   proposed  offered as a repeating pattern, not yet answered by the user.
--   confirmed the user said yes. Never ask again -- this is the state 4.2/4.8
--             depend on surviving a restart.
ALTER TABLE playbooks ADD COLUMN template_state TEXT NOT NULL DEFAULT 'none'
    CHECK (template_state IN ('none', 'proposed', 'confirmed'));

-- When the confirmation happened. NULL unless confirmed, and the CHECK makes
-- that structural rather than a convention every writer has to remember --
-- the same reasoning as `run_steps_log`'s is_sensitive/data_payload pairing.
-- Revoking a confirmation must clear this, which is the intended behaviour
-- rather than an obstacle: a stale timestamp would claim the user answered
-- when they have not.
ALTER TABLE playbooks ADD COLUMN template_confirmed_at TEXT
    CHECK (
        (template_state =  'confirmed' AND template_confirmed_at IS NOT NULL)
     OR (template_state <> 'confirmed' AND template_confirmed_at IS NULL)
    );

-- Whether a templated workflow is currently executing, and whether the user
-- has paused it. Separate from `template_state` because it is orthogonal: a
-- confirmed workflow can be idle, running, or paused, and the two answer
-- different questions.
--
-- Deliberately NOT constrained to templated playbooks. A cross-column CHECK
-- ("only a confirmed workflow may be running") was considered and rejected:
-- Phase 1 replay is synchronous and never persists a status, so the constraint
-- would buy nothing today, while foreclosing Phase 2's retry and drift-repair
-- work marking an ordinary playbook as running. The default keeps every
-- existing row at 'idle' regardless.
ALTER TABLE playbooks ADD COLUMN run_state TEXT NOT NULL DEFAULT 'idle'
    CHECK (run_state IN ('idle', 'running', 'paused'));

-- Finding the workflows that are actually doing something, without scanning
-- every playbook ever recorded. Partial, matching the `idx_runs_playbook`
-- precedent: the overwhelming majority of rows are 'idle' and indexing them
-- would be dead weight.
CREATE INDEX IF NOT EXISTS idx_playbooks_active
    ON playbooks (run_state, updated_at)
    WHERE run_state <> 'idle';

CREATE INDEX IF NOT EXISTS idx_playbooks_template_state
    ON playbooks (template_state, updated_at)
    WHERE template_state <> 'none';

-- ------------------------------------------------- processed source rows ----
--
-- Which specific source rows a given workflow has already handled.
--
-- ## The key is (workflow, source, row) -- and the workflow part is the point
--
-- Tracking by source alone would be a real correctness defect, not a
-- theoretical one (4.13). Two saved workflows can read the SAME source for
-- different purposes -- one copying orders into a shipping sheet, another into
-- an accounting sheet. If they shared tracking, running the first would make
-- the second believe rows it has never touched were already handled, and it
-- would skip them forever. The data loss would be silent and permanent.
--
-- So `playbook_id` is part of the UNIQUE key, not merely a column alongside it.
-- That single decision is what this table exists to encode.
CREATE TABLE IF NOT EXISTS workflow_processed_rows (
    id          TEXT PRIMARY KEY,
    -- The workflow that processed the row. CASCADE, unlike `runs.playbook_id`
    -- which deliberately survives its playbook: run history is an audit record
    -- that outlives what it describes, whereas this is operational state that
    -- means nothing without its workflow. Keeping it would leave rows keyed to
    -- a dead id that nothing can ever read or clear.
    playbook_id TEXT NOT NULL REFERENCES playbooks (id) ON DELETE CASCADE,
    -- Where the row came from -- a spreadsheet document, a sheet within it, a
    -- mailbox. Kept separate from `row_key` so "what has this workflow taken
    -- from that source" is answerable without parsing a composite string.
    source_id   TEXT NOT NULL,
    -- Identity of the row WITHIN that source. Opaque here on purpose: what
    -- makes a row identifiable is the source's business, not this table's.
    row_key     TEXT NOT NULL,
    processed_at TEXT NOT NULL
                     DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    -- The 4.13 invariant, structurally. Note what is NOT here:
    -- UNIQUE (source_id, row_key) would be the sharing bug.
    UNIQUE (playbook_id, source_id, row_key)
);

-- The UNIQUE constraint above already indexes (playbook_id, source_id,
-- row_key), which serves both hot queries -- "has this workflow processed this
-- row" and "what has this workflow processed from this source" -- by prefix.
-- No further index is added, because a speculative one is dead weight until a
-- measurement asks for it.
