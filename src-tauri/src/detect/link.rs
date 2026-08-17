//! Turning what capture saw into what detection reasons about.
//!
//! [`SourceLink`] is a capture fact: at sequence N, a value was copied from
//! *this* cell of *this* document and pasted into *that* cell of *that* one.
//! [`Observation`] is a detection fact, in the vocabulary §4.1 uses: a write to
//! a destination `Cell`, optionally traceable to a source `Cell`. The two are
//! deliberately not the same type -- detection is a pure function over
//! positions and must not depend on how a position was witnessed -- so
//! something has to translate, and this is it.
//!
//! ## Why the sheet folds into the surface
//!
//! A cell reference may arrive qualified (`Sheet2!B7`) because capture tracks
//! which sheet a click selected. Detection's `surface` is "the thing being
//! written to", and two tabs of one spreadsheet are two different things to
//! write to -- a pattern that walks down Sheet1 is not the same pattern as one
//! that walks down Sheet2. So the sheet qualifier is folded into the surface
//! rather than discarded, and the `Cell` keeps only the bare column and row.

use crate::capture::grid::{split_sheet_ref, SourceLink};
use crate::detect::{Cell, Observation, SourceRef};
use crate::source::spreadsheet::parse_cell_ref;

/// Split a document plus a possibly-qualified cell into a surface and a cell.
///
/// `None` when the reference is not a cell at all. A link whose position could
/// not be parsed is dropped rather than guessed at: §4.1 already treats an
/// untraceable write as something to exclude, and inventing a position would
/// put a fabricated example into a Rule-of-3 count.
fn surface_and_cell(document: &str, reference: &str) -> Option<(String, Cell)> {
    let (sheet, _) = split_sheet_ref(reference.trim());
    let (field, record) = parse_cell_ref(reference)?;
    let surface = match sheet {
        Some(s) => format!("{document}!{s}"),
        None => document.to_string(),
    };
    // `at_row` stores the row under the `"row"` scheme. Exactly the same number,
    // in the general representation -- the spreadsheet case is unchanged.
    Some((surface, Cell::at_row(field, record)))
}

/// Translate capture's links into detection's observations.
///
/// Order is preserved and `seq` carries through, because detection's correction
/// rule -- the last write to a destination wins -- is defined in terms of it.
pub fn observations(links: &[SourceLink]) -> Vec<Observation> {
    links
        .iter()
        .filter_map(|link| {
            let (destination_surface, destination) =
                surface_and_cell(&link.destination_document, &link.destination_cell)?;
            let (source_surface, source_cell) =
                surface_and_cell(&link.source_document, &link.source_cell)?;
            Some(Observation {
                seq: link.seq as usize,
                surface: destination_surface,
                destination,
                source: Some(SourceRef {
                    surface: source_surface,
                    cell: source_cell,
                }),
            })
        })
        .collect()
}

/// The source and destination surfaces a recording was mostly about.
///
/// Detection needs to be told which two surfaces to reason over, and a real
/// recording may touch others incidentally -- a glance at a third sheet, a
/// stray paste into a scratch document. Taking the most frequent pair rather
/// than the first means one stray link cannot redirect the whole detection,
/// while a recording that genuinely worked between two surfaces is unaffected.
///
/// `None` for a recording with no usable links at all, which is every ordinary
/// recording: nothing was copied between grids, so there is nothing to detect.
pub fn dominant_surfaces(links: &[SourceLink]) -> Option<(String, String)> {
    let mut counts: std::collections::BTreeMap<(String, String), usize> = Default::default();
    for link in links {
        let Some((destination, _)) =
            surface_and_cell(&link.destination_document, &link.destination_cell)
        else {
            continue;
        };
        let Some((source, _)) = surface_and_cell(&link.source_document, &link.source_cell) else {
            continue;
        };
        *counts.entry((source, destination)).or_default() += 1;
    }
    // Ties break on the pair itself, via the BTreeMap's order, so the answer is
    // deterministic rather than dependent on hash iteration order.
    counts
        .into_iter()
        .max_by_key(|(pair, n)| (*n, std::cmp::Reverse(pair.clone())))
        .map(|(pair, _)| pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(seq: u64, src_doc: &str, src: &str, dst_doc: &str, dst: &str) -> SourceLink {
        SourceLink {
            seq,
            source_document: src_doc.into(),
            source_cell: src.into(),
            destination_document: dst_doc.into(),
            destination_cell: dst.into(),
        }
    }

    #[test]
    fn a_link_becomes_an_observation_with_both_positions() {
        let obs = observations(&[link(1, "Orders", "C5", "Invoices", "B2")]);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].seq, 1);
        assert_eq!(obs[0].surface, "Invoices");
        assert_eq!(
            obs[0].destination,
            Cell::at_row("B", 2)
        );
        let source = obs[0].source.as_ref().expect("source");
        assert_eq!(source.surface, "Orders");
        assert_eq!(
            source.cell,
            Cell::at_row("C", 5)
        );
    }

    #[test]
    fn a_qualified_cell_puts_the_sheet_in_the_surface_not_the_cell() {
        // Two tabs of one document are two surfaces: a pattern down Sheet1 is
        // not the same pattern as one down Sheet2.
        let obs = observations(&[link(1, "Book", "Sheet2!C5", "Book", "Sheet1!B2")]);
        assert_eq!(obs[0].surface, "Book!Sheet1");
        assert_eq!(obs[0].destination.field, "B");
        assert_eq!(obs[0].destination.record.numeric(), Some(2));
        assert_eq!(obs[0].source.as_ref().unwrap().surface, "Book!Sheet2");
        assert_eq!(obs[0].source.as_ref().unwrap().cell.field, "C");
    }

    #[test]
    fn an_unparseable_position_is_dropped_rather_than_guessed_at() {
        // A fabricated position would count toward the Rule of 3.
        let obs = observations(&[
            link(1, "Orders", "C5", "Invoices", "B2"),
            link(2, "Orders", "not-a-cell", "Invoices", "B3"),
        ]);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].seq, 1);
    }

    #[test]
    fn the_dominant_pair_wins_over_a_stray_link() {
        let links = vec![
            link(1, "Orders", "C2", "Invoices", "B2"),
            link(2, "Orders", "C3", "Invoices", "B3"),
            link(3, "Orders", "C4", "Invoices", "B4"),
            link(4, "Scratch", "A1", "Notes", "A1"),
        ];
        assert_eq!(
            dominant_surfaces(&links),
            Some(("Orders".into(), "Invoices".into()))
        );
    }

    #[test]
    fn no_links_means_no_surfaces() {
        assert_eq!(dominant_surfaces(&[]), None);
        assert!(observations(&[]).is_empty());
    }
}
