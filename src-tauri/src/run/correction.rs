//! §4.5's one-off corrections: "this one order was weird", as distinct from
//! "the format actually changed".
//!
//! The permanent path already existed ([`crate::run::drift::apply_permanent_correction`]):
//! it repoints the stored mapping and re-records the shape, and everything
//! afterwards uses the new column. This is the other half, and it is a
//! genuinely different thing rather than a shorter version of the same thing.
//!
//! ## 1. Where a one-off lives
//!
//! **On the run, never on the template.** A [`CompiledTemplate`] is durable and
//! structural — §3 — and anything written there survives the run, every later
//! run, and the app closing. That is exactly the property a one-off must not
//! have. So corrections live in [`RunCorrections`], a handle held beside
//! [`RunControl`] on the active run: shared with the command layer so the
//! correction panel can add one mid-run, and gone when the run is.
//!
//! Nothing here is serialisable and nothing writes to the database. That is
//! not an omission to fix later; it is the distinction §4.5 is asking for,
//! made structural.
//!
//! ## 2. What makes it expire
//!
//! **It is addressed to a record, so it cannot reach any other one.**
//!
//! A correction names the source row it applies to. The loop asks for
//! corrections *for the record it is about to process*, so a correction for row
//! 7 is simply never returned when the loop is on row 8 — outliving its record
//! is not something that is prevented, it is something that cannot be
//! expressed.
//!
//! That is deliberate in preference to the two obvious alternatives:
//!
//! * **"applies to the next record"** — a positional rule, which silently
//!   attaches to the wrong record the moment anything is skipped as already
//!   processed, and §4.7 means skipping is normal.
//! * **"applies until explicitly cleared"** — a code path that can be
//!   forgotten. §4.5 exists to keep a one-time exception from becoming a
//!   permanent change, and a rule whose enforcement depends on remembering to
//!   call something would collapse precisely that distinction the first time
//!   an error path returned early.
//!
//! It is consumed when the record it names is **marked processed**, not when
//! it is read. A record whose write fails is not done, and a correction that
//! evaporated on a failed attempt would have to be re-entered by the user for
//! a record they had already corrected.
//!
//! ## 3. Where it meets the run loop
//!
//! **At the mapping step, as a substitution. The sequence does not change.**
//!
//! Item 6's loop is read → map → write → mark-done → check → repeat, and all of
//! that stays exactly as it is. The only difference is that "which columns does
//! this record use" stops being a constant read off the template and becomes a
//! function of the template *and* the corrections for this record.
//!
//! That is why this is a small change to a load-bearing loop rather than a
//! large one: a correction cannot reorder the sequence, cannot skip the fit
//! check, cannot write without marking. It can only change which column a value
//! is read from or written to, for one record.

use std::sync::{Arc, Mutex};

use crate::detect::FieldMapping;
use crate::run::drift::Side;

/// A correction that applies to exactly one source record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneOffCorrection {
    /// The source record this applies to, in the source's own terms.
    pub row_key: String,
    /// Which side of the mapping moved.
    pub side: Side,
    /// The locator the template names.
    pub old_locator: String,
    /// The locator to use instead, for this record only.
    pub new_locator: String,
}

/// The corrections attached to a run in progress.
///
/// Cloning shares the same state, so the command layer holds one and the run
/// loop holds another — the same shape as [`RunControl`].
///
/// [`RunControl`]: crate::run::RunControl
#[derive(Clone, Default)]
pub struct RunCorrections {
    inner: Arc<Mutex<Vec<OneOffCorrection>>>,
}

impl std::fmt::Debug for RunCorrections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunCorrections")
            .field("pending", &self.pending())
            .finish()
    }
}

impl RunCorrections {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a correction for a record the run has not reached yet.
    pub fn add(&self, correction: OneOffCorrection) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Re-correcting the same column of the same record replaces rather
        // than stacks. Two corrections for one column would make which one
        // applies depend on insertion order, which is not something a user
        // could reason about.
        inner.retain(|c| {
            !(c.row_key == correction.row_key
                && c.side == correction.side
                && c.old_locator == correction.old_locator)
        });
        inner.push(correction);
    }

    /// How many corrections are waiting. For reporting, not for logic.
    pub fn pending(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The corrections for one record. Does **not** consume them.
    ///
    /// Reading and expiring are separate because a record can be read and then
    /// fail to write, and a correction that vanished on a failed attempt would
    /// have to be entered twice for one record.
    pub fn for_record(&self, row_key: &str) -> Vec<OneOffCorrection> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|c| c.row_key == row_key)
            .cloned()
            .collect()
    }

    /// Expire the corrections for a record, once it is genuinely done.
    ///
    /// Called by the run loop after `mark_processed` succeeds -- the same
    /// moment §4.7 considers the record finished. Returns how many expired.
    pub fn expire(&self, row_key: &str) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = inner.len();
        inner.retain(|c| c.row_key != row_key);
        before - inner.len()
    }
}

