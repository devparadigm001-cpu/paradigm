//! The spreadsheet [`SourceReader`] -- the first, and currently only,
//! implementation of the source interface.
//!
//! Everything spreadsheet-specific lives here. Detection, mapping and replay
//! talk to [`SourceReader`], never to this type, which is the whole point of
//! Section 3's interface: a second source type is a new file next to this one,
//! not a rewrite of the callers.
//!
//! ## How a cell is read
//!
//! Navigate the Name Box to the cell, then read the **formula bar**. Both halves
//! are measured, not assumed:
//!
//! * Name Box navigation is the mechanism `replay::grid_type` already uses, and
//!   a sheet-qualified reference (`Sheet2!B2`) switches sheet and navigates in
//!   one action -- CSV-confirmed.
//! * The formula bar reports the value of whatever cell is selected, without
//!   entering edit mode -- `text_capture_probe -- sheetsread`, cross-checked
//!   against the CSV export so a write that never landed could not be mistaken
//!   for an unreadable one.
//!
//! ## Why finding the formula bar is geometric
//!
//! It has no name and six anonymous `Group` ancestors, while the window holds
//! eight `Edit`s -- one of which is the Name Box, reporting the cell REFERENCE.
//! Picking wrong would read `"B2"` where a customer name was meant and look like
//! it was working.
//!
//! What IS stable is where it sits: the same row as the Name Box, immediately to
//! its right. The Name Box is reliably locatable by name, so the formula bar is
//! located relative to it. [`pick_formula_bar`] is that rule, kept pure so it is
//! tested against the real measured bounds rather than against a live window.
//!
//! `terminator`'s `Selector::RightOf` may express the same thing natively and is
//! worth measuring as a simplification later -- deliberately not depended on
//! now, since its semantics are unverified and this reader's foundation should
//! rest on something that was measured.

use terminator::{Desktop, UIElement};

use super::{
    classify_row, Advance, FieldRef, SourceError, SourcePosition, SourceReader, SourceRecord,
    SourceShape,
};
use crate::capture::grid::clean_cell_text;

/// A rectangle in screen coordinates, as `UIElement::bounds` reports one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn right(&self) -> f64 {
        self.x + self.w
    }
    fn top(&self) -> f64 {
        self.y
    }
    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// Anything narrower or shorter than this is not a real text field.
///
/// Two of the eight `Edit`s measured in a live window are like this: one at
/// `(0,0,1,1)` and one parked off-screen at `(-7994,-9910)`. They are excluded
/// on size rather than on position, because "off-screen" is a moving target and
/// "one pixel wide" is not a field under any layout.
const MIN_FIELD_W: f64 = 20.0;
const MIN_FIELD_H: f64 = 8.0;

