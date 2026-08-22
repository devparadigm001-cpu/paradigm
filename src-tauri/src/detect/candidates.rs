//! Which actions are worth asking the user about, after a recording stops.
//!
//! This is the filter from `docs/planning/Filtered-Post-Hoc-Confirmation.md`,
//! built as a library function. Recording needs no marker: the raw action
//! stream is reduced to a small set of repeated, field-shaped actions, and the
//! user confirms or unchecks them.
//!
//! **The filter decides what to ASK about. It never decides the answer.** That
//! is the whole design, and it is a response to three separate failures, each
//! measured rather than assumed:
//!
//! * pure structural inference produced *identical* feature vectors for a
//!   meaningful field and a systematically-checked incidental one;
//! * a model judging intent was **more** confident on undecidable cases (0.9162
//!   mean) than on decidable ones (0.8910), so no escalation threshold exists;
//! * a model naming fields returned a neighbouring token rather than a
//!   classification, and was unstable across records of one column.
//!
//! Nothing can infer intent from a recording. So this does not try. Repetition
//! is used as a filter on the question set and never as an oracle.
//!
//! ## The pipeline
//!
//! 0. the raw action stream
//! 1. drop `Navigate` -- context, never a field candidate
//! 2. drop actions carrying no usable identity
//! 3. group by **field**: a spreadsheet cell by its column, a page element by
//!    its position within a record
//! 4. keep groups covering at least [`RULE_OF_THREE`] distinct records
//!
//! ## Pure, and deliberately so
//!
//! [`candidates`] is a function of `&[CapturedAction]` and nothing else. No
//! desktop, no automation handle, no database. Every rule below is therefore
//! testable against captured fixtures, which is the standard the rest of this
//! crate holds -- `identity::tree` is pure for the same reason and the two
//! share it on purpose.
//!
//! ## Three known gaps, tracked and NOT fixed here
//!
//! All three are real, all three will show up in output, and each is documented
//! so that seeing it is recognition rather than a fresh investigation.
//!
//! **1. Field labels are missing, or WRONG.** Confirmed on four applications --
//! Gmail, Amazon, OrderFlow and the ChatGPT pricing page. A page element often
//! has no label beside it, so a candidate surfaces described only by its
//! position.
//!
//! The worse half, measured on 2026-08-22: a label that repeats in every record
//! is structural by the multiplicity rule and gets adopted as the field's name,
//! so an order status became the label for the value beside it -- `el/0/Pending`
//! rather than `el/0/CUSTOMER`. **A confidently wrong name is worse than none**,
//! because an empty label at least announces that it does not know. See
//! `docs/known-issues/an-element-identity-mark-records-no-field-label.md`.
//!
//! The grouping does not depend on labels, which is why it still works; the
//! description the user reads is what suffers.
//!
//! **3. Positions from two windows of one application are pooled.** The record
//! pitch is derived per `source_app`, which is the PROCESS name -- so a browser
//! page and a browser-hosted spreadsheet are both `msedge.exe` and share one
//! pitch. On the first natural recording that manufactured a fourth record for a
//! page showing three. See
//! `docs/known-issues/an-action-cannot-say-which-window-it-happened-in.md`; the
//! discriminator this needs is already computed by
//! `capture::grid::page_identity` and thrown away.
//!
//! **2. A scrolled session can inflate the record count.** `element_bounds` is
//! viewport-relative, so scrolling moves an element without the element
//! changing. Two observations of ONE record at two scroll positions can
//! therefore land in two record bands and look like two records. With the Rule
//! of 3 that means a candidate could in principle surface from a single record
//! read three times while scrolling. See
//! `docs/known-issues/element-bounds-are-viewport-relative-so-scrolling-moves-them.md`;
//! it is defect 1 there, and this module inherits it rather than introducing
//! it. The spreadsheet path is immune, because a cell reference is not a pixel.
//!
//! Reproduced live rather than argued, and pinned by a deliberately FAILING
//! test -- `one_element_seen_at_three_scroll_positions_is_not_three_records`.
//! The obvious guard was checked and does not work: three "records" sharing one
//! element name looks like a signal, but a real page carries `Pending` three
//! times at one x and one width, once per genuine record. A repeated value and
//! a repeated observation are identical in every property the action stream
//! holds, so the separation has to come from OBSERVING the scroll.

