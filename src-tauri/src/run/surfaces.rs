//! Turning a template's `source_id` / `destination_id` into live surfaces.
//!
//! A template stores *which* source and destination it works between, as
//! `<document>!<sheet>`. Nothing in it says which window that is, because
//! windows are not durable and §3 keeps the template to structure. So
//! something has to resolve one into the other at run time, and this is it.
//!
//! Kept out of `commands.rs` on purpose -- that file's own docs say no business
//! logic lives there, and "find the browser window whose address bar mentions
//! this document" is exactly that.
//!
//! ## Why the factory builds its own runtime
//!
//! Resolving a window is async (the locator API), but a run executes on a plain
//! `std::thread` with no reactor on it. The factory therefore stands up a
//! single-threaded runtime, resolves, and tears it down -- paid once per run,
//! on the run's own thread, which is also where the COM handles need to be
//! created anyway.

use terminator::{Desktop, UIElement};

use crate::compile::CompiledTemplate;
use crate::run::background::{SurfaceFactory, Surfaces};
use crate::run::spreadsheet::SpreadsheetWriter;
use crate::run::DestinationWriter;
use crate::source::spreadsheet::SpreadsheetReader;

/// Split `<document>!<sheet>` into its two halves.
///
/// A surface id with no `!` is a whole document with no sheet named, which is
/// legitimate -- a single-sheet document addresses cells bare.
pub fn split_surface_id(id: &str) -> (&str, Option<&str>) {
    match id.split_once('!') {
        Some((doc, sheet)) if !doc.is_empty() && !sheet.is_empty() => (doc, Some(sheet)),
        _ => (id, None),
    }
}

/// The address bar's text for a window, if it has one.
async fn address_of(desktop: &Desktop, window: &UIElement) -> String {
    if let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(std::time::Duration::from_secs(5)), None)
        .await
    {
        for b in &bars {
            let t = b.text(0).unwrap_or_default();
            if !t.is_empty() {
                return t;
            }
        }
    }
    String::new()
}

/// Find the window showing a given document.
///
/// Matched on the document id in the address bar, not on the window title.
/// Titles are user-editable and duplicate freely -- two "Untitled spreadsheet"
/// windows are the normal case, not the exception -- whereas the id in the URL
/// identifies exactly one document. This is the same reasoning
/// `docs/known-issues/replay-window-selector-ambiguity.md` records for replay.
pub async fn window_for(desktop: &Desktop, document: &str) -> Option<UIElement> {
    let windows = desktop
        .locator("role:Window")
        .within(desktop.root())
        .all(Some(std::time::Duration::from_secs(8)), Some(3))
        .await
        .ok()?;

    for w in windows {
        if address_of(desktop, &w).await.contains(document) {
            return Some(w);
        }
    }
    None
}

/// Open a reader on the source and a writer on the destination.
///
/// `scan_columns` are the columns whose header the reader will read for drift
/// and for labelling a preview; the mapped source columns are the ones that
/// matter, so they are what gets passed.
pub async fn open_for(
    desktop: &Desktop,
    template: &CompiledTemplate,
    source_row: u64,
    header_row: u64,
    destination_row: u64,
) -> Result<Surfaces, String> {
    let (source_doc, source_sheet) = split_surface_id(&template.source_id);
    let (destination_doc, destination_sheet) = split_surface_id(&template.destination_id);

    let source_window = window_for(desktop, source_doc)
        .await
        .ok_or_else(|| format!("no open window is showing the source document {source_doc}"))?;
    let destination_window = window_for(desktop, destination_doc).await.ok_or_else(|| {
        format!("no open window is showing the destination document {destination_doc}")
    })?;

    let scan_columns: Vec<String> = template
        .fields
        .iter()
        .map(|f| f.source_field.clone())
        .collect();

    let reader = SpreadsheetReader::open(
        desktop.clone(),
        &source_window,
        template.source_id.clone(),
        source_sheet.map(str::to_string),
        source_row,
        header_row,
        scan_columns,
    )
    .await
    .map_err(|e| format!("could not open the source: {e}"))?;

    let writer = SpreadsheetWriter::open(
        desktop.clone(),
        &destination_window,
        template.destination_id.clone(),
        destination_sheet.map(str::to_string),
        destination_row,
    )
    .await
    .map_err(|e| format!("could not open the destination: {e}"))?;

    Ok((Box::new(reader), Box::new(writer)))
}

