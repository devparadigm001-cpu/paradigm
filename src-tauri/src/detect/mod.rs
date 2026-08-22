//! Pattern detection on stop -- the Rule-of-3 check from §4.1 and §4.12.
//!
//! Backend item 3. Runs once when a recording ends and answers one question:
//! do these actions describe a repeating, data-driven pattern, or just repeated
//! clicking?
//!
//! ## Deliberately source-agnostic
//!
//! §4 says to read "row" as "the next record from the source" and "column" as
//! "a mapped field". This module takes that literally: it deals in [`Cell`],
//! which is a field plus a record index, and knows nothing about spreadsheets.
//! Converting `"B2"` into a field and a record is the spreadsheet adapter's job
//! -- see [`crate::source::spreadsheet::parse_cell_ref`] -- so a second source
//! type reuses this rule rather than growing its own copy.
//!
//! ## The output is structure, never content
//!
//! §3 locks this: the learned mapping stores "source column C → destination
//! column E, advance one row each run" and *never* a literal reference to
//! specific past rows or their content. [`Pattern`] therefore carries field
//! identities and step sizes only -- no values, and no record indices. It is
//! safe to persist precisely because there is nothing in it to leak, and a test
//! pins that.
//!
//! ## What this module does NOT do
//!
//! It does not obtain its own input. [`detect`] takes [`Observation`]s that
//! already say where a value came from and where it went, and **nothing in the
//! current pipeline produces them.** Two separate gaps, both measured:
//!
//! * **No captured action carries a source position.** `CapturedAction` holds
//!   `element_role`, `element_name`, `payload`, `source_app`, `process_name` and
//!   timestamps -- all describing where a value LANDED. `source_app` is the app
//!   the action happened in, not where the data came from. So the
//!   [`Observation::source`] half has no producer at all today.
//! * **A paste into a grid cell is not captured at all.** `CaptureReport`
//!   records it plainly: "Measured -- a paste into a Google Sheets cell reaches
//!   the document while capture holds no record of it at all." Pastes are
//!   counted, never read. §4.1 detects across "3 pasted/typed values", and §1's
//!   driving example is *copying* orders between sheets, so for the design's own
//!   primary workflow the destination half is missing too. Typed grid entries
//!   ARE captured (`capture::grid`, verified live), so this is specific to
//!   pastes rather than to grids.
//!
//! The rule is built and tested here regardless, because it is the part §4.1 and
//! §4.12 actually specify and it is correct independently of where the input
//! comes from. Wiring it to a producer is a design decision with real privacy
//! weight -- §3 permits source positions only transiently -- and is deliberately
//! left to be settled on evidence rather than answered implicitly by whatever
//! this file assumed.

pub mod candidates;
pub mod link;
pub mod verify;

use std::collections::{BTreeMap, BTreeSet};

/// A position in a source or destination, in the general terms §4 asks for: a
/// field, and which record it belongs to.
///
/// For a spreadsheet, `field` is a column (`"B"`) and `record` is the row,
/// carried as `Declared { scheme: "row", value: "2" }`. That representation is
/// exact -- a row number survives the widening unchanged, so the spreadsheet
/// case behaves identically -- while leaving room for a record identified by
/// something that is not a number at all, which is what a dashboard, an inbox
/// or a listing page provides.
///
/// See `docs/planning/Generalizing-Source-Destination-Tracking.md` §1: the
/// end-state is one general mechanism that happens to handle spreadsheets, not
/// a spreadsheet mechanism with exceptions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell {
    pub field: String,
    pub record: crate::identity::RecordKey,
}

impl Cell {
    /// A spreadsheet-shaped position. The row is stored as a declared identity
    /// under the `"row"` scheme rather than as a bare integer.
    pub fn at_row(field: impl Into<String>, row: i64) -> Self {
        Self {
            field: field.into(),
            record: crate::identity::RecordKey::declared("row", row.to_string()),
        }
    }
}

/// Where a written value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    pub surface: String,
    pub cell: Cell,
}

/// One write seen during a recording.
///
/// `surface` is where the value landed -- a document, a sheet, an inbox. A write
/// with no `source` is one detection cannot relate to the source at all, which
/// is not the same as one that did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// Capture order. Later wins when the same destination is written twice --
    /// see the correction rule in [`detect`].
    pub seq: usize,
    pub surface: String,
    pub destination: Cell,
    pub source: Option<SourceRef>,
}

