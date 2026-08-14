-- Phase 2, §4.5: what each surface looked like when the workflow was confirmed.
--
-- `detect_drift(recorded, current)` has existed since item 2 and has had no
-- `recorded` to compare against, because nothing stored one. A mapping records
-- that source column C feeds destination column A; it does not record that C
-- was headed "Customer Name" at the time. Without that, a run cannot tell the
-- difference between "column C is still the customer name" and "someone
-- inserted a column and C is now the order date" -- and §4.5 exists precisely
-- to stop the second case writing into the wrong column.
--
-- ## Why storing header labels is consistent with §3
--
-- §3 keeps durable data structural and content transient. A column header is
-- structure: it names the column, the same way the column letter locates it.
-- It is not a row, not a value read during a run, and does not accumulate --
-- one row per mapped column, replaced when the user confirms a correction.
--
-- The distinction that matters: "the column at C is called Customer Name" is a
-- fact about the sheet's shape. "Row 7 of column C says Acme Ltd" is content,
-- and nothing here can hold it -- there is no row column and no value column.

CREATE TABLE IF NOT EXISTS workflow_surface_shape (
    id          TEXT PRIMARY KEY,
    -- References the TEMPLATE, not the playbook: a recorded shape without a
    -- template describes nothing, and the cascade keeps the two together.
    playbook_id TEXT NOT NULL
                    REFERENCES workflow_templates (playbook_id) ON DELETE CASCADE,
    -- Which surface this describes. Both sides are recorded because §4.5 is
    -- explicit that drift is checked on both -- "both sides, not just one" --
    -- and a destination whose columns were reordered is the more dangerous
    -- case, since the run writes there.
    side        TEXT NOT NULL CHECK (side IN ('source', 'destination')),
    -- Where the column is, in that surface's own terms (a spreadsheet column
    -- letter), and what it called itself at confirmation time.
    locator     TEXT NOT NULL,
    label       TEXT NOT NULL,

    -- One record per column per side. A second row for the same locator would
    -- make "what was C called?" ambiguous, which is the one question this
    -- table exists to answer.
    UNIQUE (playbook_id, side, locator)
);

-- The only query: "what did this workflow's surfaces look like?", answered per
-- playbook and usually per side. Served by the UNIQUE constraint's index by
-- prefix, so no separate index -- the same reasoning as migrations
-- 20260813000004 and ...005.
