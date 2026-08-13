//! Writing records into a spreadsheet -- the destination half of a run.
//!
//! The mirror of [`SpreadsheetReader`], and deliberately built out of the same
//! two elements: the Name Box addresses a cell, the formula bar reports what is
//! in it. Nothing else in a Google Sheets window is addressable enough to be
//! worth using, which is the finding `source::spreadsheet` records at length.
//!
//! ## Every step here was learned the hard way, not designed
//!
//! The write sequence is lifted from `replay::grid_type`, which arrived at it
//! through measurement. Three parts of it look arbitrary and are not:
//!
//! * **The commit key is `{Tab}`, not `{Enter}`.** `press_key` prefixes
//!   Enter-like keys with `{LEFT}{END}`, which relocates the cursor before the
//!   key lands. `replay` pins this with its own test; this module asserts the
//!   same invariant rather than trusting the constant to stay put.
//! * **The committer is re-fetched after typing.** Typing opens the cell editor
//!   and focus moves to it, so the element that was focused before typing is
//!   stale. Committing against the stale one abandons the edit. The symptom was
//!   precise: every cell landed except the last, because each pending edit was
//!   being committed by the *next* navigation rather than by its own Tab.
//! * **The Name Box is checked before typing, not after.** Nothing has been
//!   written at that point, so refusing costs nothing -- whereas typing into
//!   whatever happens to be selected is exactly how a run fills the wrong cell
//!   while reporting success.
//!
//! ## What is verified, and what cannot be
//!
//! Each write is read back through the formula bar and compared. That catches a
//! write that silently did not land, which is the failure this system keeps
//! finding.
//!
//! It does **not** verify the sheet. A qualified reference echoes back bare
//! from the Name Box -- `Sheet2!B2` in, `"B2"` out -- so nothing in the tree
//! confirms which tab a cell is on. Stated rather than papered over: the sheet
//! is verified end to end by CSV export in `examples/templated_run_probe.rs`,
//! not here.
//!
//! [`SpreadsheetReader`]: crate::source::spreadsheet::SpreadsheetReader

use std::time::Duration;
use terminator::{Desktop, UIElement};

use crate::run::DestinationWriter;
use crate::source::spreadsheet::{cell_ref, require_formula_bar, Rect};
use crate::source::SourceError;

/// Commits a cell edit. See the module docs -- Tab, never Enter.
const COMMIT_KEY: &str = "{Tab}";

/// After submitting the Name Box, before reading where it landed.
const NAVIGATE_SETTLE: Duration = Duration::from_millis(1200);
/// After typing, before committing.
const TYPE_SETTLE: Duration = Duration::from_millis(400);
/// After committing, before anything reads the cell back.
const COMMIT_SETTLE: Duration = Duration::from_millis(600);

/// Writes records into a spreadsheet, one field at a time.
pub struct SpreadsheetWriter {
    /// Kept, unlike [`SpreadsheetReader`], which deliberately keeps none.
    ///
    /// The difference is real: committing an edit needs whatever holds focus
    /// *after* typing, and only `Desktop::focused_element` can answer that. A
    /// reader never types, so it never needs to ask.
    ///
    /// [`SpreadsheetReader`]: crate::source::spreadsheet::SpreadsheetReader
    desktop: Desktop,
    name_box: UIElement,
    formula_bar: UIElement,
    destination_id: String,
    sheet: Option<String>,
    row: u64,
}

