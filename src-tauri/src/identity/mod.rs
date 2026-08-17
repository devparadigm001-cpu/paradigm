//! Element-identity record tracking, replacing row/column position tracking.
//!
//! Per `docs/planning/Generalizing-Source-Destination-Tracking.md` §1, the Name
//! Box mechanism is a spreadsheet-specific workaround and must not remain the
//! detection mechanism. This module is the replacement's core: the part that
//! decides *which record is this* and *did the source advance*, expressed
//! without any spreadsheet concept.
//!
//! **Nothing here is wired into detection yet, deliberately.** The agreed plan
//! is to land the new mechanism behind its own tests, prove it independently,
//! and cut Sheets over as the final step with a live re-verification as the
//! acceptance gate. Until then `detect::link` and `capture::grid` are untouched
//! and the old path is still the one that runs.
//!
//! ## The algebra problem this solves
//!
//! `detect::Cell` carries `record: i64`, and that integer was doing two
//! unrelated jobs at once:
//!
//! * **Identity** -- which record is this, for the ledger.
//! * **Ordering** -- proof the source advanced, via `steps()` computing
//!   differences between consecutive rows, and the rule for computing where to
//!   go next (`first_row + processed_count * step`).
//!
//! An element identity gives the first for free and destroys the second: an
//! ASIN, a message id and an order number have no ordering and no difference
//! operator. But the second job never actually needed arithmetic.
//!
//! ### "Measurably advanced" becomes distinctness, not difference
//!
//! `detect` rejects `source_step == 0` because, in its own words, a still
//! source "is the difference between 'copy each order in turn' and 'type the
//! same thing three times'". That predicate is not about magnitude. It is about
//! **each repetition drawing from a different record**. So the general form is
//! [`prove_advance`]: the N repetitions must reference N pairwise-distinct
//! record identities.
//!
//! On a spreadsheet this is exactly equivalent -- rows 2, 3, 4 are distinct and
//! rows 2, 2, 2 are not -- so the Sheets case is subsumed rather than
//! special-cased, which is what §1 requires.
//!
//! ### "Uniform step" becomes position in a container sequence
//!
//! Uniformity was proving the traversal was regular so replay knew where to go
//! next. Generalized: a **record is a repeating container**, a **field is a
//! relative path within it**, and **advancing is taking the next container in
//! document order**. `resume_destination_row = first_row + processed * step`
//! becomes "the next container whose identity is not in the ledger"
//! ([`next_unprocessed`]) -- the same thing on a grid, and still meaningful off
//! one.
//!
//! ## Addressing and identity are opposite problems
//!
//! The planning document's §3 evidence -- the same trap in four applications --
//! implies an inversion worth stating plainly, because every naive
//! single-signal approach failed on it:
//!
//! * **Labels are bad identity but good addressing.** `PRODUCT` carries id
//!   `168734` on all seven dashboard cards, and `"Not starred"` matches 100
//!   elements in Gmail. Useless for saying *which* record. But "the value next
//!   to the label reading QUANTITY" is a robust, reorder-proof way to *find* a
//!   field -- which the dashboard needs, since its labels and values are
//!   unassociated sibling elements.
//! * **Values are good identity but bad addressing.** They have distinct ids
//!   and distinct text, but you cannot locate one without already knowing it,
//!   and they collide: `$145.00` is both RS-1003's unit price and RS-1006's
//!   order total.
//!
//! So: **address by label, identify by value.** [`FieldAddress`] is the first
//! half; [`RecordKey`] is the second.

pub mod digest;
pub mod tree;

use std::collections::HashSet;

/// Proof that a human explicitly chose a field as the record identifier.
///
/// Tier 2 was approved on the condition that it is an explicit user choice and
/// **never inferred**. That condition is enforced by the type rather than by a
/// comment: there is exactly one constructor, it is named for what it means,
/// and no code path can produce a digest key without holding one of these.
///
/// The same technique as the run path's `RunAuthorization` -- a decision the
/// user made becomes a value that has to be carried, so it cannot be forgotten
/// or assumed somewhere further down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignatedIdentityField {
    field: String,
}

impl DesignatedIdentityField {
    /// Construct from a real, explicit user selection. The name is deliberately
    /// awkward: it should be uncomfortable to call from anywhere that is not
    /// handling a user's answer.
    pub fn chosen_by_user(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
        }
    }

    pub fn field(&self) -> &str {
        &self.field
    }
}