use std::collections::{BTreeMap, BTreeSet};

use crate::capture::stream::{ActionKind, CapturedAction};
use crate::source::spreadsheet::parse_cell_ref;

/// How many distinct records a field must appear in to be worth asking about.
///
/// §4.1's Rule of 3, and the same threshold [`super::detect`] applies. Over
/// **records**, never occurrences: three clicks on one record is one example
/// repeated, not three examples.
pub const RULE_OF_THREE: usize = 3;

/// Pairwise y-differences within this many pixels are the same candidate pitch.
pub const RECORD_PITCH_TOLERANCE_PX: f64 = 10.0;

/// Smallest y-difference allowed to be a record pitch.
///
/// **The weakest number in this module, and it is a calibration rather than a
/// finding.** It exists because on the one real layout measured, the spread
/// *within* a record (116px) was larger than the gap *between* records (99px),
/// so the offsets between fields of one record would otherwise win the vote and
/// be mistaken for the record spacing.
///
/// 120px clears that layout's within-record spread. A denser list would defeat
/// it. [`candidates`] therefore validates the pitch it finds rather than
/// trusting it -- see [`assign_records`] -- so a wrong pitch declines instead of
/// producing confident nonsense.
pub const RECORD_PITCH_FLOOR_PX: f64 = 120.0;

/// Elements whose offset within a record differs by less than this are on the
/// same visual row, and are ranked left to right against each other.
pub const ROW_BAND_PX: f64 = 20.0;

/// One thing the user will be asked to confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldCandidate {
    /// Stable within one [`CandidateSet`], and the handle the UI checks off.
    pub id: String,
    /// What the user reads. Structure only -- never a captured value.
    pub detail: String,
    pub kind: ActionKind,
    /// How many distinct records this field was touched in. The number the
    /// Rule of 3 is applied to.
    pub distinct_records: usize,
    /// Indices into the slice passed to [`candidates`], ascending.
    pub action_indices: Vec<usize>,
}

impl FieldCandidate {
    pub fn occurrences(&self) -> usize {
        self.action_indices.len()
    }
}

/// How many actions survived each stage.
///
/// Carried out of the same pass that produces the groups rather than
/// recomputed, so the reported funnel and the actual filtering can never
/// disagree -- the reason `identity::tree` shares one walk between `records`
/// and `locate`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Funnel {
    /// Stage 0.
    pub raw: usize,
    /// Stage 1: after dropping `Navigate`.
    pub after_navigate: usize,
    /// Stage 2: after dropping actions with no cell reference and no position.
    pub with_identity: usize,
    /// Stage 3: distinct field groups, before the Rule of 3.
    pub field_groups: usize,
    /// Stage 4: groups covering at least [`RULE_OF_THREE`] records.
    pub surviving: usize,
}

/// Everything worth asking about in one recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateSet {
    pub groups: Vec<FieldCandidate>,
    pub funnel: Funnel,
}

impl CandidateSet {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// The candidate an action belongs to, by its index in the original slice.
    ///
    /// This is what lets the review screen mark individual steps rather than
    /// only listing groups.
    pub fn candidate_of(&self, action_index: usize) -> Option<&str> {
        self.groups
            .iter()
            .find(|g| g.action_indices.contains(&action_index))
            .map(|g| g.id.as_str())
    }
}

/// A field, expressed so that the same field in different records collides.
///
/// Values differ across records by design -- that is what makes them data -- so
/// the value can never be the key. The key is a spreadsheet column, or a
/// position within the record's own layout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FieldKey {
    Column {
        kind: &'static str,
        column: String,
    },
    Position {
        kind: &'static str,
        /// The application the position belongs to.
        ///
        /// **Positions from two applications are never the same field**, and
        /// they are never even measured together: the record pitch is derived
        /// per application. A pitch computed across two windows is arithmetic
        /// on unrelated coordinate systems, and on the first natural recording
        /// it manufactured a fourth record for a page that has three.
        app: String,
        /// Which visual row within a record, in [`ROW_BAND_PX`] units.
        band: i64,
        /// Rank left-to-right within that row. **Rank, not absolute x.**
        /// Columns are cleanly separated within any one record; it is only
        /// their absolute positions that drift between records, and a rank is
        /// immune to that drift.
        rank: usize,
    },
}

