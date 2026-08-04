-- Phase 1: confidence calibration evidence.
--
-- Structure is a reliability histogram, one row per (model_source, score bin).
-- Bins are half-open: raw_score_min <= raw_score < raw_score_max.
--
-- Phase 1 only ACCUMULATES evidence -- as run_steps_log rows land, the engine
-- bumps sample_count, and success_count when the step actually did what the
-- model predicted. That is the "real data" this table collects now.
--
-- Phase 2 owns normalized_score: it reads the accumulated counts and writes the
-- calibrated value back (success_count / sample_count, smoothed). It stays NULL
-- until then, which is why it is nullable -- NULL means "not yet calibrated",
-- and readers must fall back to the raw score rather than treating NULL as 0.
--
-- Storing counts rather than a bare raw->normalized pair is what makes the
-- mapping re-derivable: recalibration is a recompute over evidence that is still
-- here, not a destructive overwrite of a number whose provenance is gone.

CREATE TABLE IF NOT EXISTS confidence_calibration (
    id               TEXT PRIMARY KEY,
    model_source     TEXT NOT NULL,
    raw_score_min    REAL NOT NULL,
    raw_score_max    REAL NOT NULL,
    sample_count     INTEGER NOT NULL DEFAULT 0,
    success_count    INTEGER NOT NULL DEFAULT 0,
    -- Written by Phase 2's calibration pass. NULL = not yet calibrated.
    normalized_score REAL,
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    -- One row per bin per model. This UNIQUE also serves as the lookup index
    -- for "all bins for model X" (model_source is its leftmost column), so no
    -- separate index is needed.
    UNIQUE (model_source, raw_score_min, raw_score_max),

    CHECK (raw_score_min >= 0.0 AND raw_score_max <= 1.0),
    CHECK (raw_score_min < raw_score_max),
    CHECK (sample_count >= 0),
    CHECK (success_count >= 0 AND success_count <= sample_count),
    CHECK (normalized_score IS NULL OR (normalized_score >= 0.0 AND normalized_score <= 1.0))
);

-- Exactly one bin must own any given raw score. UNIQUE above only blocks an
-- identical bin -- it does not stop 0.80-0.90 and 0.85-0.95 coexisting for the
-- same model, which would leave a raw score of 0.87 matching two rows with no
-- defined winner. Overlap detection needs a subquery, so it lives in a trigger
-- rather than a CHECK. The bounds test is half-open, so touching bins
-- (0.80-0.90 and 0.90-1.00) are correctly allowed.
CREATE TRIGGER IF NOT EXISTS trg_confidence_calibration_no_overlap_ins
BEFORE INSERT ON confidence_calibration
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM confidence_calibration c
              WHERE c.model_source = NEW.model_source
                AND c.id <> NEW.id
                AND NEW.raw_score_min < c.raw_score_max
                AND c.raw_score_min < NEW.raw_score_max)
BEGIN
    SELECT RAISE(ABORT, 'confidence_calibration bins may not overlap for a model');
END;

-- Same rule for widening an existing bin into a neighbour.
CREATE TRIGGER IF NOT EXISTS trg_confidence_calibration_no_overlap_upd
BEFORE UPDATE OF raw_score_min, raw_score_max, model_source ON confidence_calibration
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM confidence_calibration c
              WHERE c.model_source = NEW.model_source
                AND c.id <> NEW.id
                AND NEW.raw_score_min < c.raw_score_max
                AND c.raw_score_min < NEW.raw_score_max)
BEGIN
    SELECT RAISE(ABORT, 'confidence_calibration bins may not overlap for a model');
END;

CREATE TRIGGER IF NOT EXISTS trg_confidence_calibration_touch_updated_at
AFTER UPDATE ON confidence_calibration
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE confidence_calibration
       SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     WHERE id = NEW.id;
END;
