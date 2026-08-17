//! Turning a flat accessibility tree into records, generally.
//!
//! Per §1 and §5.2 of `docs/planning/Generalizing-Source-Destination-Tracking.md`:
//! a general element-identity mechanism needs a rule for **which elements carry
//! identity** and **which elements belong to the same record**, expressed
//! without reference to any particular application.
//!
//! Nothing here names an application, a role, a label or an id. The only inputs
//! are a document-order list of nodes and their multiplicity. The fixtures in
//! the tests are real captures from four separate investigations, used as
//! evidence that the rule generalizes -- not as things the code matches against.
//!
//! ## The rule for structural vs content, and the evidence for it
//!
//! Four investigations found the same trap in four shapes: an element looks
//! reliable and is not. The common thread, measured each time, is that
//! **structural elements share one id across every record, while content
//! elements have an id of their own**:
//!
//! | tree | shared id | what it was |
//! |---|---|---|
//! | Gmail | `601850` | every row's star cell |
//! | Amazon | `118404` | every listing's price label |
//! | dashboard | `168734` | every card's product label |
//!
//! So the rule is multiplicity, and nothing else:
//!
//! > An id occurring more than once in the tree is **structural**. An id
//! > occurring exactly once is **content**.
//!
//! Names cannot do this job. A sender name legitimately repeats across Gmail
//! rows, and a price legitimately repeats across listings -- the planning
//! document records `$145.00` appearing twice with two different meanings. Ids
//! separate the two cases and names do not.
//!
//! ## Grouping into records without needing a container
//!
//! The obvious approach is to find a per-record container element and read its
//! children. It does not survive contact with real trees:
//!
//! * Amazon's listings resolve at different roles *and* different depths -- some
//!   one hop above a price, one four hops above, and the container is sometimes
//!   an unnamed `Group` and sometimes a named `ListItem`.
//! * The dashboard has **no per-record container at all** along one traversal:
//!   walking down from the document yields 77 sibling nodes flat, while walking
//!   up from a node reports intermediate groups. Both were measured in the same
//!   run, and one of them is provably self-contradictory (a parent reporting
//!   zero children while being a parent).
//!
//! A rule that depends on containers therefore cannot be general, because the
//! containers are not reliably there. So this does not use them.
//!
//! Instead: **structural elements repeat once per record, so their k-th
//! occurrence belongs to record k.** That holds whether the tree is nested or
//! flat, because it depends only on document order and multiplicity. A record is
//! reconstructed rather than located.
//!
//! Content is attached by adjacency -- the content node following a structural
//! one is its value. This is the "address by label, identify by value"
//! inversion from [`super`]: labels are useless as identity and reliable as
//! addresses, values are the reverse.
//!
//! Content not claimed by any label is kept separately as
//! [`RecordView::unlabelled`]. Those are the natural Tier 2 identity candidates
//! -- a record number sitting on its own with no label beside it -- and the k-th
//! such value belongs to record k for the same reason.
//!
//! ## Where this does NOT apply, stated plainly
//!
//! **A canvas grid has no elements to classify.** Google Sheets' entire window
//! is ~56 nodes and its document subtree is two: `Document > Pane`. There are no
//! per-cell elements, no rows, and nothing whose id could be counted. This
//! module cannot read a spreadsheet grid, and no refinement of it will, because
//! the input does not exist.
//!
//! That is a real limit on "one general mechanism", and it is not routed around
//! here. The spreadsheet case is served by reading the CSV export, which
//! produces records and fields directly. See `super`'s docs and §8 of the
//! planning document.

use std::collections::BTreeMap;

/// One node of an accessibility tree, flattened into document order.
///
/// Deliberately not tied to the automation library: the rules below are pure
/// functions over this, so they are testable against captured fixtures from any
/// application without a browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    /// The platform's element id. Whatever the platform calls it, all this
    /// needs is that equal ids mean the same underlying element.
    pub id: String,
    pub role: String,
    pub name: String,
}