/// How a record's identity was established. The tier travels with the key.
///
/// Tier 3 -- no stable identity -- is deliberately **not** a variant here. A
/// record with no identity has no key, and representing that as a key would let
/// it flow into the ledger looking like the others. It is
/// [`IdentityResolution::Unavailable`] instead, which callers must handle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecordKey {
    /// Tier 1: the source publishes a non-content identifier of its own.
    /// Amazon's `/dp/<ASIN>`; a spreadsheet row number. Structural and durable,
    /// and clean under §3 because it describes a location, not content.
    Declared { scheme: String, value: String },
    /// Tier 2: a one-way digest of a user-designated identity field. Content
    /// *derived*, never content *retained*.
    Digest { field: String, hex: String },
}

impl RecordKey {
    /// Tier 1, from an identifier the source itself publishes.
    pub fn declared(scheme: impl Into<String>, value: impl Into<String>) -> Self {
        RecordKey::Declared {
            scheme: scheme.into(),
            value: value.into(),
        }
    }

    /// Tier 2. Requires a [`DesignatedIdentityField`], so it cannot be reached
    /// by inference, and a non-empty salt, so it cannot be reached weakly.
    pub fn digest(
        designated: &DesignatedIdentityField,
        value: &str,
        salt: &[u8],
    ) -> Result<Self, digest::DigestError> {
        let bytes = digest::hmac_sha256(salt, value.as_bytes())?;
        Ok(RecordKey::Digest {
            field: designated.field().to_string(),
            hex: digest::to_hex(&bytes),
        })
    }

    /// The scheme this key was declared under, if it is a Tier 1 key.
    pub fn scheme(&self) -> Option<&str> {
        match self {
            RecordKey::Declared { scheme, .. } => Some(scheme),
            RecordKey::Digest { .. } => None,
        }
    }

    /// This key as a number, when it happens to be one.
    ///
    /// Exists for the one thing identities genuinely cannot do: arithmetic. A
    /// spreadsheet destination still needs a numeric step, because a run has to
    /// compute where the next write lands. Sources do not -- their advancement
    /// is [`prove_advance`], which needs no ordering.
    ///
    /// Returns `None` for anything that is not a plain integer, including every
    /// digest, so a caller that needs arithmetic finds out rather than being
    /// handed a fabricated number.
    pub fn numeric(&self) -> Option<i64> {
        match self {
            RecordKey::Declared { value, .. } => value.parse().ok(),
            RecordKey::Digest { .. } => None,
        }
    }

    /// The string stored in `workflow_processed_rows.row_key`.
    ///
    /// The tier prefix is load-bearing, not decoration. Without it a Tier 1
    /// value could collide with a Tier 2 digest, or two schemes could collide
    /// with each other, and a collision in this column means "already
    /// processed" for a record that never was -- a silently skipped record,
    /// which is the failure this ledger exists to prevent.
    ///
    /// The column is already `TEXT NOT NULL` with
    /// `UNIQUE (playbook_id, source_id, row_key)`, so this needs no migration.
    pub fn ledger_key(&self) -> String {
        match self {
            RecordKey::Declared { scheme, value } => format!("d1:{scheme}:{value}"),
            RecordKey::Digest { field, hex } => format!("d2:{field}:{hex}"),
        }
    }
}

/// The outcome of trying to identify a record.
///
/// `Unavailable` is a first-class result because it is a real and common state
/// -- Gmail's message list exposes no identifier at all -- and because the
/// agreed rule is that a source which cannot be identified must stop the run
/// rather than fall back to guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityResolution {
    Resolved(RecordKey),
    Unavailable { reason: String },
}

/// Whether a recording's repetitions genuinely walked different records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvanceProof {
    /// Every repetition drew from a different record.
    Distinct { count: usize },
    /// Two or more repetitions drew from the same record, naming the offender.
    Repeated { key: String, times: usize },
    /// Fewer than two repetitions: nothing to compare, so nothing is proven.
    Inconclusive { count: usize },
}

impl AdvanceProof {
    pub fn advanced(&self) -> bool {
        matches!(self, AdvanceProof::Distinct { .. })
    }
}

/// The general replacement for `source_step != 0`.
///
/// Order-free by construction: it asks only whether the identities differ, not
/// how far apart they are, so it works identically on row numbers, ASINs and
/// digests.
pub fn prove_advance(keys: &[RecordKey]) -> AdvanceProof {
    if keys.len() < 2 {
        return AdvanceProof::Inconclusive { count: keys.len() };
    }
    let mut seen: HashSet<String> = HashSet::new();
    for key in keys {
        let k = key.ledger_key();
        if !seen.insert(k.clone()) {
            let times = keys.iter().filter(|o| o.ledger_key() == k).count();
            return AdvanceProof::Repeated { key: k, times };
        }
    }
    AdvanceProof::Distinct { count: keys.len() }
}