/// One field of the learned mapping: which source field feeds which
/// destination field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldMapping {
    pub source_field: String,
    pub destination_field: String,
}

/// A detected, coherent pattern. **Structure only** -- see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// Sorted, so two recordings that filled the same fields in a different
    /// order produce the same pattern -- §4.12: "Field order doesn't matter,
    /// field identity does."
    pub fields: Vec<FieldMapping>,
    /// How far the source advances per record. Never zero: a source that does
    /// not move is [`Detection::SourceDidNotAdvance`], not a pattern.
    pub source_step: i64,
    pub destination_step: i64,
    /// How many records the pattern was inferred from. At least 3.
    pub examples: usize,
}

/// Why detection did or did not find a pattern.
///
/// Every negative case is distinct rather than a single "no", because §4.1 and
/// §4.12 prescribe different responses: too few examples means keep recording,
/// a still source means inconclusive, and genuinely unrelated patterns mean the
/// recording should be split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    Pattern(Pattern),
    /// Fewer than three records survived filtering. §2's Rule of 3.
    TooFewExamples { records: usize },
    /// The destination advanced but the source did not. §2 is explicit that
    /// this is inconclusive, NOT confirmation of a fixed, unchanging value.
    SourceDidNotAdvance,
    /// Positions moved, but not by a consistent step.
    InconsistentAdvance { source_steps: Vec<i64>, destination_steps: Vec<i64> },
    /// Records disagree about which source field feeds which destination field.
    InconsistentMapping { signatures: usize },
    /// Two or more genuinely distinct patterns, each self-consistent and each
    /// with enough examples. §4.12: split the recording, do not try to resolve
    /// it here.
    MultiplePatterns { signatures: usize },
}

