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
--             UNREACHABLE by the current design -- see below. Kept
--             deliberately, not by oversight.
--   confirmed the user said yes. Never ask again -- this is the state 4.2/4.8
--             depend on surviving a restart.
--
-- ## `proposed` is unreachable today, and that is a conclusion, not an oversight
--
-- Checked against the code rather than inferred from the spec. There is exactly
-- ONE production path that writes a `playbooks` row -- `store::store`, called
-- only from `compile_and_store_playbook` (`commands.rs`). `stop_record_session`
-- writes nothing: it hands the captured actions to an in-memory `pending`
-- slot and returns a summary for review.
--
-- So the design's post-stop sequence -- 4.1 detect, 4.12 review, 4.2 confirm --
-- runs entirely before anything is persisted. Whichever way the user answers,
-- the row that eventually gets written is already answered: 'confirmed' if they
-- said yes and the 4.3 preview passed, 'none' otherwise, since 4.10 says a
-- rejected preview leaves "an ordinary one-shot playbook, unaffected". There is
-- no moment at which a detected-but-unanswered workflow exists on disk.
--
-- ## Why it is kept anyway
--
-- Two reasons, and neither is "it might be useful someday":
--
--   * SQLite cannot alter a CHECK constraint without rebuilding the table.
--     An unused enum value costs nothing; adding one later costs a migration
--     against live user data.
--   * Section 7 lists live pattern detection -- detecting WHILE recording
--     rather than on stop -- as a deliberate future option. That flow has no
--     natural "review before store" boundary, so a detected-but-unanswered
--     state is exactly what it would need. This value is the seat kept for it.
--
-- What must NOT happen is code writing 'proposed' to mean something else
-- because an unexplained value was sitting here. It means "detected, not yet
-- answered", and nothing today can produce it.
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
--
-- Known consequence for backend item 7, recorded here because the schema is
-- what makes it possible: this column is durable, but a run is not. 4.10 says a
-- run executes on the app's own background thread and does NOT survive a full
-- app close. So a crash or a close mid-run leaves 'running' or 'paused' behind
-- with nothing executing. Item 7 needs a startup reconciliation -- deciding
-- whether a persisted non-idle state means "resume", "reset to idle", or "ask"
-- -- and 4.6's rule that a paused record is redone cleanly from its start makes
-- resetting safe. Not resolved here: item 1 is schema only, and inventing the
-- policy now would fix it before the code that lives with it exists.
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
-- ## A POSITION MARKER ONLY. Never a value.
--
-- This is a locked privacy decision, not a stylistic one. The foundational
-- design (Section 3) resolves a real tension: storing where a pasted value came
-- FROM was previously considered and rejected on privacy grounds, in
-- multiline-document-capture-duplicates.md. The resolution that makes this
-- feature buildable is that the durable data is *structural* -- "row 47: done"
-- -- while actual source content is handled only transiently, during detection
-- and during each run.
--
-- So this table records that a row was processed and nothing about what it
-- contained. Adding a column here that holds source content -- even "just for
-- debugging" -- would recreate exactly the durable position-plus-content pair
-- the original privacy reasoning refused, and would silently reverse a decision
-- taken deliberately. `a_processed_row_is_a_position_marker_not_a_value` in
-- db::migrations::tests fails if the column set changes, so that reversal
-- cannot happen quietly.
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