/// The generalized "is this the next new record" signal.
///
/// `sequence` is the source's records **in document order** -- the order the
/// containers appear on the page, which is the only ordering that survives
/// generalization. `processed` holds ledger keys already handled.
///
/// This replaces `resume_destination_row = first_row + processed * step`. It
/// needs no arithmetic, and it is correct when records are inserted or removed
/// between runs, which the arithmetic version silently was not.
pub fn next_unprocessed<'a>(
    sequence: &'a [RecordKey],
    processed: &HashSet<String>,
) -> Option<&'a RecordKey> {
    sequence
        .iter()
        .find(|key| !processed.contains(&key.ledger_key()))
}

/// How to find a field inside a record's container.
///
/// See the module docs: labels are the reliable way to *address*, even though
/// they are useless as identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldAddress {
    /// The content element `offset` positions after the label whose text is
    /// `label`. Offset 1 is the usual case -- a label followed by its value.
    ///
    /// Preferred, because it survives the container being reordered and does
    /// not depend on ids that are shared across every record.
    ByLabel { label: String, offset: i32 },
    /// The nth content element in the container.
    ///
    /// A fallback for containers with no labels at all. Weaker: it breaks the
    /// moment a field is added, removed or reordered, and it breaks silently.
    ByOrdinal { index: usize },
}