impl FieldKey {
    fn detail(&self) -> String {
        match self {
            FieldKey::Column { kind, column } => {
                format!("{kind} into column {column} of the spreadsheet")
            }
            FieldKey::Position {
                kind,
                app,
                band,
                rank,
            } => {
                let ordinal = match rank {
                    0 => "1st".to_string(),
                    1 => "2nd".to_string(),
                    2 => "3rd".to_string(),
                    n => format!("{}th", n + 1),
                };
                format!(
                    "{kind} on the {ordinal} element across, {}px into each record, in {app}",
                    (*band as f64 * ROW_BAND_PX) as i64
                )
            }
        }
    }

    fn kind(&self) -> ActionKind {
        let k = match self {
            FieldKey::Column { kind, .. } | FieldKey::Position { kind, .. } => *kind,
        };
        match k {
            "type" => ActionKind::Type,
            "read" => ActionKind::Read,
            _ => ActionKind::Click,
        }
    }
}

/// A page element that survived stage 2, kept with where it came from.
struct Positioned {
    index: usize,
    kind: &'static str,
    /// Which application's coordinate system `x` and `y` belong to.
    app: String,
    x: f64,
    y: f64,
}

/// The record pitch: the most common pairwise y-difference above the floor.
///
/// `None` when no difference recurs, which is the honest answer for a page that
/// is not a repeating list.
///
/// Proximity clustering, **not** fixed-width buckets. Rounding 214 and 215 into
/// adjacent buckets split the true pitch in half and let its own 2x harmonic tie
/// with it and win -- measured 2026-08-20, and the reason this reads the way it
/// does.
fn record_pitch(ys: &[f64]) -> Option<f64> {
    let mut diffs: Vec<f64> = Vec::new();
    for (i, a) in ys.iter().enumerate() {
        for b in ys.iter().skip(i + 1) {
            let d = (b - a).abs();
            if d >= RECORD_PITCH_FLOOR_PX {
                diffs.push(d);
            }
        }
    }
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut clusters: Vec<Vec<f64>> = Vec::new();
    for d in diffs {
        match clusters.last_mut() {
            Some(c) if d - c[c.len() - 1] <= RECORD_PITCH_TOLERANCE_PX => c.push(d),
            _ => clusters.push(vec![d]),
        }
    }

    let best = clusters.iter().filter(|c| c.len() >= 2).map(|c| c.len()).max()?;
    // Among equally-supported clusters take the SMALLEST. A repeating list
    // always produces harmonics -- with N records the pitch appears N-1 times
    // and twice the pitch N-2 times -- and the fundamental is the actual record
    // spacing.
    clusters
        .iter()
        .filter(|c| c.len() == best)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .min_by(|a, b| a.partial_cmp(b).unwrap())
}