/// The Rule-of-3 check.
///
/// Steps, in the order §4.1/§4.2/§4.12 describe them:
///
/// 1. **Filter unrelated activity entirely.** An observation that did not land
///    on the destination, or that cannot be traced to the source, is dropped
///    before anything else looks at it -- §2's "actions that never touch the
///    source or destination are excluded entirely".
/// 2. **Collapse corrections.** When the same destination cell is written more
///    than once, only the last write counts. §2: "a value that gets corrected
///    before moving on counts only by its final, settled value -- the
///    correction itself is not treated as a separate step."
/// 3. **Group into records** by destination record index, so field order within
///    a record is irrelevant (§4.12).
/// 4. **Require three.**
/// 5. **Require one coherent mapping.** A pattern may span several fields, as
///    long as every record maps them the same way (§4.12). Records that
///    disagree are either an inconsistency or two distinct patterns.
/// 6. **Require consistent advancement, and require the source to move.**
pub fn detect(
    observations: &[Observation],
    source_surface: &str,
    destination_surface: &str,
) -> Detection {
    // 1. Filter.
    let mut relevant: Vec<&Observation> = observations
        .iter()
        .filter(|o| o.surface == destination_surface)
        .filter(|o| {
            o.source
                .as_ref()
                .map(|s| s.surface == source_surface)
                .unwrap_or(false)
        })
        .collect();

    // 2. Collapse corrections: last write to a destination cell wins.
    relevant.sort_by_key(|o| o.seq);
    let mut settled: BTreeMap<Cell, &Observation> = BTreeMap::new();
    for o in relevant {
        settled.insert(o.destination.clone(), o);
    }

    // 3. Group into records.
    let mut records: BTreeMap<crate::identity::RecordKey, Vec<&Observation>> = BTreeMap::new();
    for o in settled.values() {
        records
            .entry(o.destination.record.clone())
            .or_default()
            .push(o);
    }

    // 4. Rule of 3.
    if records.len() < 3 {
        return Detection::TooFewExamples {
            records: records.len(),
        };
    }

    // 5. One coherent mapping across every record.
    let signature = |writes: &[&Observation]| -> Vec<FieldMapping> {
        let mut m: Vec<FieldMapping> = writes
            .iter()
            .filter_map(|o| {
                o.source.as_ref().map(|s| FieldMapping {
                    source_field: s.cell.field.clone(),
                    destination_field: o.destination.field.clone(),
                })
            })
            .collect();
        // Sorted and deduplicated: field ORDER does not matter, identity does.
        m.sort();
        m.dedup();
        m
    };

    let mut by_signature: BTreeMap<Vec<FieldMapping>, Vec<crate::identity::RecordKey>> =
        BTreeMap::new();
    for (record, writes) in &records {
        by_signature
            .entry(signature(writes))
            .or_default()
            .push(record.clone());
    }

    if by_signature.len() > 1 {
        // Distinguish "two real patterns" from "one pattern with a ragged
        // record". Only a group that could stand on its own -- three records --
        // counts as a pattern in its own right.
        let standalone = by_signature.values().filter(|rs| rs.len() >= 3).count();
        return if standalone >= 2 {
            Detection::MultiplePatterns {
                signatures: standalone,
            }
        } else {
            Detection::InconsistentMapping {
                signatures: by_signature.len(),
            }
        };
    }

    let (fields, record_indices) = by_signature.into_iter().next().expect("one signature");

    // 6. Advancement, both sides.
    //
    // Steps are computed only from records that ARE numbers. A destination
    // still needs arithmetic -- a run has to compute where the next write
    // lands -- so a destination whose records carry no number cannot yield a
    // step, and that is reported rather than invented.
    let destination_numeric: Vec<i64> = record_indices.iter().filter_map(|k| k.numeric()).collect();
    let destination_is_numeric = destination_numeric.len() == record_indices.len();
    let destination_steps = if destination_is_numeric {
        steps(&destination_numeric)
    } else {
        Vec::new()
    };

    // The source's records, in the order their destinations appear. Kept as
    // identities: the source's advancement rule is distinctness, which needs no
    // ordering, so nothing here has to be a number.
    let source_keys: Vec<crate::identity::RecordKey> = records
        .values()
        .filter_map(|writes| {
            writes
                .iter()
                .filter_map(|o| o.source.as_ref().map(|s| s.cell.record.clone()))
                .min()
        })
        .collect();

    // Only for reporting and for the step below -- never for the accept/reject
    // decision, which is `prove_advance`.
    let mut source_records: Vec<i64> = source_keys.iter().filter_map(|k| k.numeric()).collect();
    let source_is_numeric = source_records.len() == source_keys.len();
    source_records.sort_unstable();
    let source_steps = if source_is_numeric {
        steps(&source_records)
    } else {
        Vec::new()
    };

    let uniform = |v: &[i64]| v.windows(2).all(|w| w[0] == w[1]);

    // The DESTINATION must still advance uniformly. This is not the same
    // question as the source's, and §7.1 did not decide it: an irregular
    // destination means writes landing at scattered positions, which is a
    // different claim from "the user skipped a source record". Where the next
    // write LANDS has to be predictable, or a run cannot place it.
    //
    // A destination with no numeric records cannot satisfy that at all -- there
    // is no arithmetic to be uniform -- so it is refused here rather than
    // handed a fabricated step. Writing to a non-numeric destination is a
    // separate piece of work; a source that is not a spreadsheet is not.
    if !destination_is_numeric || !uniform(&destination_steps) {
        return Detection::InconsistentAdvance {
            source_steps,
            destination_steps,
        };
    }

    // The SOURCE's advancement is now distinctness, not difference.
    //
    // Per the planning document's §7.1: a skipped record is still evidence of
    // a genuinely repetitive task. What makes a recording a pattern is the same
    // relationship recurring across DISTINCT records; the distance between them
    // is an artifact of the substrate, and requiring it to be uniform imports a
    // spreadsheet assumption into what is meant to be a general mechanism.
    //
    // `prove_advance` is the general form of the old `source_step == 0` check.
    // That check existed to separate "copy each order in turn" from "type the
    // same thing three times", and that distinction is about each repetition
    // drawing from a DIFFERENT record -- not about how far apart they are. On
    // row numbers the two rules agree exactly; the general one also works on
    // identities that have no difference operator at all -- which is now the
    // real case rather than a hypothetical, since `source_keys` comes straight
    // off the observations instead of being rebuilt from row numbers.
    if !crate::identity::prove_advance(&source_keys).advanced() {
        return Detection::SourceDidNotAdvance;
    }

    // A step still has to be reported, because `CompiledTemplate` and the run
    // loop consume one. A uniform walk keeps exactly the step it always had, so
    // nothing that previously produced a `Pattern` changes.
    //
    // A non-uniform walk reports 1 -- deliberately the smallest step, so the
    // run VISITS every record and lets the ledger decide which to process,
    // rather than striding over records it never checks. See §8.1: a step
    // greater than 1 skips `is_processed` on the records it steps over, and a
    // recording with gaps is precisely the case where those records matter.
    //
    // A source whose records are not numbers at all also reports 1, for the
    // same reason and more strongly: there is no step to preserve, and the run
    // must walk every record so the ledger decides.
    let source_step = if source_is_numeric && uniform(&source_steps) {
        source_steps.first().copied().unwrap_or(0)
    } else {
        1
    };

    Detection::Pattern(Pattern {
        fields,
        source_step,
        destination_step: destination_steps.first().copied().unwrap_or(0),
        examples: records.len(),
    })
}