impl SpreadsheetWriter {
    /// Resolve the two elements this writer needs.
    ///
    /// The only async part, for the same reason as the reader: [`DestinationWriter`]
    /// must stay object-safe so a run can hold one as `Box<dyn ...>`.
    ///
    /// Fails at construction rather than on a later write. A writer that exists
    /// is one that knows how to address a cell and how to check what landed
    /// there; there is no half-built state in which writes go somewhere unknown.
    pub async fn open(
        desktop: Desktop,
        window: &UIElement,
        destination_id: impl Into<String>,
        sheet: Option<String>,
        first_row: u64,
    ) -> Result<Self, SourceError> {
        let name_box = desktop
            .locator("name:Name box")
            .within(window.clone())
            .all(Some(Duration::from_secs(5)), None)
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
                    "no Name Box on the destination: a cell is reached through it, and without \
                     one there is no way to address where a record should go"
                        .to_string(),
                )
            })?;

        let name_box_rect = bounds_of(&name_box)?;

        let edits = desktop
            .locator("role:Edit")
            .within(window.clone())
            .all(Some(Duration::from_secs(6)), None)
            .await
            .map_err(|e| SourceError::Unreachable(format!("Edit enumeration failed: {e}")))?;

        let rects: Vec<(usize, Rect)> = edits
            .iter()
            .enumerate()
            .filter_map(|(i, el)| bounds_of(el).ok().map(|r| (i, r)))
            .collect();

        let chosen = require_formula_bar(name_box_rect, &rects)?;

        Ok(Self {
            desktop,
            name_box,
            formula_bar: edits[chosen].clone(),
            destination_id: destination_id.into(),
            sheet,
            row: first_row,
        })
    }

    pub fn destination_id(&self) -> &str {
        &self.destination_id
    }

    /// Move the selection to a cell and confirm it arrived.
    ///
    /// Compared against the BARE cell, because a qualified reference echoes back
    /// without its sheet. Comparing against the full reference would reject
    /// every cross-sheet write at the moment it worked.
    fn goto(&self, column: &str, row: u64) -> Result<(), SourceError> {
        let reference = cell_ref(self.sheet.as_deref(), column, row);
        let bare = format!("{column}{row}");

        self.name_box
            .set_value(&reference)
            .map_err(|e| SourceError::Unreadable {
                locator: column.to_string(),
                row: row.to_string(),
                reason: format!("could not address the Name Box: {e}"),
            })?;
        self.name_box
            .press_key("{Enter}")
            .map_err(|e| SourceError::Unreadable {
                locator: column.to_string(),
                row: row.to_string(),
                reason: format!("could not submit the Name Box: {e}"),
            })?;
        std::thread::sleep(NAVIGATE_SETTLE);

        let landed = self.name_box.text(0).unwrap_or_default();
        if landed.trim() != bare {
            return Err(SourceError::PositionLost(format!(
                "asked for {reference:?} but the Name Box reads {landed:?}; refusing to write to \
                 a cell that is not demonstrably the one requested"
            )));
        }
        Ok(())
    }

    /// Type into whatever cell is currently selected, and commit it.
    fn type_here(&self, column: &str, row: u64, value: &str) -> Result<(), SourceError> {
        let err = |reason: String| SourceError::Unreadable {
            locator: column.to_string(),
            row: row.to_string(),
            reason,
        };

        let target = self
            .desktop
            .focused_element()
            .map_err(|e| err(format!("nothing holds focus to type into: {e}")))?;

        // An empty value still has to clear whatever was there: a run that
        // leaves a stale value in a cell it was asked to blank has written the
        // wrong record. `set_value` on the editor is not available here, so the
        // cell is selected and typed over, which replaces its contents.
        target
            .type_text(value, false)
            .map_err(|e| err(format!("typing failed: {e}")))?;
        std::thread::sleep(TYPE_SETTLE);

        // Re-fetched, not reused. See the module docs: `target` is stale by now
        // because typing moved focus into the cell editor.
        let committer = self.desktop.focused_element().unwrap_or(target);
        debug_assert!(
            !COMMIT_KEY.to_uppercase().contains("ENTER")
                && !COMMIT_KEY.to_uppercase().contains("RETURN"),
            "the commit key must not be one press_key prefixes with {{LEFT}}{{END}}"
        );
        committer
            .press_key(COMMIT_KEY)
            .map_err(|e| err(format!("typed the value but committing it failed: {e}")))?;
        std::thread::sleep(COMMIT_SETTLE);
        Ok(())
    }

    /// Read a cell back through the formula bar.
    fn read_back(&self, column: &str, row: u64) -> Result<String, SourceError> {
        self.goto(column, row)?;
        let raw = self
            .formula_bar
            .text(0)
            .map_err(|e| SourceError::Unreadable {
                locator: column.to_string(),
                row: row.to_string(),
                reason: format!("could not read the formula bar back: {e}"),
            })?;
        Ok(raw.trim().to_string())
    }
}

fn bounds_of(el: &UIElement) -> Result<Rect, SourceError> {
    let (x, y, width, height) = el
        .bounds()
        .map_err(|e| SourceError::Unreachable(format!("element has no bounds: {e}")))?;
    Ok(Rect {
        x,
        y,
        w: width,
        h: height,
    })
}

impl DestinationWriter for SpreadsheetWriter {
    fn position(&self) -> String {
        self.row.to_string()
    }

    /// Navigate, type, commit, read back, compare.
    ///
    /// The read-back is not belt-and-braces. A write that silently does not
    /// land is the single most common failure this system has found, and a run
    /// that marked such a record as processed would skip it forever -- §4.7's
    /// ledger is only as trustworthy as the writes it records.
    fn write(&mut self, field: &str, value: &str) -> Result<(), SourceError> {
        let row = self.row;
        self.goto(field, row)?;
        self.type_here(field, row, value)?;

        let landed = self.read_back(field, row)?;
        if landed != value.trim() {
            return Err(SourceError::Unreadable {
                locator: field.to_string(),
                row: row.to_string(),
                reason: format!(
                    "wrote {value:?} but the cell reads {landed:?} afterwards; refusing to \
                     report a write that did not land"
                ),
            });
        }
        Ok(())
    }

    fn advance(&mut self, step: i64) -> Result<(), SourceError> {
        // Guarded rather than wrapped: a negative step past row 1 would wrap a
        // u64 into an enormous row, and the resulting Name Box navigation would
        // fail somewhere far from the cause.
        let next = self.row as i64 + step;
        if next < 1 {
            return Err(SourceError::PositionLost(format!(
                "a step of {step} from row {} would land on row {next}, which does not exist",
                self.row
            )));
        }
        self.row = next as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commit_key_cannot_relocate_the_cursor() {
        // The same invariant `replay` pins, asserted here too rather than
        // assumed to hold because a constant elsewhere says so. An Enter-like
        // commit key gets prefixed with {LEFT}{END} by press_key, which moves
        // the cursor before the key lands.
        let k = COMMIT_KEY.to_uppercase();
        assert!(
            !k.contains("ENTER") && !k.contains("RETURN"),
            "commit key {COMMIT_KEY:?} would be prefixed with navigation by press_key"
        );
    }

    #[test]
    fn the_settle_delays_are_ordered_the_way_the_sequence_needs() {
        // Navigation is the slowest step -- it waits on a real grid scroll --
        // and committing has to outlast the editor closing. Encoded so a later
        // "tidy these up to one constant" has to argue with a test.
        assert!(NAVIGATE_SETTLE > COMMIT_SETTLE);
        assert!(COMMIT_SETTLE > TYPE_SETTLE);
    }
}
