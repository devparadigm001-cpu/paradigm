-- Phase 2: the learned mapping and advancement rule, stored alongside the
-- literal example steps rather than instead of them.
--
-- §5 item 5 is explicit that this is "additive to the existing schema, not a
-- replacement": a templated workflow still compiles to ordinary
-- `playbook_steps`, exactly as it does today, and gains a template describing
-- how to repeat the pattern with new data. Nothing here changes what an
-- existing playbook is or how it replays.
--
-- ## What is stored is STRUCTURE
--
-- §3's privacy resolution: the durable mapping records "source column C ->
-- destination column E, advance one row each run" and never a literal reference
-- to specific past rows or their content. So there is no value column here and
-- no row index -- the same rule `workflow_processed_rows` follows, for the same
-- reason.

-- A separate table rather than more columns on `playbooks`, because this is a
-- 1:0..1 relationship and most playbooks are not templated -- putting six
-- template columns on the main table would leave them NULL on nearly every row.
-- The lifecycle state added in migration 20260813000004 stayed on `playbooks`
-- deliberately: that is state every playbook has, this is a definition only
-- some do.
CREATE TABLE IF NOT EXISTS workflow_templates (
    -- PRIMARY KEY, not merely a foreign key. §4.12 locks "one pattern per
    -- workflow", and a primary key makes a second pattern unrepresentable
    -- rather than something every writer has to remember not to do.
    playbook_id      TEXT PRIMARY KEY
                         REFERENCES playbooks (id) ON DELETE CASCADE,
    -- Which source this workflow reads and which destination it writes.
    -- Required for §4.8's new-batch watching, which has to know what to watch.
    source_id        TEXT NOT NULL,
    destination_id   TEXT NOT NULL,
    -- The advancement rule: how far each side moves per record.
    --
    -- source_step may not be zero. §2 treats a source that did not move as
    -- INCONCLUSIVE -- "not as confirmation of a fixed, unchanging value" -- so
    -- a zero-step pattern is precisely the thing detection refuses to call a
    -- pattern, and storing one would smuggle it past that judgement.
    source_step      INTEGER NOT NULL CHECK (source_step <> 0),
    destination_step INTEGER NOT NULL,
    -- The Rule of 3, structurally. Fewer than three examples is
    -- `Detection::TooFewExamples`, never a stored template.
    examples         INTEGER NOT NULL CHECK (examples >= 3),
    created_at       TEXT NOT NULL
                         DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- One row per mapped field. §4.12: a pattern may span several fields as long as
-- they advance together, so this is a list, not a single pair.
CREATE TABLE IF NOT EXISTS workflow_field_mappings (
    id                TEXT PRIMARY KEY,
    -- References the TEMPLATE, not the playbook: a field mapping without a
    -- template describes nothing, and this makes that state unreachable rather
    -- than merely unlikely.
    playbook_id       TEXT NOT NULL
                          REFERENCES workflow_templates (playbook_id)
                          ON DELETE CASCADE,
    source_field      TEXT NOT NULL,
    destination_field TEXT NOT NULL,

    -- §4.12: "field order doesn't matter, field identity does." Identity is the
    -- pair, so the same pair cannot be recorded twice while two different
    -- source fields may legitimately feed two different destinations.
    UNIQUE (playbook_id, source_field, destination_field)
);

-- The one hot query: "what does this workflow map?" -- served by the UNIQUE
-- constraint's index by prefix. No further index, on the same reasoning as
-- migration 20260813000004: a speculative one is dead weight until something
-- measures a need.