/// Differences between consecutive positions.
fn steps(positions: &[i64]) -> Vec<i64> {
    positions.windows(2).map(|w| w[1] - w[0]).collect()
}

/// The fields a pattern reads, as a set -- for callers that need to know what
/// to read without caring where it lands.
pub fn source_fields(pattern: &Pattern) -> BTreeSet<String> {
    pattern
        .fields
        .iter()
        .map(|f| f.source_field.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "orders";
    const DST: &str = "shipping";

    fn cell(field: &str, record: i64) -> Cell {
        // Every existing test still speaks in row numbers. `at_row` is the same
        // number in the general representation, which is exactly the claim the
        // spreadsheet case has to keep satisfying.
        Cell::at_row(field, record)
    }

    /// A write from `src_field`/`src_record` into `dst_field`/`dst_record`.
    fn w(seq: usize, src: (&str, i64), dst: (&str, i64)) -> Observation {
        Observation {
            seq,
            surface: DST.to_string(),
            destination: cell(dst.0, dst.1),
            source: Some(SourceRef {
                surface: SRC.to_string(),
                cell: cell(src.0, src.1),
            }),
        }
    }

    /// Three clean records, two fields each, advancing one row at a time.
    fn three_good_records() -> Vec<Observation> {
        vec![
            w(0, ("C", 2), ("B", 5)),
            w(1, ("D", 2), ("E", 5)),
            w(2, ("C", 3), ("B", 6)),
            w(3, ("D", 3), ("E", 6)),
            w(4, ("C", 4), ("B", 7)),
            w(5, ("D", 4), ("E", 7)),
        ]
    }

    #[test]
    fn three_consistent_records_are_a_pattern() {
        match detect(&three_good_records(), SRC, DST) {
            Detection::Pattern(p) => {
                assert_eq!(
                    p.fields,
                    vec![
                        FieldMapping { source_field: "C".into(), destination_field: "B".into() },
                        FieldMapping { source_field: "D".into(), destination_field: "E".into() },
                    ]
                );
                assert_eq!(p.source_step, 1);
                assert_eq!(p.destination_step, 1);
                assert_eq!(p.examples, 3);
            }
            other => panic!("expected a pattern, got {other:?}"),
        }
    }

    #[test]
    fn two_records_are_not_enough() {
        let obs: Vec<Observation> = three_good_records().into_iter().take(4).collect();
        assert_eq!(
            detect(&obs, SRC, DST),
            Detection::TooFewExamples { records: 2 }
        );
    }

    #[test]
    fn a_source_that_never_moves_is_inconclusive_not_a_pattern() {
        // The §2 rule that stops "typed the same thing three times" from being
        // read as "copies row N each time".
        let obs = vec![
            w(0, ("C", 2), ("B", 5)),
            w(1, ("C", 2), ("B", 6)),
            w(2, ("C", 2), ("B", 7)),
        ];
        assert_eq!(detect(&obs, SRC, DST), Detection::SourceDidNotAdvance);
    }

    #[test]
    fn unrelated_activity_is_dropped_entirely() {
        // An accidental tab switch, and a write into some other document. Both
        // must vanish before the Rule of 3 counts anything -- otherwise they
        // would either pad the count or break the rhythm.
        let mut obs = three_good_records();
        obs.push(Observation {
            seq: 99,
            surface: "some other doc".into(),
            destination: cell("A", 1),
            source: Some(SourceRef {
                surface: SRC.into(),
                cell: cell("C", 9),
            }),
        });
        obs.push(Observation {
            seq: 100,
            surface: DST.into(),
            destination: cell("Z", 40),
            source: None, // typed by hand, not from the source
        });
        match detect(&obs, SRC, DST) {
            Detection::Pattern(p) => assert_eq!(p.examples, 3),
            other => panic!("unrelated activity should not disturb detection, got {other:?}"),
        }
    }

    #[test]
    fn a_corrected_value_counts_only_once_by_its_final_write() {
        // Paste wrong, then correct it. §2: the correction is not a separate
        // step, and the settled write is what counts -- so the mapping comes
        // from the LAST write, and the record is not counted twice.
        let mut obs = three_good_records();
        obs.push(w(6, ("Q", 9), ("B", 5))); // wrong source, same destination
        obs.push(w(7, ("C", 2), ("B", 5))); // corrected, back to the real one
        match detect(&obs, SRC, DST) {
            Detection::Pattern(p) => {
                assert_eq!(p.examples, 3, "a correction must not add a record");
                assert!(
                    !p.fields.iter().any(|f| f.source_field == "Q"),
                    "the discarded write must not appear in the mapping: {:?}",
                    p.fields
                );
            }
            other => panic!("expected a pattern, got {other:?}"),
        }
    }

    #[test]
    fn a_multi_field_pattern_is_one_pattern_not_several() {
        // §4.12: several column mappings advancing together are ONE coherent
        // pattern. Three fields, to be sure two is not a special case.
        let obs = vec![
            w(0, ("A", 1), ("B", 1)),
            w(1, ("C", 1), ("D", 1)),
            w(2, ("E", 1), ("F", 1)),
            w(3, ("A", 2), ("B", 2)),
            w(4, ("C", 2), ("D", 2)),
            w(5, ("E", 2), ("F", 2)),
            w(6, ("A", 3), ("B", 3)),
            w(7, ("C", 3), ("D", 3)),
            w(8, ("E", 3), ("F", 3)),
        ];
        match detect(&obs, SRC, DST) {
            Detection::Pattern(p) => assert_eq!(p.fields.len(), 3),
            other => panic!("expected one three-field pattern, got {other:?}"),
        }
    }

    #[test]
    fn field_order_within_a_record_does_not_matter() {
        // Same mapping, filled in a different order each time.
        let obs = vec![
            w(0, ("C", 2), ("B", 5)),
            w(1, ("D", 2), ("E", 5)),
            w(2, ("D", 3), ("E", 6)), // reversed
            w(3, ("C", 3), ("B", 6)),
            w(4, ("D", 4), ("E", 7)), // reversed again
            w(5, ("C", 4), ("B", 7)),
        ];
        match detect(&obs, SRC, DST) {
            Detection::Pattern(p) => assert_eq!(p.examples, 3),
            other => panic!("field order must not matter, got {other:?}"),
        }
    }

    #[test]
    fn two_unrelated_patterns_fail_rather_than_being_handled_at_once() {
        // §4.12: a recording doing two genuinely different jobs should be split
        // by the user, not resolved here. Both halves are self-consistent and
        // both have three records, which is what makes this different from one
        // ragged pattern.
        let mut obs = vec![
            w(0, ("C", 2), ("B", 5)),
            w(1, ("C", 3), ("B", 6)),
            w(2, ("C", 4), ("B", 7)),
        ];
        obs.extend([
            w(3, ("Z", 2), ("Y", 20)),
            w(4, ("Z", 3), ("Y", 21)),
            w(5, ("Z", 4), ("Y", 22)),
        ]);
        assert_eq!(
            detect(&obs, SRC, DST),
            Detection::MultiplePatterns { signatures: 2 }
        );
    }

    #[test]
    fn one_odd_record_is_an_inconsistency_not_a_second_pattern() {
        // The distinction the standalone-count rule exists for: a single record
        // that disagrees cannot stand on its own, so it is a ragged mapping
        // rather than evidence of a second job.
        let mut obs = three_good_records();
        obs.push(w(6, ("Z", 5), ("Y", 8)));
        match detect(&obs, SRC, DST) {
            Detection::InconsistentMapping { .. } => {}
            other => panic!("expected InconsistentMapping, got {other:?}"),
        }
    }

    /// The §7.1 cutover, as the exact scenario that motivated it: the user
    /// processed source rows 2, 3 and 4, deliberately skipped 5, and used 6 as
    /// the fourth example. Destination stays regular at 2, 3, 4, 5.
    ///
    /// This returned `InconsistentAdvance { source_steps: [1, 1, 2] }` before
    /// the cutover, so the recording could never be confirmed or saved.
    #[test]
    fn a_skipped_source_record_is_still_valid_advancement() {
        let obs = vec![
            w(0, ("C", 2), ("B", 2)),
            w(1, ("C", 3), ("B", 3)),
            w(2, ("C", 4), ("B", 4)),
            w(3, ("C", 6), ("B", 5)),
        ];
        match detect(&obs, SRC, DST) {
            Detection::Pattern(p) => {
                assert_eq!(p.examples, 4);
                assert_eq!(p.destination_step, 1);
                // 1, not 2: the run must visit every record so the ledger can
                // decide about the skipped one. See §8.1.
                assert_eq!(p.source_step, 1);
            }
            other => panic!("expected a Pattern, got {other:?}"),
        }
    }

    /// A uniform walk must be completely unaffected -- same step, same
    /// examples. Nothing that already produced a `Pattern` may change.
    #[test]
    fn a_uniform_walk_keeps_exactly_the_step_it_always_had() {
        let obs = vec![
            w(0, ("C", 2), ("B", 2)),
            w(1, ("C", 4), ("B", 3)),
            w(2, ("C", 6), ("B", 4)),
        ];
        match detect(&obs, SRC, DST) {
            Detection::Pattern(p) => {
                assert_eq!(p.source_step, 2, "a step-2 source must stay step 2");
                assert_eq!(p.destination_step, 1);
                assert_eq!(p.examples, 3);
            }
            other => panic!("expected a Pattern, got {other:?}"),
        }
    }

    /// The still-source rule survives the rewrite. Distinctness must reject
    /// what `source_step == 0` used to reject -- this is the same case as
    /// `a_source_that_never_moves_is_inconclusive_not_a_pattern`, kept here as
    /// well because it is now enforced by a different mechanism.
    #[test]
    fn distinctness_still_rejects_a_source_that_repeats_one_record() {
        let obs = vec![
            w(0, ("C", 7), ("B", 2)),
            w(1, ("C", 7), ("B", 3)),
            w(2, ("C", 7), ("B", 4)),
        ];
        assert_eq!(detect(&obs, SRC, DST), Detection::SourceDidNotAdvance);
    }

    /// A partially-still source is still not advancement: two of the three
    /// examples came from the same record.
    #[test]
    fn a_source_that_repeats_one_record_among_others_is_rejected() {
        let obs = vec![
            w(0, ("C", 2), ("B", 2)),
            w(1, ("C", 3), ("B", 3)),
            w(2, ("C", 3), ("B", 4)),
        ];
        assert_eq!(detect(&obs, SRC, DST), Detection::SourceDidNotAdvance);
    }

    #[test]
    fn an_irregular_rhythm_is_not_a_pattern() {
        // Destination jumps 5 -> 6 -> 9. "Consistent, predictable" is the
        // requirement; three writes alone are not enough.
        let obs = vec![
            w(0, ("C", 2), ("B", 5)),
            w(1, ("C", 3), ("B", 6)),
            w(2, ("C", 4), ("B", 9)),
        ];
        match detect(&obs, SRC, DST) {
            Detection::InconsistentAdvance { destination_steps, .. } => {
                assert_eq!(destination_steps, vec![1, 3]);
            }
            other => panic!("expected InconsistentAdvance, got {other:?}"),
        }
    }

    #[test]
    fn a_pattern_carries_no_rows_and_no_values() {
        // §3's rule, structurally. What gets saved is "source C -> destination
        // B, advance 1" and nothing that could identify a past row or its
        // contents. A Debug of the whole pattern is the cheapest way to assert
        // that nothing content-shaped slipped in.
        let Detection::Pattern(p) = detect(&three_good_records(), SRC, DST) else {
            panic!("expected a pattern");
        };
        let printed = format!("{p:?}");
        for row in ["2", "3", "4", "5", "6", "7"] {
            assert!(
                !printed.contains(&format!("record: {row}")),
                "a pattern must not carry record indices: {printed}"
            );
        }
        assert!(printed.contains('C') && printed.contains('B'));
    }
}