/// Assign page elements to records and fields, or decline.
///
/// Returns `(field key, record label)` per element, in input order.
///
/// **Validates the assignment rather than trusting it.** If two elements land
/// at the same record, the same row band and the same rank, the key cannot tell
/// them apart, and the honest response is to produce nothing.
///
/// The usual cause is a mis-detected pitch folding two records into one. It is
/// not the only one -- two genuinely stacked elements at the same x, close
/// enough in y to round into one band, collide the same way -- and this does not
/// try to distinguish those, because the consequence is identical either way:
/// a key that addresses two different things.
///
/// **The decline is all-or-nothing for the page side.** One collision anywhere
/// drops every positional candidate in the recording; the spreadsheet half is
/// unaffected, since it never enters here. That is deliberately blunt. A
/// partial answer would mean reporting some fields as grouped while silently
/// omitting others, which reads as "these are the repeated fields" when it is
/// not.
fn assign_records(items: &[Positioned]) -> Option<Vec<(FieldKey, String)>> {
    let ys: Vec<f64> = items.iter().map(|p| p.y).collect();
    let pitch = record_pitch(&ys)?;
    let y_min = ys.iter().cloned().fold(f64::INFINITY, f64::min);

    // record -> band -> the xs sitting there, so rank can be taken within a row.
    let mut rows: BTreeMap<(i64, i64), Vec<f64>> = BTreeMap::new();
    let mut placed: Vec<(i64, i64, f64)> = Vec::with_capacity(items.len());
    for p in items {
        let record = ((p.y - y_min) / pitch).floor() as i64;
        let band = ((p.y - (y_min + record as f64 * pitch)) / ROW_BAND_PX).round() as i64;
        rows.entry((record, band)).or_default().push(p.x);
        placed.push((record, band, p.x));
    }
    for xs in rows.values_mut() {
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        xs.dedup();
    }

    let mut out = Vec::with_capacity(items.len());
    // key -> the y that claimed it. The VALUE is what makes this a real check
    // rather than a repeat-detector: see below.
    let mut seen: BTreeMap<(i64, i64, usize), i64> = BTreeMap::new();
    for (p, (record, band, x)) in items.iter().zip(placed) {
        let rank = rows
            .get(&(record, band))?
            .iter()
            .position(|c| (*c - x).abs() < f64::EPSILON)?;
        // The validation, and it must compare the Y rather than merely notice a
        // repeat.
        //
        // Rank comes from a deduplicated x list, so two elements at one key
        // always share an x. Two clicks on the SAME element are then
        // indistinguishable from two records folded together by a bad pitch --
        // unless the y is checked, which separates them exactly: the same
        // element clicked twice has one y, a fold has two.
        //
        // Rejecting on the repeat alone -- which is what this did until
        // 2026-08-22 -- makes the whole page side collapse on any real
        // recording. Measured, on the first natural one: 179 clicks over 24
        // distinct positions, 23 of them clicked more than once and one hit 59
        // times. Clicking the same thing twice is ordinary, not ambiguous.
        let y_key = p.y.round() as i64;
        if *seen.entry((record, band, rank)).or_insert(y_key) != y_key {
            return None;
        }
        out.push((
            FieldKey::Position {
                kind: p.kind,
                app: p.app.clone(),
                band,
                rank,
            },
            format!("r{record}"),
        ));
    }
    Some(out)
}

/// Run [`assign_records`] once per application, and keep what succeeds.
///
/// **A pitch across two windows is arithmetic on unrelated coordinate
/// systems.** Measured on session record-1a2123c0: pooling an OrderFlow page
/// with a Google Sheets window produced a candidate claiming FOUR records for a
/// page that shows three, because the sheet's y values entered the same
/// division. Partitioning is not a refinement of the rule; without it the rule
/// is computing something that does not mean anything.
///
/// A decline stays all-or-nothing **within** an application and no longer takes
/// the others down with it, which is the one thing partitioning improves beyond
/// correctness.
///
/// **It does not separate two WINDOWS of one application, and that is the case
/// that actually bites.** `source_app` is the process name, so a browser page
/// and a browser-hosted spreadsheet are both `msedge.exe` and stay pooled —
/// verified on record-1a2123c0, where adding this partition changed the result
/// not at all. The four-records-from-three artifact there survives it.
///
/// Fixing that needs a per-window or per-document discriminator on
/// `CapturedAction`, which capture does not currently store even though
/// `capture::grid::page_identity` already computes a URL for the position path.
/// So this partition is correct and insufficient, and is kept for what it does
/// prevent: pooling a desktop application's coordinates with a browser's.
fn assign_records_per_app(items: &[Positioned]) -> Vec<(usize, FieldKey, String)> {
    let mut by_app: BTreeMap<&str, Vec<&Positioned>> = BTreeMap::new();
    for p in items {
        by_app.entry(p.app.as_str()).or_default().push(p);
    }

    let mut out = Vec::new();
    for group in by_app.values() {
        let owned: Vec<Positioned> = group
            .iter()
            .map(|p| Positioned {
                index: p.index,
                kind: p.kind,
                app: p.app.clone(),
                x: p.x,
                y: p.y,
            })
            .collect();
        if let Some(assigned) = assign_records(&owned) {
            for (p, (key, record)) in group.iter().zip(assigned) {
                out.push((p.index, key, record));
            }
        }
    }
    out
}

