//! Accumulate confidence-calibration evidence.
//!
//! Phase 1 only ACCUMULATES. It bumps `sample_count`, and `success_count` when
//! the call produced usable output, into the reliability histogram created by
//! migration 20260803000003. Phase 2 reads those counts and writes
//! `normalized_score`; nothing here computes a calibrated value.
//!
//! ## What "success" means here, precisely
//!
//! The proxy is *the model's output was usable as generated* -- it validated as
//! `{"label": "<string>"}` without needing brace repair. That is NOT the same
//! as "the user accepted the suggested label", which is the outcome we
//! ultimately want to calibrate against but cannot observe until suggestions
//! are shown to users. Phase 2 should replace this proxy once acceptance is
//! observable, and should treat rows recorded now as measuring well-formedness,
//! not usefulness.

use rusqlite::Connection;

use crate::db::DbError;

use super::LabelOutcome;

/// Bin width for the reliability histogram. 10 bins over [0.0, 1.0].
const BIN_WIDTH: f64 = 0.1;
const BIN_COUNT: usize = 10;

/// One observation ready to be folded into the histogram.
#[derive(Debug, Clone)]
pub struct CalibrationSample {
    pub model_source: String,
    /// The model's own mean token probability for this generation.
    pub raw_score: f64,
    /// See the module docs: well-formed-as-generated, not user-accepted.
    pub success: bool,
}

impl CalibrationSample {
    pub fn from_outcome(outcome: &LabelOutcome) -> Self {
        Self {
            model_source: outcome.model_source.clone(),
            raw_score: outcome.mean_token_probability,
            // Needing repair means the raw generation was malformed, even
            // though we salvaged a label from it.
            success: !outcome.repaired,
        }
    }

    /// Half-open bin `[min, max)` this score falls in.
    ///
    /// A score of exactly 1.0 belongs to the last bin, which is the one case
    /// half-open bins get wrong, so it is clamped rather than left to round
    /// into a nonexistent eleventh bin.
    pub fn bin(&self) -> (f64, f64) {
        let idx = ((self.raw_score / BIN_WIDTH).floor() as usize).min(BIN_COUNT - 1);
        let min = idx as f64 * BIN_WIDTH;
        (round2(min), round2(min + BIN_WIDTH))
    }
}

/// Floats are compared for equality when locating the bin row, so both the
/// stored and the looked-up bounds must come from the same rounding.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Fold a sample into `confidence_calibration`.
///
/// Update-then-insert inside a transaction rather than an upsert: the table has
/// a BEFORE INSERT trigger rejecting overlapping bins, and that trigger fires
/// before SQLite resolves an `ON CONFLICT`, so an upsert on an existing bin
/// would abort as a self-overlap.
pub fn record(conn: &mut Connection, sample: &CalibrationSample) -> Result<(), DbError> {
    let (min, max) = sample.bin();
    let success = i64::from(sample.success);

    let tx = conn.transaction()?;

    let updated = tx.execute(
        "UPDATE confidence_calibration
            SET sample_count  = sample_count + 1,
                success_count = success_count + ?4,
                updated_at    = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
          WHERE model_source = ?1 AND raw_score_min = ?2 AND raw_score_max = ?3",
        (&sample.model_source, min, max, success),
    )?;

    if updated == 0 {
        // Deterministic id keeps a bin's identity stable across processes.
        let id = format!("{}|{:.2}", sample.model_source, min);
        tx.execute(
            "INSERT INTO confidence_calibration
                 (id, model_source, raw_score_min, raw_score_max, sample_count, success_count)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            (&id, &sample.model_source, min, max, success),
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Every bin recorded for a model, oldest bound first. For inspection.
pub fn bins_for(
    conn: &Connection,
    model_source: &str,
) -> Result<Vec<(f64, f64, i64, i64, Option<f64>)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT raw_score_min, raw_score_max, sample_count, success_count, normalized_score
           FROM confidence_calibration
          WHERE model_source = ?1
          ORDER BY raw_score_min",
    )?;
    let rows = stmt.query_map([model_source], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(score: f64, success: bool) -> CalibrationSample {
        CalibrationSample {
            model_source: "test-model".to_string(),
            raw_score: score,
            success,
        }
    }

    #[test]
    fn bins_are_half_open_tenths() {
        assert_eq!(sample(0.0, true).bin(), (0.0, 0.1));
        assert_eq!(sample(0.85, true).bin(), (0.8, 0.9));
        assert_eq!(sample(0.9, true).bin(), (0.9, 1.0));
    }

    #[test]
    fn score_of_exactly_one_lands_in_the_last_bin() {
        // Not (1.0, 1.1): that bin violates the table's raw_score_max <= 1.0.
        assert_eq!(sample(1.0, true).bin(), (0.9, 1.0));
    }
}