/// What the user has selected in a surface, in the terms a correction needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The column letter, e.g. `"D"`.
    pub column: String,
    /// The header at that column, when it has one. §4.5's panel says "You
    /// selected column D — 'Client Phone.'", and the quoted half comes from
    /// here; without it the confirmation can only name a letter, which is a
    /// far weaker guard against a mis-click.
    pub label: Option<String>,
}

/// Read the cell the user currently has selected on one side of a workflow.
///
/// This is what makes §4.5's interaction *click-only*: the user clicks a column
/// in the live spreadsheet, and this reports which one, so nothing has to be
/// typed. Nothing else in the system asks this question -- readers and writers
/// both *set* the selection through the Name Box and never read it back as user
/// input.
///
/// ## It puts the selection back
///
/// Reading the header label means navigating to the header row, which moves the
/// user's cursor. Leaving it there would be a UI that quietly rearranges the
/// document it is asking about -- and worse, a second read would then report
/// the header cell as the user's selection. So the original cell is restored
/// afterwards, best-effort: failing to restore is not worth failing the read
/// the user is waiting on.
pub async fn read_selection(
    desktop: &Desktop,
    surface_id: &str,
    header_row: u64,
) -> Result<Selection, String> {
    let (document, sheet) = split_surface_id(surface_id);
    let window = window_for(desktop, document)
        .await
        .ok_or_else(|| format!("no open window is showing {document}"))?;

    let name_box = desktop
        .locator("name:Name box")
        .within(window.clone())
        .all(Some(std::time::Duration::from_secs(5)), None)
        .await
        .map_err(|e| format!("Name Box lookup failed: {e}"))?
        .into_iter()
        .next()
        .and_then(|g| {
            g.children()
                .ok()
                .and_then(|c| c.into_iter().find(|e| e.role() == "Edit"))
        })
        .ok_or_else(|| "no Name Box, so there is no way to tell what is selected".to_string())?;

    let raw = name_box.text(0).unwrap_or_default();
    let raw = raw.trim().to_string();
    let (column, _row) = crate::source::spreadsheet::parse_cell_ref(&raw)
        .ok_or_else(|| format!("the Name Box reads {raw:?}, which is not a single cell"))?;

    // The header for that column, read through a writer because it is the one
    // that exposes `shape` for arbitrary columns.
    let label = match SpreadsheetWriter::open(
        desktop.clone(),
        &window,
        surface_id.to_string(),
        sheet.map(str::to_string),
        header_row,
    )
    .await
    {
        Ok(mut w) => w
            .shape(&[column.clone()], header_row)
            .ok()
            .and_then(|s| s.columns.into_iter().next())
            .map(|c| c.label),
        Err(_) => None,
    };

    // Put the cursor back where the user left it.
    let restore = match sheet {
        Some(s) => format!("{s}!{raw}"),
        None => raw.clone(),
    };
    let _ = name_box.set_value(&restore);
    let _ = name_box.press_key("{Enter}");

    Ok(Selection { column, label })
}

/// A [`SurfaceFactory`] that resolves the windows on the run's own thread.
pub fn factory_for(
    template: CompiledTemplate,
    source_row: u64,
    header_row: u64,
    destination_row: u64,
) -> SurfaceFactory {
    Box::new(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("could not start a runtime for the run: {e}"))?;
        runtime.block_on(async {
            let desktop = Desktop::new(false, false)
                .map_err(|e| format!("accessibility engine unavailable: {e}"))?;
            open_for(
                &desktop,
                &template,
                source_row,
                header_row,
                destination_row,
            )
            .await
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qualified_surface_id_splits_into_document_and_sheet() {
        assert_eq!(split_surface_id("abc123!Sheet2"), ("abc123", Some("Sheet2")));
    }

    #[test]
    fn an_unqualified_surface_id_is_a_whole_document() {
        // Legitimate, not malformed: a single-sheet document addresses bare.
        assert_eq!(split_surface_id("abc123"), ("abc123", None));
    }

    #[test]
    fn a_degenerate_id_is_not_split() {
        assert_eq!(split_surface_id("abc!"), ("abc!", None));
        assert_eq!(split_surface_id("!Sheet2"), ("!Sheet2", None));
    }
}