impl FieldAddress {
    /// Whether this address depends on nothing but position.
    ///
    /// Surfaced so a caller can warn: an all-ordinal mapping is exactly the
    /// shape that goes wrong quietly when a page changes.
    pub fn is_positional_only(&self) -> bool {
        matches!(self, FieldAddress::ByOrdinal { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn salt() -> &'static [u8] {
        b"playbook-salt"
    }

    #[test]
    fn a_designated_field_is_the_only_route_to_a_digest_key() {
        let designated = DesignatedIdentityField::chosen_by_user("Order Name");
        let key = RecordKey::digest(&designated, "Order RS-1001", salt()).expect("digest");
        match &key {
            RecordKey::Digest { field, hex } => {
                assert_eq!(field, "Order Name");
                assert!(!hex.contains("1001"), "the digest must not carry the value");
            }
            other => panic!("expected a digest key, got {other:?}"),
        }
    }

    #[test]
    fn a_digest_key_refuses_an_empty_salt() {
        let designated = DesignatedIdentityField::chosen_by_user("Order Name");
        assert_eq!(
            RecordKey::digest(&designated, "Order RS-1001", b""),
            Err(digest::DigestError::MissingSalt)
        );
    }

    /// The collision this prefix exists to prevent: without it, a Tier 1 value
    /// and a Tier 2 digest could produce the same ledger string, and a
    /// collision means a record is silently treated as already processed.
    #[test]
    fn tiers_cannot_collide_in_the_ledger_key() {
        let declared = RecordKey::declared("asin", "B00PGB7OKM");
        let designated = DesignatedIdentityField::chosen_by_user("asin");
        let digested = RecordKey::digest(&designated, "B00PGB7OKM", salt()).expect("digest");
        assert_ne!(declared.ledger_key(), digested.ledger_key());
        assert!(declared.ledger_key().starts_with("d1:"));
        assert!(digested.ledger_key().starts_with("d2:"));
    }

    #[test]
    fn two_schemes_with_the_same_value_do_not_collide() {
        let a = RecordKey::declared("asin", "1001");
        let b = RecordKey::declared("row", "1001");
        assert_ne!(a.ledger_key(), b.ledger_key());
    }

    // ---- advancement -----------------------------------------------------

    #[test]
    fn distinct_records_prove_the_source_advanced() {
        let keys = vec![
            RecordKey::declared("row", "2"),
            RecordKey::declared("row", "3"),
            RecordKey::declared("row", "4"),
        ];
        assert_eq!(prove_advance(&keys), AdvanceProof::Distinct { count: 3 });
        assert!(prove_advance(&keys).advanced());
    }

    /// The exact case `source_step == 0` existed to reject -- "type the same
    /// thing three times" -- now expressed without subtraction.
    #[test]
    fn a_still_source_is_rejected_without_any_arithmetic() {
        let keys = vec![
            RecordKey::declared("row", "2"),
            RecordKey::declared("row", "2"),
            RecordKey::declared("row", "2"),
        ];
        match prove_advance(&keys) {
            AdvanceProof::Repeated { key, times } => {
                assert_eq!(key, "d1:row:2");
                assert_eq!(times, 3);
            }
            other => panic!("expected Repeated, got {other:?}"),
        }
        assert!(!prove_advance(&keys).advanced());
    }

    /// Non-uniform gaps used to be `InconsistentAdvance`. They are legitimate
    /// here: a user who skips a record has still advanced. This is a real
    /// behaviour change and it is the point -- uniformity was a property of
    /// grids, not of walking a list.
    #[test]
    fn an_uneven_walk_still_counts_as_advancing() {
        let keys = vec![
            RecordKey::declared("row", "2"),
            RecordKey::declared("row", "5"),
            RecordKey::declared("row", "9"),
        ];
        assert!(prove_advance(&keys).advanced());
    }

    #[test]
    fn identities_with_no_ordering_at_all_still_prove_advance() {
        let keys = vec![
            RecordKey::declared("asin", "B00PGB7OKM"),
            RecordKey::declared("asin", "B0GV25DTDV"),
            RecordKey::declared("asin", "B07CMS5Q6P"),
        ];
        assert!(prove_advance(&keys).advanced());
    }

    #[test]
    fn fewer_than_two_records_proves_nothing() {
        assert_eq!(
            prove_advance(&[]),
            AdvanceProof::Inconclusive { count: 0 }
        );
        assert_eq!(
            prove_advance(&[RecordKey::declared("row", "2")]),
            AdvanceProof::Inconclusive { count: 1 }
        );
    }

    /// Two dashboard orders that share a *value* must not be confused. The
    /// planning doc's `$145.00` collision is the live example.
    #[test]
    fn records_sharing_a_field_value_are_still_distinct_records() {
        let designated = DesignatedIdentityField::chosen_by_user("Order Name");
        let keys = vec![
            RecordKey::digest(&designated, "Order RS-1003", salt()).expect("d"),
            RecordKey::digest(&designated, "Order RS-1006", salt()).expect("d"),
        ];
        assert!(prove_advance(&keys).advanced());
    }

    // ---- next-new-record -------------------------------------------------

    #[test]
    fn the_next_record_is_the_first_one_not_in_the_ledger() {
        let seq = vec![
            RecordKey::declared("row", "2"),
            RecordKey::declared("row", "3"),
            RecordKey::declared("row", "4"),
        ];
        let processed: HashSet<String> =
            ["d1:row:2".to_string(), "d1:row:3".to_string()].into_iter().collect();
        assert_eq!(
            next_unprocessed(&seq, &processed),
            Some(&RecordKey::declared("row", "4"))
        );
    }

    #[test]
    fn nothing_new_returns_none_rather_than_a_position() {
        let seq = vec![RecordKey::declared("row", "2")];
        let processed: HashSet<String> = ["d1:row:2".to_string()].into_iter().collect();
        assert_eq!(next_unprocessed(&seq, &processed), None);
    }

    /// What the arithmetic version got silently wrong. With
    /// `first_row + processed * step`, inserting a record above the cursor
    /// shifts every later row and the run reprocesses or skips. Identity does
    /// not care where a record sits.
    #[test]
    fn a_record_inserted_above_the_cursor_does_not_shift_the_answer() {
        let processed: HashSet<String> =
            ["d1:asin:AAA".to_string(), "d1:asin:BBB".to_string()].into_iter().collect();
        let after_insert = vec![
            RecordKey::declared("asin", "NEW"),
            RecordKey::declared("asin", "AAA"),
            RecordKey::declared("asin", "BBB"),
        ];
        // The newly inserted record is the answer, and the two already-done
        // ones are still recognised despite every position having moved.
        assert_eq!(
            next_unprocessed(&after_insert, &processed),
            Some(&RecordKey::declared("asin", "NEW"))
        );
    }

    /// §1's requirement, as a test: the spreadsheet case must be expressible in
    /// the general vocabulary with no spreadsheet-specific concept involved.
    #[test]
    fn the_sheets_case_is_expressible_without_any_spreadsheet_concept() {
        let seq: Vec<RecordKey> = (2..=10)
            .map(|r| RecordKey::declared("row", r.to_string()))
            .collect();
        assert!(prove_advance(&seq).advanced());
        let processed: HashSet<String> = (2..=6)
            .map(|r| format!("d1:row:{r}"))
            .collect();
        assert_eq!(
            next_unprocessed(&seq, &processed),
            Some(&RecordKey::declared("row", "7"))
        );
    }

    // ---- addressing ------------------------------------------------------

    #[test]
    fn an_ordinal_address_is_flagged_as_positional_only() {
        assert!(FieldAddress::ByOrdinal { index: 3 }.is_positional_only());
        assert!(!FieldAddress::ByLabel {
            label: "QUANTITY".into(),
            offset: 1
        }
        .is_positional_only());
    }
}