/// Apply a record's corrections to the template's field list.
///
/// Pure, and the whole of what a correction can do: swap a locator on one side
/// of one mapped pair. It cannot add a field, remove one, or change which
/// destination a source feeds -- so a correction can never turn one record's
/// write into a different shape from every other record's.
///
/// A correction naming a locator the mapping does not use is ignored rather
/// than treated as an error: it describes a column this workflow does not
/// touch, and refusing the whole record over it would be a worse answer than
/// proceeding with the mapping as recorded.
pub fn apply(fields: &[FieldMapping], corrections: &[OneOffCorrection]) -> Vec<FieldMapping> {
    let mut out = fields.to_vec();
    for correction in corrections {
        for mapping in &mut out {
            let target = match correction.side {
                Side::Source => &mut mapping.source_field,
                Side::Destination => &mut mapping.destination_field,
            };
            if target.eq_ignore_ascii_case(&correction.old_locator) {
                *target = correction.new_locator.clone();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(pairs: &[(&str, &str)]) -> Vec<FieldMapping> {
        pairs
            .iter()
            .map(|(s, d)| FieldMapping {
                source_field: s.to_string(),
                destination_field: d.to_string(),
            })
            .collect()
    }

    fn correction(row: &str, side: Side, old: &str, new: &str) -> OneOffCorrection {
        OneOffCorrection {
            row_key: row.into(),
            side,
            old_locator: old.into(),
            new_locator: new.into(),
        }
    }

    #[test]
    fn a_source_correction_repoints_only_the_matching_pair() {
        let out = apply(
            &mapping(&[("C", "A"), ("D", "B")]),
            &[correction("7", Side::Source, "C", "E")],
        );
        assert_eq!(out, mapping(&[("E", "A"), ("D", "B")]));
    }

    #[test]
    fn a_destination_correction_repoints_the_other_side() {
        let out = apply(
            &mapping(&[("C", "A"), ("D", "B")]),
            &[correction("7", Side::Destination, "B", "F")],
        );
        assert_eq!(out, mapping(&[("C", "A"), ("D", "F")]));
    }

    #[test]
    fn a_correction_for_a_column_the_mapping_does_not_use_changes_nothing() {
        // It describes a column this workflow never touches. Refusing the
        // record over it would be a worse answer than proceeding as recorded.
        let out = apply(
            &mapping(&[("C", "A")]),
            &[correction("7", Side::Source, "Z", "Y")],
        );
        assert_eq!(out, mapping(&[("C", "A")]));
    }

    #[test]
    fn corrections_cannot_add_or_remove_fields() {
        // The shape of the write is fixed by the template. A correction moves a
        // column; it cannot make one record write a different set of cells.
        let out = apply(
            &mapping(&[("C", "A"), ("D", "B")]),
            &[
                correction("7", Side::Source, "C", "E"),
                correction("7", Side::Destination, "B", "F"),
            ],
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out, mapping(&[("E", "A"), ("D", "F")]));
    }

    #[test]
    fn a_correction_is_only_returned_for_the_record_it_names() {
        // The heart of it: outliving its record is not prevented, it is
        // inexpressible.
        let c = RunCorrections::new();
        c.add(correction("7", Side::Source, "C", "E"));

        assert_eq!(c.for_record("7").len(), 1);
        assert!(c.for_record("8").is_empty());
        assert!(c.for_record("6").is_empty());
    }

    #[test]
    fn reading_a_correction_does_not_expire_it() {
        // A record can be read and then fail to write. A correction that
        // vanished on the attempt would have to be entered twice.
        let c = RunCorrections::new();
        c.add(correction("7", Side::Source, "C", "E"));
        assert_eq!(c.for_record("7").len(), 1);
        assert_eq!(c.for_record("7").len(), 1, "still there after a read");
        assert_eq!(c.pending(), 1);
    }

    #[test]
    fn expiring_removes_only_that_records_corrections() {
        let c = RunCorrections::new();
        c.add(correction("7", Side::Source, "C", "E"));
        c.add(correction("8", Side::Source, "C", "F"));

        assert_eq!(c.expire("7"), 1);
        assert!(c.for_record("7").is_empty());
        assert_eq!(c.for_record("8").len(), 1, "row 8's correction is untouched");
    }

    #[test]
    fn re_correcting_the_same_column_replaces_rather_than_stacks() {
        // Two corrections for one column would make the winner depend on
        // insertion order, which a user cannot reason about.
        let c = RunCorrections::new();
        c.add(correction("7", Side::Source, "C", "E"));
        c.add(correction("7", Side::Source, "C", "G"));

        let found = c.for_record("7");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].new_locator, "G");
    }

    #[test]
    fn correcting_two_different_columns_of_one_record_keeps_both() {
        let c = RunCorrections::new();
        c.add(correction("7", Side::Source, "C", "E"));
        c.add(correction("7", Side::Destination, "B", "F"));
        assert_eq!(c.for_record("7").len(), 2);
    }

    #[test]
    fn clones_share_one_set() {
        // The command layer holds one and the run loop holds another.
        let a = RunCorrections::new();
        let b = a.clone();
        a.add(correction("7", Side::Source, "C", "E"));
        assert_eq!(b.for_record("7").len(), 1);
        b.expire("7");
        assert_eq!(a.pending(), 0);
    }
}