impl TreeNode {
    pub fn new(id: impl Into<String>, role: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: role.into(),
            name: name.into(),
        }
    }
}

/// What an element is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Repeats across records. Good for addressing, useless as identity.
    Structural,
    /// Unique to one record. Good as identity, useless for addressing.
    Content,
}

/// Classify every id in the tree by multiplicity.
///
/// Nodes with an empty name are ignored: they carry nothing to address by and
/// nothing to identify with, and including them would inflate the structural
/// set with spacers and decorations.
pub fn classify(nodes: &[TreeNode]) -> BTreeMap<String, Kind> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for n in nodes.iter().filter(|n| !n.name.trim().is_empty()) {
        *counts.entry(n.id.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(id, count)| {
            (
                id.to_string(),
                if count > 1 {
                    Kind::Structural
                } else {
                    Kind::Content
                },
            )
        })
        .collect()
}

/// One field of one record: the label that addressed it, and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub value: String,
}

/// One reconstructed record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordView {
    pub fields: Vec<Field>,
    /// Content with no label beside it. Tier 2 identity candidates.
    pub unlabelled: Vec<String>,
}

impl RecordView {
    /// The value addressed by a given label, if this record has one.
    pub fn value_of(&self, label: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.value.as_str())
    }
}

/// Reconstruct records from a document-order tree.
///
/// No container detection, no role assumptions, no per-application knowledge --
/// see the module docs for why containers cannot be relied on.
pub fn records(nodes: &[TreeNode]) -> Vec<RecordView> {
    let kinds = classify(nodes);
    let kind_of = |n: &TreeNode| kinds.get(&n.id).copied();

    // How many times each structural id has been seen so far. The k-th
    // occurrence of a structural element belongs to record k.
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut unlabelled_seen = 0usize;
    let mut out: Vec<RecordView> = Vec::new();

    let ensure = |out: &mut Vec<RecordView>, k: usize| {
        while out.len() <= k {
            out.push(RecordView::default());
        }
    };

    // The most recent structural node, waiting for the content that follows it.
    let mut pending: Option<(String, usize)> = None;

    for node in nodes.iter().filter(|n| !n.name.trim().is_empty()) {
        match kind_of(node) {
            Some(Kind::Structural) => {
                let k = seen.entry(node.id.clone()).or_insert(0);
                let index = *k;
                *k += 1;
                // A structural node immediately following another one means the
                // first addressed nothing. Dropping it is correct: a label with
                // no value is not a field, and inventing an empty one would put
                // a fabricated field into a record.
                pending = Some((node.name.clone(), index));
            }
            Some(Kind::Content) => match pending.take() {
                Some((label, index)) => {
                    ensure(&mut out, index);
                    out[index].fields.push(Field {
                        label,
                        value: node.name.clone(),
                    });
                }
                None => {
                    let index = unlabelled_seen;
                    unlabelled_seen += 1;
                    ensure(&mut out, index);
                    out[index].unlabelled.push(node.name.clone());
                }
            },
            None => {}
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str, role: &str, name: &str) -> TreeNode {
        TreeNode::new(id, role, name)
    }

    // ---------------------------------------------------------------------
    // Fixtures below are REAL captures, ids included, from the four
    // investigations. They are evidence that one rule handles all of them;
    // nothing in the code above refers to any of these strings.
    // ---------------------------------------------------------------------

    /// Real capture: `text_capture_probe pagetree` against a local card-based
    /// order dashboard. The tree is FLAT -- 77 sibling nodes, no per-record
    /// container -- which is the case container detection cannot handle.
    fn flat_card_tree() -> Vec<TreeNode> {
        vec![
            n("140747", "Text", "Order RS-1001"),
            n("121421", "Text", "Pending"),
            n("956010", "Text", "Harbor Point Traders"),
            n("168734", "Text", "PRODUCT"),
            n("136415", "Text", "Ceramic Mug Set"),
            n("967325", "Text", "QUANTITY"),
            n("287472", "Text", "12"),
            n("176925", "Text", "UNIT PRICE"),
            n("149595", "Text", "$8.50"),
            n("891429", "Text", "ORDER TOTAL"),
            n("548019", "Text", "$102.00"),
            n("456893", "Text", "Order RS-1002"),
            n("121421", "Text", "Pending"),
            n("565789", "Text", "Ashgrove Manufacturing"),
            n("168734", "Text", "PRODUCT"),
            n("825412", "Text", "Steel Bracket (box of 50)"),
            n("967325", "Text", "QUANTITY"),
            n("151760", "Text", "40"),
            n("176925", "Text", "UNIT PRICE"),
            n("177269", "Text", "$3.25"),
            n("891429", "Text", "ORDER TOTAL"),
            n("203888", "Text", "$130.00"),
            n("146803", "Text", "Order RS-1003"),
            n("121421", "Text", "Pending"),
            n("663973", "Text", "Windmere Consulting"),
            n("168734", "Text", "PRODUCT"),
            n("170976", "Text", "Ergonomic Office Chair"),
            n("967325", "Text", "QUANTITY"),
            n("127685", "Text", "2"),
        ]
    }

    #[test]
    fn multiplicity_separates_labels_from_values() {
        let kinds = classify(&flat_card_tree());
        // Repeated ids -- the label elements.
        assert_eq!(kinds.get("168734"), Some(&Kind::Structural));
        assert_eq!(kinds.get("967325"), Some(&Kind::Structural));
        assert_eq!(kinds.get("121421"), Some(&Kind::Structural));
        // Unique ids -- the values, including two that share a name with
        // nothing and one identity-shaped value.
        assert_eq!(kinds.get("136415"), Some(&Kind::Content));
        assert_eq!(kinds.get("140747"), Some(&Kind::Content));
    }

    #[test]
    fn a_flat_tree_with_no_containers_still_yields_records() {
        let recs = records(&flat_card_tree());
        assert_eq!(recs.len(), 3, "three records were present");

        assert_eq!(recs[0].value_of("PRODUCT"), Some("Ceramic Mug Set"));
        assert_eq!(recs[0].value_of("QUANTITY"), Some("12"));
        assert_eq!(recs[0].value_of("UNIT PRICE"), Some("$8.50"));
        assert_eq!(recs[0].value_of("ORDER TOTAL"), Some("$102.00"));

        assert_eq!(recs[1].value_of("PRODUCT"), Some("Steel Bracket (box of 50)"));
        assert_eq!(recs[1].value_of("QUANTITY"), Some("40"));
        assert_eq!(recs[2].value_of("PRODUCT"), Some("Ergonomic Office Chair"));
    }

    /// The identity-shaped value has no label beside it, so it lands in
    /// `unlabelled` -- which is what makes it a Tier 2 candidate the user can
    /// designate. Nothing here knows what an order number looks like.
    #[test]
    fn content_with_no_label_is_kept_as_an_identity_candidate() {
        let recs = records(&flat_card_tree());
        assert_eq!(recs[0].unlabelled, vec!["Order RS-1001"]);
        assert_eq!(recs[1].unlabelled, vec!["Order RS-1002"]);
        assert_eq!(recs[2].unlabelled, vec!["Order RS-1003"]);
    }

    /// Real capture: `gmailopened`, two message rows from different views. The
    /// shared ids are the star cell, a spacer and an image -- present on every
    /// row -- while sender and date are unique per row.
    fn message_list_tree() -> Vec<TreeNode> {
        vec![
            n("601850", "DataItem", "Not starred"),
            n("954379", "DataItem", "Edikted"),
            n("900112", "DataItem", "Sun, Aug 16, 2026, 10:30 AM"),
            n("601850", "DataItem", "Not starred"),
            n("736162", "DataItem", "AutoForward"),
            n("386997", "DataItem", "Sat, Aug 15, 2026, 6:32 AM"),
        ]
    }

    /// The same rule, unchanged, on a completely different application's tree.
    #[test]
    fn the_same_rule_groups_a_message_list(
    ) {
        let kinds = classify(&message_list_tree());
        assert_eq!(kinds.get("601850"), Some(&Kind::Structural));
        assert_eq!(kinds.get("954379"), Some(&Kind::Content));

        let recs = records(&message_list_tree());
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].value_of("Not starred"), Some("Edikted"));
        assert_eq!(recs[1].value_of("Not starred"), Some("AutoForward"));
        // The date follows a content node, so it is unlabelled rather than
        // mis-attached to the star -- see the honest limit in the report.
        assert_eq!(
            recs[0].unlabelled,
            vec!["Sun, Aug 16, 2026, 10:30 AM"]
        );
    }

    /// Real capture: `amzntree`. The price LABEL shares one id across listings
    /// while each price value has its own -- the same shape as the other two
    /// trees, from a third application.
    fn listing_tree() -> Vec<TreeNode> {
        vec![
            n("118404", "Text", "Price, product page"),
            n("167114", "Hyperlink", "$13.79 List: $17.99"),
            n("118404", "Text", "Price, product page"),
            n("112881", "Hyperlink", "$15.99"),
        ]
    }

    #[test]
    fn the_same_rule_groups_a_listing_page() {
        let kinds = classify(&listing_tree());
        assert_eq!(kinds.get("118404"), Some(&Kind::Structural));
        assert_eq!(kinds.get("167114"), Some(&Kind::Content));

        let recs = records(&listing_tree());
        assert_eq!(recs.len(), 2);
        assert_eq!(
            recs[0].value_of("Price, product page"),
            Some("$13.79 List: $17.99")
        );
        assert_eq!(recs[1].value_of("Price, product page"), Some("$15.99"));
    }

    /// Two records whose VALUES coincide must still be two records. The planning
    /// document's real example: one listing's unit price equals another's total.
    #[test]
    fn records_are_not_merged_when_two_values_coincide() {
        let tree = vec![
            n("100", "Text", "PRICE"),
            n("201", "Text", "$145.00"),
            n("100", "Text", "PRICE"),
            n("202", "Text", "$145.00"),
        ];
        let recs = records(&tree);
        assert_eq!(recs.len(), 2, "identical values, different elements");
    }

    /// A tree with no repetition has no structural elements, so it yields no
    /// labelled fields. Reported as such rather than guessed at.
    #[test]
    fn a_tree_with_no_repetition_yields_no_labelled_fields() {
        let tree = vec![
            n("1", "Text", "alpha"),
            n("2", "Text", "bravo"),
            n("3", "Text", "charlie"),
        ];
        let recs = records(&tree);
        assert!(recs.iter().all(|r| r.fields.is_empty()));
        assert_eq!(recs.len(), 3, "each unlabelled value is its own candidate");
    }

    /// The canvas-grid case, as a test rather than a caveat in prose. A
    /// spreadsheet's document subtree is two nodes and neither is a cell, so
    /// there is nothing to classify and nothing to group.
    #[test]
    fn a_canvas_grid_offers_nothing_to_work_with() {
        let tree = vec![
            n("100", "Document", "Untitled spreadsheet - Google Sheets"),
            n("101", "Pane", ""),
        ];
        assert!(records(&tree).iter().all(|r| r.fields.is_empty()));
        // Not a bug to be fixed here: the spreadsheet path reads the CSV export
        // instead, which produces records directly.
    }

    #[test]
    fn empty_names_are_ignored_rather_than_counted_as_structure() {
        let tree = vec![
            n("900", "Pane", ""),
            n("900", "Pane", ""),
            n("100", "Text", "LABEL"),
            n("201", "Text", "value one"),
            n("100", "Text", "LABEL"),
            n("202", "Text", "value two"),
        ];
        let kinds = classify(&tree);
        assert!(!kinds.contains_key("900"), "unnamed nodes carry nothing");
        assert_eq!(records(&tree).len(), 2);
    }
}