/// Reduce a recording to the things worth asking the user about.
///
/// Pure. Deterministic. Ordered most-repeated first, then by field, so the same
/// recording always produces the same list in the same order.
pub fn candidates(actions: &[CapturedAction]) -> CandidateSet {
    // Stage 1 and 2 together, because both are per-action decisions.
    //
    // Stage 2 is "no usable identity", and it means something stronger here
    // than it did in the prototype. The prototype had no captured bounds to
    // work with and so required a usable NAME; an element with a position but
    // no name was dropped. That is precisely the case the empty-label defect
    // produces, and dropping it would discard most page-side sources. An action
    // is usable if it has a cell reference OR a position.
    let mut columns: Vec<(usize, FieldKey, String)> = Vec::new();
    let mut positioned: Vec<Positioned> = Vec::new();
    let mut funnel = Funnel {
        raw: actions.len(),
        ..Funnel::default()
    };

    for (index, action) in actions.iter().enumerate() {
        // Stage 1. A window switch is context: it says where the user went, and
        // is never itself a field being transferred.
        if action.kind == ActionKind::Navigate {
            continue;
        }
        funnel.after_navigate += 1;
        let kind = action.kind.as_str();

        // A spreadsheet cell resolves exactly, so it is preferred over geometry
        // wherever it is available -- and it is immune to the scroll problem in
        // this module's header.
        if let Some(name) = action.element_name.as_deref() {
            if let Some((column, row)) = parse_cell_ref(name) {
                funnel.with_identity += 1;
                columns.push((index, FieldKey::Column { kind, column }, row.to_string()));
                continue;
            }
        }

        if let Some((x, y, _w, _h)) = action.element_bounds {
            funnel.with_identity += 1;
            positioned.push(Positioned {
                index,
                kind,
                app: action.source_app.clone(),
                x,
                y,
            });
        }
        // Everything else has no identity this can group by, and is dropped.
    }

    // Stage 3.
    let mut grouped: BTreeMap<FieldKey, (BTreeSet<String>, Vec<usize>)> = BTreeMap::new();
    for (index, key, record) in columns {
        let e = grouped.entry(key).or_default();
        e.0.insert(record);
        e.1.push(index);
    }
    for (index, key, record) in assign_records_per_app(&positioned) {
        let e = grouped.entry(key).or_default();
        e.0.insert(record);
        e.1.push(index);
    }

    funnel.field_groups = grouped.len();

    // Stage 4.
    let mut surviving: Vec<(FieldKey, BTreeSet<String>, Vec<usize>)> = grouped
        .into_iter()
        .filter(|(_, (records, _))| records.len() >= RULE_OF_THREE)
        .map(|(key, (records, indices))| (key, records, indices))
        .collect();
    // Most-repeated first, then by field key, which is total -- so the order is
    // fully determined and a test can pin it.
    surviving.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    funnel.surviving = surviving.len();

    CandidateSet {
        funnel,
        groups: surviving
            .into_iter()
            .enumerate()
            .map(|(n, (key, records, mut indices))| {
                indices.sort_unstable();
                FieldCandidate {
                    id: format!("cand-{}", n + 1),
                    detail: key.detail(),
                    kind: key.kind(),
                    distinct_records: records.len(),
                    action_indices: indices,
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::exclusion::ExclusionList;
    use crate::capture::stream::{ActionCandidate, CapturedStream};

    /// Build a real `CapturedAction` by pushing through the gate, because there
    /// is no other way to make one -- and that is deliberate, so a test cannot
    /// assemble an ungated action that the product could never produce.
    fn actions(specs: Vec<(ActionKind, Option<&str>, Option<(f64, f64, f64, f64)>)>) -> Vec<CapturedAction> {
        let mut stream = CapturedStream::new(ExclusionList::from_patterns(Vec::<String>::new()));
        for (kind, name, bounds) in specs {
            stream.admit(ActionCandidate {
                kind,
                identifiers: vec!["msedge.exe".into()],
                process_name: Some("msedge.exe".into()),
                element_role: Some("Text".into()),
                element_name: name.map(str::to_string),
                payload: None,
                detail: None,
                element_bounds: bounds,
                timestamp_ms: 0,
            });
        }
        stream.actions().to_vec()
    }

    fn cell<'a>(name: &'a str) -> (ActionKind, Option<&'a str>, Option<(f64, f64, f64, f64)>) {
        (ActionKind::Type, Some(name), None)
    }

    fn at(x: f64, y: f64) -> (ActionKind, Option<&'static str>, Option<(f64, f64, f64, f64)>) {
        (ActionKind::Click, None, Some((x, y, 50.0, 20.0)))
    }

    #[test]
    fn a_column_touched_in_three_rows_is_a_candidate() {
        let set = candidates(&actions(vec![cell("A2"), cell("A3"), cell("A4")]));
        assert_eq!(set.groups.len(), 1);
        assert_eq!(set.groups[0].distinct_records, 3);
        assert_eq!(set.groups[0].kind, ActionKind::Type);
        assert!(set.groups[0].detail.contains("column A"));
    }

    #[test]
    fn two_rows_are_not_enough() {
        let set = candidates(&actions(vec![cell("A2"), cell("A3")]));
        assert!(set.is_empty(), "the Rule of 3 is three, not two");
    }

    /// The rule is over RECORDS. One row touched repeatedly is one example.
    #[test]
    fn one_row_touched_five_times_is_still_one_record() {
        let set = candidates(&actions(vec![
            cell("A2"),
            cell("A2"),
            cell("A2"),
            cell("A2"),
            cell("A2"),
        ]));
        assert!(set.is_empty());
    }

    #[test]
    fn navigate_is_context_and_never_a_candidate() {
        let mut specs = vec![
            (ActionKind::Navigate, Some("A2"), None),
            (ActionKind::Navigate, Some("A3"), None),
            (ActionKind::Navigate, Some("A4"), None),
        ];
        specs.push(cell("B2"));
        let set = candidates(&actions(specs));
        assert!(
            set.groups.iter().all(|g| g.kind != ActionKind::Navigate),
            "a window switch is never a field"
        );
    }

    #[test]
    fn an_action_with_neither_a_cell_nor_a_position_is_dropped() {
        let set = candidates(&actions(vec![
            (ActionKind::Click, Some("Save"), None),
            (ActionKind::Click, Some("Save"), None),
            (ActionKind::Click, Some("Save"), None),
        ]));
        assert!(
            set.is_empty(),
            "a repeated button click has no field identity to group by"
        );
    }

    /// The empty-label case, which is the common one on a page. No names at all,
    /// and the grouping still works because it never needed them.
    #[test]
    fn unlabelled_page_elements_still_group_by_position() {
        // Three records, 200px apart, two fields each.
        let set = candidates(&actions(vec![
            at(100.0, 100.0),
            at(300.0, 100.0),
            at(100.0, 300.0),
            at(300.0, 300.0),
            at(100.0, 500.0),
            at(300.0, 500.0),
        ]));
        assert_eq!(set.groups.len(), 2, "two fields, three records each");
        for g in &set.groups {
            assert_eq!(g.distinct_records, 3);
            assert_eq!(g.occurrences(), 3);
        }
    }

    /// Absolute x drifts between records; rank does not. This is the case
    /// x-banding got wrong before 2-D clustering replaced it.
    #[test]
    fn a_field_is_found_by_rank_even_when_its_x_drifts() {
        let set = candidates(&actions(vec![
            at(100.0, 100.0),
            at(300.0, 100.0),
            at(140.0, 300.0), // drifted right
            at(360.0, 300.0),
            at(80.0, 500.0), // drifted left
            at(280.0, 500.0),
        ]));
        assert_eq!(
            set.groups.len(),
            2,
            "drifting x must not split one field into several"
        );
        assert!(set.groups.iter().all(|g| g.distinct_records == 3));
    }

    /// Clicking the same element more than once must not destroy the grouping.
    ///
    /// Regression for a defect the first natural recording exposed on
    /// 2026-08-22 and no synthetic fixture had: 179 page clicks landed on 24
    /// distinct positions, 23 of them clicked more than once, and one position
    /// was clicked **59 times**. The collision check rejected on the repeat
    /// alone, so `assign_records` declined and every page-side candidate in the
    /// recording vanished -- 0 groups from 179 clicks.
    ///
    /// A person clicking the same thing twice is ordinary. Only two DIFFERENT
    /// ys at one key is a real fold.
    ///
    /// **The fixture is the real click distribution, not a tidy one.** A clean
    /// fixture -- one click per field per record -- is exactly what failed to
    /// catch this, because the defect only appears once a position repeats. The
    /// counts below are transcribed from record-1a2123c0: the OrderFlow page,
    /// three orders, with the click count each position actually received.
    #[test]
    fn repeated_clicks_on_one_element_do_not_collapse_the_page_side() {
        // (x, y, times clicked) -- measured, not invented. Three records at
        // y234/407/580 for the status and y259/432/605 for the customer, plus
        // the product row at y327/500/673 where one position was hit 10 times.
        let measured: [(f64, f64, usize); 12] = [
            (2738.0, 234.0, 6),
            (2061.0, 259.0, 6),
            (2061.0, 327.0, 6),
            (2319.0, 327.0, 6),
            (2738.0, 407.0, 4),
            (2061.0, 432.0, 6),
            (2061.0, 500.0, 6),
            (2319.0, 500.0, 6),
            (2738.0, 580.0, 4),
            (2061.0, 605.0, 6),
            (2061.0, 673.0, 6),
            (2319.0, 673.0, 10),
        ];
        let mut specs = Vec::new();
        for (x, y, times) in measured {
            for _ in 0..times {
                specs.push(at(x, y));
            }
        }
        let total: usize = measured.iter().map(|m| m.2).sum();
        assert_eq!(total, 72, "the fixture is the measured distribution");

        let set = candidates(&actions(specs));
        assert_eq!(
            set.groups.len(),
            4,
            "status, customer, product and price -- four fields, whatever the \
             click counts. Got: {:?}",
            set.groups.iter().map(|g| &g.detail).collect::<Vec<_>>()
        );
        for g in &set.groups {
            assert_eq!(
                g.distinct_records, 3,
                "three records, counted by position and not by click"
            );
        }
        assert_eq!(
            set.groups.iter().map(|g| g.occurrences()).sum::<usize>(),
            total,
            "every click is still accounted for as an occurrence"
        );
    }

    /// A page that is not a repeating list has no pitch, and the honest answer
    /// is no candidates rather than an invented grouping.
    #[test]
    fn a_page_with_no_repeating_structure_yields_nothing() {
        let set = candidates(&actions(vec![
            at(100.0, 100.0),
            at(220.0, 137.0),
            at(90.0, 611.0),
            at(400.0, 1290.0),
        ]));
        assert!(set.is_empty());
    }

    /// §3: content never reaches the output, the same invariant `Pattern`
    /// carries. Fed with real captured values on both paths, so the assertion
    /// has something it could actually fail on.
    #[test]
    fn a_candidate_carries_no_captured_value() {
        let set = candidates(&actions(vec![
            // Page side: the value is the element's name.
            (ActionKind::Click, Some("Harbor Point Traders"), Some((176.0, 321.0, 201.0, 22.0))),
            (ActionKind::Click, Some("Ashgrove Manufacturing"), Some((176.0, 535.0, 241.0, 22.0))),
            (ActionKind::Click, Some("Windmere Consulting"), Some((176.0, 749.0, 212.0, 22.0))),
            // Spreadsheet side: the value is the payload, the name is the cell.
            cell("B2"),
            cell("B3"),
            cell("B4"),
        ]));
        assert_eq!(set.groups.len(), 2, "one page field and one column");

        let rendered = format!("{set:?}");
        for value in [
            "Harbor Point Traders",
            "Ashgrove Manufacturing",
            "Windmere Consulting",
        ] {
            assert!(
                !rendered.contains(value),
                "a captured value reached the candidate set: {value:?}"
            );
        }
    }

    /// **THIS TEST FAILS, AND IS COMMITTED FAILING ON PURPOSE.**
    ///
    /// Gap 2 from this module's header, reproduced live on 2026-08-22 rather
    /// than argued: one element -- `MARKER-6` on a tall page -- clicked three
    /// times with two wheel notches between the clicks. What capture recorded:
    ///
    /// ```text
    ///   2 click  MARKER-6  x2024 y930
    ///   3 click  MARKER-6  x2024 y730
    ///   4 click  MARKER-6  x2024 y530
    /// ```
    ///
    /// and this function turns that into `cand-1, 3 record(s)`. One record read
    /// three times is offered to the user as a repeating pattern.
    ///
    /// **The obvious guard does not work, and that was checked before writing
    /// this off.** Three "records" sharing one element NAME looks like a signal
    /// -- but the OrderFlow page carries `Pending` three times, once per real
    /// record, at the same x and the same width. A genuine field whose value
    /// repeats and one element seen at three scroll positions are identical in
    /// every property the action stream holds. There is nothing here to
    /// separate them.
    ///
    /// So the fix is not in this module. It needs the scroll to be OBSERVED,
    /// which `element-bounds-are-viewport-relative-so-scrolling-moves-them.md`
    /// scopes as direction 3a and which tonight's measurements showed is
    /// already reachable -- wheel events carry into the event stream and are
    /// currently discarded. This test is the standing reason to build it.
    ///
    /// It asserts the CORRECT behaviour, so it goes green the day the defect is
    /// fixed and not before.
    #[test]
    fn one_element_seen_at_three_scroll_positions_is_not_three_records() {
        let set = candidates(&actions(vec![
            (ActionKind::Click, Some("MARKER-6"), Some((2024.0, 930.0, 72.0, 20.0))),
            (ActionKind::Click, Some("MARKER-6"), Some((2024.0, 730.0, 72.0, 20.0))),
            (ActionKind::Click, Some("MARKER-6"), Some((2024.0, 530.0, 72.0, 20.0))),
        ]));
        assert!(
            set.is_empty(),
            "one element at three scroll positions is one record, not three -- \
             got {} candidate(s): {:?}",
            set.groups.len(),
            set.groups.iter().map(|g| &g.detail).collect::<Vec<_>>()
        );
    }

    #[test]
    fn candidate_of_maps_an_action_back_to_its_group() {
        let set = candidates(&actions(vec![cell("A2"), cell("A3"), cell("A4")]));
        assert_eq!(set.candidate_of(0), Some("cand-1"));
        assert_eq!(set.candidate_of(2), Some("cand-1"));
        assert_eq!(set.candidate_of(99), None);
    }

    #[test]
    fn the_order_is_deterministic_and_most_repeated_first() {
        let set = candidates(&actions(vec![
            cell("A2"),
            cell("A3"),
            cell("A4"),
            cell("B2"),
            cell("B3"),
            cell("B4"),
            cell("B5"),
        ]));
        assert_eq!(set.groups.len(), 2);
        assert!(set.groups[0].detail.contains("column B"), "4 records first");
        assert_eq!(set.groups[0].id, "cand-1");
        assert!(set.groups[1].detail.contains("column A"));
    }

    /// A click and a type on the same column are different questions.
    #[test]
    fn kind_separates_two_candidates_on_one_column() {
        let mut specs = vec![cell("A2"), cell("A3"), cell("A4")];
        for r in 2..5 {
            specs.push((ActionKind::Click, Some(Box::leak(format!("A{r}").into_boxed_str()) as &str), None));
        }
        let set = candidates(&actions(specs));
        assert_eq!(set.groups.len(), 2);
        assert!(set.groups.iter().any(|g| g.kind == ActionKind::Type));
        assert!(set.groups.iter().any(|g| g.kind == ActionKind::Click));
    }
}