/// Choose the formula bar from the window's `Edit` elements, given where the
/// Name Box is.
///
/// The rule, in order:
///
/// 1. discard degenerate rectangles -- see [`MIN_FIELD_W`];
/// 2. keep those sharing a row with the Name Box, by **vertical overlap**
///    rather than by comparing tops or centres, so a taller formula bar still
///    matches a shorter Name Box;
/// 3. keep those beginning at or after the Name Box's right edge;
/// 4. take the nearest.
///
/// Returns the winner's index in `candidates`, or `None` when nothing qualifies
/// -- which is a real outcome, not a fallback to guessing. A reader that cannot
/// identify the formula bar must fail loudly rather than read some other `Edit`.
pub fn pick_formula_bar(name_box: Rect, candidates: &[(usize, Rect)]) -> Option<usize> {
    let row_overlap = |c: &Rect| -> f64 {
        let top = name_box.top().max(c.top());
        let bottom = name_box.bottom().min(c.bottom());
        (bottom - top).max(0.0)
    };

    candidates
        .iter()
        .filter(|(_, c)| c.w >= MIN_FIELD_W && c.h >= MIN_FIELD_H)
        // Sharing a row means overlapping vertically by more than half the
        // shorter of the two -- a brush of a pixel or two is not a row.
        .filter(|(_, c)| row_overlap(c) > name_box.h.min(c.h) / 2.0)
        .filter(|(_, c)| c.x >= name_box.right())
        .min_by(|(_, a), (_, b)| {
            a.x.partial_cmp(&b.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| *i)
}

/// [`pick_formula_bar`], as the reader uses it: a failure to identify is an
/// error, never a fallback.
///
/// Split out from `open` so the fail-loud behaviour is *tested* rather than
/// merely asserted in a comment. There is no second-choice element and no
/// "closest thing" -- the alternatives to the formula bar in a real window
/// include the Name Box, which reports a plausible-looking cell reference, so a
/// reader that degraded gracefully here would return `"B2"` where a customer
/// name was meant and look like it was working.
pub fn require_formula_bar(
    name_box: Rect,
    candidates: &[(usize, Rect)],
) -> Result<usize, SourceError> {
    pick_formula_bar(name_box, candidates).ok_or_else(|| {
        SourceError::Unreachable(format!(
            "no formula bar found on the Name Box's row at {name_box:?} and to its right, \
             among {} candidate field(s). Refusing to read from another element: the Name Box \
             itself reports a cell reference, which would pass for a value.",
            candidates.len()
        ))
    })
}

/// A spreadsheet cell reference, sheet-qualified when the sheet is known.
///
/// Qualifying is what makes a read land on the right sheet -- the same
/// mechanism, and the same reasoning, as `replay::grid_type`'s writes.
pub fn cell_ref(sheet: Option<&str>, column: &str, row: u64) -> String {
    match sheet {
        Some(s) if !s.trim().is_empty() => format!("{}!{}{}", s.trim(), column, row),
        _ => format!("{column}{row}"),
    }
}

/// How many rows past a blank one to look before calling the source exhausted.
///
/// 4.10 wants "clearly more non-blank data further down" distinguished from the
/// end of the data, which needs *some* window. This is only ever paid once per
/// run: [`SpreadsheetReader::peek`] scans ahead only when the current row is
/// already blank, so the common case costs nothing. Each probed row is a real
/// navigation, so the number is a cost, not a free parameter.
const LOOKAHEAD_ROWS: u64 = 5;

/// Reads records out of a spreadsheet, one row at a time.
pub struct SpreadsheetReader {
    // No `Desktop` is kept. Once the Name Box and formula bar are resolved,
    // every operation goes through those two elements directly, and holding a
    // handle that nothing reads would be the sort of purposeful-looking dead
    // weight this project deletes elsewhere. A future operation that needs to
    // re-resolve the window can take one as an argument.
    name_box: UIElement,
    formula_bar: UIElement,
    source_id: String,
    sheet: Option<String>,
    row: u64,
    header_row: u64,
    /// Columns `shape()` inspects when looking for drift. Bounded because every
    /// column costs a navigation; see [`SpreadsheetReader::open`].
    scan_columns: Vec<String>,
}

impl SpreadsheetReader {
    /// Resolve the two elements this reader needs, and settle on a first row.
    ///
    /// The only async part. Everything after this is synchronous, which is what
    /// lets [`SourceReader`] stay object-safe -- see the module docs on
    /// `super`.
    pub async fn open(
        desktop: Desktop,
        window: &UIElement,
        source_id: impl Into<String>,
        sheet: Option<String>,
        first_row: u64,
        header_row: u64,
        scan_columns: Vec<String>,
    ) -> Result<Self, SourceError> {
        let name_box = desktop
            .locator("name:Name box")
            .within(window.clone())
            .all(Some(std::time::Duration::from_secs(5)), None)
            .await
            .map_err(|e| SourceError::Unreachable(format!("Name Box lookup failed: {e}")))?
            .into_iter()
            .next()
            .and_then(|g| {
                g.children()
                    .ok()
                    .and_then(|c| c.into_iter().find(|e| e.role() == "Edit"))
            })
            .ok_or_else(|| {
                SourceError::Unreachable(
                    "no Name Box: a spreadsheet is reached through it, and without one there \
                     is no way to address a cell"
                        .to_string(),
                )
            })?;

        let name_box_rect = bounds_of(&name_box)?;

        let edits = desktop
            .locator("role:Edit")
            .within(window.clone())
            .all(Some(std::time::Duration::from_secs(6)), None)
            .await
            .map_err(|e| SourceError::Unreachable(format!("Edit enumeration failed: {e}")))?;

        let rects: Vec<(usize, Rect)> = edits
            .iter()
            .enumerate()
            .filter_map(|(i, el)| bounds_of(el).ok().map(|r| (i, r)))
            .collect();

        // Fails HERE, at construction, rather than on some later read. A reader
        // that exists is a reader that knows where the value comes from; there
        // is no half-built state in which reads quietly return the wrong thing.
        let chosen = require_formula_bar(name_box_rect, &rects)?;

        Ok(Self {
            name_box,
            formula_bar: edits[chosen].clone(),
            source_id: source_id.into(),
            sheet,
            row: first_row,
            header_row,
            scan_columns,
        })
    }

    /// Move the selection to a cell, and confirm it arrived.
    ///
    /// Verified rather than timed. `grid_type` established that the Name Box
    /// echoes back the bare cell after a qualified navigation, so the check
    /// compares against the cell part -- a comparison against the full
    /// reference would reject every cross-sheet read at the moment it worked.
    fn goto(&self, column: &str, row: u64) -> Result<(), SourceError> {
        let reference = cell_ref(self.sheet.as_deref(), column, row);
        let bare = format!("{column}{row}");

        self.name_box.set_value(&reference).map_err(|e| {
            SourceError::Unreadable {
                locator: column.to_string(),
                row: row.to_string(),
                reason: format!("could not address the Name Box: {e}"),
            }
        })?;
        self.name_box
            .press_key("{Enter}")
            .map_err(|e| SourceError::Unreadable {
                locator: column.to_string(),
                row: row.to_string(),
                reason: format!("could not submit the Name Box: {e}"),
            })?;
        std::thread::sleep(std::time::Duration::from_millis(900));

        let landed = self.name_box.text(0).unwrap_or_default();
        if landed.trim() != bare {
            return Err(SourceError::PositionLost(format!(
                "asked for {reference:?} but the Name Box reads {landed:?}; refusing to read a \
                 cell that is not demonstrably the one requested"
            )));
        }
        Ok(())
    }

    /// The value of one cell, cleaned.
    fn read_cell(&self, column: &str, row: u64) -> Result<String, SourceError> {
        self.goto(column, row)?;
        let raw = self
            .formula_bar
            .text(0)
            .map_err(|e| SourceError::Unreadable {
                locator: column.to_string(),
                row: row.to_string(),
                reason: format!("formula bar unreadable: {e}"),
            })?;
        // The formula bar returns a trailing newline -- measured, "value\n".
        // `clean_cell_text` already strips that and the U+FEFF Sheets seeds its
        // editor with, so there is one cleaning rule rather than two.
        Ok(clean_cell_text(&raw))
    }

    fn read_row(&self, fields: &[FieldRef], row: u64) -> Result<Vec<String>, SourceError> {
        fields
            .iter()
            .map(|f| self.read_cell(&f.locator, row))
            .collect()
    }
}

fn bounds_of(el: &UIElement) -> Result<Rect, SourceError> {
    el.bounds()
        .map(|(x, y, w, h)| Rect { x, y, w, h })
        .map_err(|e| SourceError::Unreachable(format!("element has no usable bounds: {e}")))
}

impl SourceReader for SpreadsheetReader {
    fn position(&self) -> SourcePosition {
        SourcePosition {
            source_id: self.source_id.clone(),
            row_key: self.row.to_string(),
        }
    }

    fn peek(&mut self, fields: &[FieldRef]) -> Result<Advance, SourceError> {
        let current = self.read_row(fields, self.row)?;

        // Fast path, and the common one: a row with data needs no lookahead.
        // Only a blank row raises 4.10's question, and that happens once per
        // run -- which is what keeps the scan below from costing anything in
        // the ordinary case.
        if current.iter().any(|v| !v.trim().is_empty()) {
            return Ok(Advance::Record);
        }

        let mut ahead = Vec::new();
        for offset in 1..=LOOKAHEAD_ROWS {
            ahead.push(self.read_row(fields, self.row + offset)?);
        }
        Ok(classify_row(&current, &ahead))
    }

    fn read(&mut self, fields: &[FieldRef]) -> Result<SourceRecord, SourceError> {
        let values = self.read_row(fields, self.row)?;
        Ok(SourceRecord {
            position: self.position(),
            fields: fields
                .iter()
                .map(|f| f.name.clone())
                .zip(values)
                .collect(),
        })
    }

    fn advance(&mut self) -> Result<(), SourceError> {
        self.row += 1;
        Ok(())
    }

    fn shape(&mut self) -> Result<SourceShape, SourceError> {
        let mut columns = Vec::new();
        for column in &self.scan_columns {
            let label = self.read_cell(column, self.header_row)?;
            if !label.trim().is_empty() {
                columns.push(super::ColumnShape {
                    locator: column.clone(),
                    label: label.trim().to_string(),
                });
            }
        }
        Ok(SourceShape { columns })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Name Box, exactly as measured in a live window.
    fn name_box() -> Rect {
        Rect {
            x: 2014.0,
            y: 207.0,
            w: 75.0,
            h: 20.0,
        }
    }

    /// Every `Edit` from the same measured window, in the order the enumeration
    /// returned them. Index 5 is the formula bar; index 4 is the Name Box.
    fn measured_edits() -> Vec<(usize, Rect)> {
        vec![
            // [0] address and search bar
            (0, Rect { x: 2035.0, y: 58.0, w: 612.0, h: 25.0 }),
            // [1] document title
            (1, Rect { x: 2060.0, y: 100.0, w: 188.0, h: 25.0 }),
            // [2] zoom
            (2, Rect { x: 2199.0, y: 162.0, w: 49.0, h: 29.0 }),
            // [3] font size
            (3, Rect { x: 2583.0, y: 163.0, w: 33.0, h: 25.0 }),
            // [4] the Name Box itself
            (4, Rect { x: 2014.0, y: 207.0, w: 75.0, h: 20.0 }),
            // [5] the formula bar
            (5, Rect { x: 2146.0, y: 205.0, w: 740.0, h: 27.0 }),
            // [6] parked off-screen
            (6, Rect { x: -7994.0, y: -9910.0, w: 169.0, h: 37.0 }),
            // [7] one pixel
            (7, Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
        ]
    }

    #[test]
    fn the_formula_bar_is_picked_out_of_a_real_window() {
        // The whole reason this rule exists: eight Edits, and choosing wrong
        // reads a cell reference where a value was meant.
        assert_eq!(pick_formula_bar(name_box(), &measured_edits()), Some(5));
    }

    #[test]
    fn the_name_box_is_never_chosen_as_its_own_formula_bar() {
        // It shares the row and is a perfectly good Edit; only "begins at or
        // after the right edge" excludes it. Worth its own test, because this
        // is the specific confusion that would read "B2" as a customer name.
        let only_name_box = vec![(4, name_box())];
        assert_eq!(pick_formula_bar(name_box(), &only_name_box), None);
    }

    #[test]
    fn elements_on_other_rows_are_not_candidates() {
        // Zoom and font size sit above the formula bar and to its right; row
        // membership is what rules them out.
        let above: Vec<(usize, Rect)> = measured_edits()
            .into_iter()
            .filter(|(i, _)| *i == 2 || *i == 3)
            .collect();
        assert_eq!(pick_formula_bar(name_box(), &above), None);
    }

    #[test]
    fn degenerate_rectangles_are_rejected_even_when_they_share_the_row() {
        // A one-pixel Edit placed in the right row and to the right would
        // otherwise win on nearness alone.
        let sliver = vec![(9, Rect { x: 2100.0, y: 210.0, w: 1.0, h: 1.0 })];
        assert_eq!(pick_formula_bar(name_box(), &sliver), None);
    }

    #[test]
    fn the_nearest_qualifying_edit_wins() {
        let near = Rect { x: 2146.0, y: 205.0, w: 300.0, h: 27.0 };
        let far = Rect { x: 2600.0, y: 205.0, w: 300.0, h: 27.0 };
        assert_eq!(
            pick_formula_bar(name_box(), &[(1, far), (0, near)]),
            Some(0),
            "the field beside the Name Box is the formula bar; a later one is something else"
        );
    }

    #[test]
    fn a_taller_formula_bar_still_shares_the_row() {
        // Overlap rather than centre-matching: the measured bar is 27px against
        // the Name Box's 20px and starts two pixels higher.
        let taller = Rect { x: 2146.0, y: 190.0, w: 740.0, h: 60.0 };
        assert_eq!(pick_formula_bar(name_box(), &[(0, taller)]), Some(0));
    }

    #[test]
    fn nothing_qualifying_returns_none_rather_than_a_guess() {
        assert_eq!(pick_formula_bar(name_box(), &[]), None);
    }

    #[test]
    fn failing_to_identify_the_formula_bar_is_an_error_not_a_fallback() {
        // The fail-loud property, tested rather than asserted. `open` calls
        // exactly this, so a window without an identifiable formula bar
        // produces NO reader at all -- there is no partially-built state in
        // which reads quietly come from the wrong element.
        let err = require_formula_bar(name_box(), &[]).expect_err("must not succeed");
        assert!(
            matches!(err, SourceError::Unreachable(_)),
            "expected Unreachable, got {err:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("Refusing to read from another element"),
            "the error must say why there is no fallback, got: {message}"
        );

        // And with only the Name Box present -- the specific wrong answer that
        // would look plausible -- it still refuses rather than settling.
        assert!(require_formula_bar(name_box(), &[(4, name_box())]).is_err());
    }

    #[test]
    fn identifying_the_formula_bar_succeeds_on_the_real_window() {
        // The other half of the same property: it is not refusing everything.
        assert_eq!(
            require_formula_bar(name_box(), &measured_edits()).expect("real window"),
            5
        );
    }

    #[test]
    fn a_cell_reference_is_qualified_only_when_a_sheet_is_known() {
        assert_eq!(cell_ref(Some("Sheet2"), "B", 2), "Sheet2!B2");
        assert_eq!(cell_ref(None, "B", 2), "B2");
        // A blank sheet name is not a sheet -- qualifying with it would produce
        // "!B2", which addresses nothing.
        assert_eq!(cell_ref(Some("   "), "B", 2), "B2");
        assert_eq!(cell_ref(Some("Orders"), "AA", 47), "Orders!AA47");
    }
}
