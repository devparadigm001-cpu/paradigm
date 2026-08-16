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
        let address = address_of(desktop, &w).await;
        if is_export_url(&address) {
            continue;
        }
        if address.contains(document) {
            return Some(w);
        }
    }
    None
}

/// Is this address a CSV export rather than a document being viewed?
///
/// A guard, and a permanent one. The export URL **contains the document id** --
/// `.../d/<id>/export?format=csv&gid=0` -- so a browser window left sitting on
/// one satisfies a plain `contains(document)` match perfectly. The run would
/// then open a reader on a window holding no spreadsheet: no Name Box, no
/// formula bar, and a failure somewhere far from the cause.
///
/// That is not hypothetical. `csv_snapshot::fetch_export` opens exactly this
/// URL to fetch a scan's data, and on a COLD browser -- the unattended case it
/// exists for -- the window it opens has no other tab to fall back to and
/// lingers as "Untitled". Measured: `window_for` then matched it in preference
/// to the real document.
///
/// `fetch_export` now closes that window, so this should rarely fire. It is
/// kept anyway because the close is best-effort and this is not: any future
/// route that leaves an export URL on screen -- a user opening one by hand, a
/// failed download, a second export mechanism -- would resurrect the same bug,
/// and a document is never legitimately *worked in* through its export URL.
pub fn is_export_url(address: &str) -> bool {
    let lower = address.to_lowercase();
    lower.contains("/export?") || lower.contains("format=csv")
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

    // The SOURCE may not need a window at all -- see the reader choice below.
    // The destination always does, because writing needs the live grid.
    let exportable =
        crate::source::csv_snapshot::exportable_doc_id(&template.source_id).map(str::to_string);

    let source_window = match &exportable {
        Some(_) => None,
        None => match window_for(desktop, source_doc).await {
            Some(w) => Some(w),
            None => return Err(not_found(desktop, "source", source_doc).await),
        },
    };
    let destination_window = match window_for(desktop, destination_doc).await {
        Some(w) => w,
        None => return Err(not_found(desktop, "destination", destination_doc).await),
    };

    let scan_columns: Vec<String> = template
        .fields
        .iter()
        .map(|f| f.source_field.clone())
        .collect();

    // The run's source is read by EXPORT when the surface names a whole
    // document, and through the formula bar only when it does not.
    //
    // Not an optimisation -- a correctness requirement. A Sheets formula bar
    // reports nothing until the page has been typed into, and nobody ever types
    // into a source document, so `SpreadsheetReader` opened on a freshly loaded
    // source refuses. See
    // `docs/known-issues/the-formula-bar-only-reports-after-the-page-is-typed-into.md`.
    // An export needs no window and no provocation, and costs ~2.12s per record
    // against ~1.95s for a two-column formula-bar read -- flat in columns
    // rather than linear.
    //
    // A sheet-qualified source (`<doc>!Sheet2`) still uses the reader: the
    // export URL selects a sheet by gid, a template names one by NAME, and
    // mapping between them needs the document open, which is the cost this
    // avoids. Those sources are also the one-document case that already worked.
    //
    // The DESTINATION is deliberately untouched. It writes before it reads
    // back, so its page is always awake by the time it reads, and it has never
    // had this problem.
    let reader: Box<dyn crate::source::SourceReader + Send> = match (&exportable, &source_window) {
        (Some(doc_id), _) => Box::new(
            crate::source::csv_live::CsvLiveReader::new(
                doc_id.clone(),
                template.source_id.clone(),
                "0",
                source_row,
                header_row,
                scan_columns,
            )
            .map_err(|e| format!("could not open the source: {e}"))?,
        ),
        (None, Some(window)) => Box::new(
            SpreadsheetReader::open(
                desktop.clone(),
                window,
                template.source_id.clone(),
                source_sheet.map(str::to_string),
                source_row,
                header_row,
                scan_columns,
            )
            .await
            .map_err(|e| format!("could not open the source: {e}"))?,
        ),
        (None, None) => {
            return Err(format!(
                "no open window is showing the source document {source_doc}"
            ))
        }
    };

    let writer = SpreadsheetWriter::open(
        desktop.clone(),
        &destination_window,
        template.destination_id.clone(),
        destination_sheet.map(str::to_string),
        destination_row,
    )
    .await
    .map_err(|e| format!("could not open the destination: {e}"))?;

    Ok((reader, Box::new(writer)))
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

/// Does any window announce that it is holding background tabs?
///
/// Browsers title a multi-tab window "<active page> and N more pages", so the
/// title says how many pages are hidden behind the one being reported. That is
/// the only signal available: [`address_of`] reads a window's single address
/// bar, which shows the ACTIVE tab, so a document in any other tab is
/// unreachable to matching no matter how plainly it is open.
///
/// Pure, so the phrase-matching is tested rather than asserted.
pub fn background_tabs_in(titles: &[String]) -> bool {
    titles.iter().any(|t| {
        let t = t.to_lowercase();
        t.contains(" more page") || t.contains(" more tab")
    })
}

/// Explain a document that could not be resolved, without overstating.
///
/// The old message was one sentence -- "no open window is showing the
/// destination document" -- and it was measured false in the ordinary case: a
/// user had both documents open, in tabs of one window, and the destination was
/// simply not frontmost. See
/// `docs/known-issues/two-documents-in-one-window-cannot-both-resolve.md`,
/// where a nine-tab window resolved its active tab and hid the other eight.
///
/// So the message now depends on what can actually be established:
///
/// * background tabs detected -- say it MAY be behind one, and say what to do;
/// * none detected -- the original claim, which is now the case it fits;
/// * titles unreadable -- say that too, rather than picking one.
///
/// Deliberately hedged in the first case. The title says a window has hidden
/// pages; it does not say WHICH document is in them, and claiming the
/// destination is definitely there would replace one confident wrong answer
/// with another.
async fn not_found(desktop: &Desktop, role: &str, document: &str) -> String {
    let titles: Vec<String> = match desktop
        .locator("role:Window")
        .within(desktop.root())
        .all(Some(std::time::Duration::from_secs(5)), Some(3))
        .await
    {
        Ok(windows) => windows.iter().filter_map(|w| w.name()).collect(),
        Err(e) => {
            return format!(
                "could not find a window showing the {role} document {document}, and could not \
                 read the open windows to say why: {e}"
            )
        }
    };

    if titles.is_empty() {
        return format!(
            "could not find a window showing the {role} document {document}. No window titles \
             could be read, so whether it is open at all is unknown"
        );
    }

    if background_tabs_in(&titles) {
        format!(
            "could not reach the {role} document {document}. It MAY be open in a background \
             tab -- a window here reports having more pages behind the one it is showing, and \
             only the front tab of a window can be found. Bring that document to the front, or \
             give it its own window, and try again"
        )
    } else {
        format!("no open window is showing the {role} document {document}")
    }
}

#[cfg(test)]
mod not_found_tests {
    use super::background_tabs_in;

    fn titles(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The real title that produced the false error, verbatim.
    #[test]
    fn a_window_announcing_more_pages_is_detected() {
        assert!(background_tabs_in(&titles(&[
            "Untitled spreadsheet - Google Sheets and 8 more pages - Personal - Microsoft Edge",
        ])));
        // Singular, and the Chrome phrasing.
        assert!(background_tabs_in(&titles(&["Something and 1 more page - Google Chrome"])));
        assert!(background_tabs_in(&titles(&["Something and 3 more tabs"])));
    }

    /// Only one window needs to have them for the hedge to be warranted.
    #[test]
    fn one_window_among_many_is_enough() {
        assert!(background_tabs_in(&titles(&[
            "Paradigm",
            "Untitled spreadsheet - Google Sheets - Personal - Microsoft Edge",
            "Example Domain and 11 more pages - Personal - Microsoft Edge",
        ])));
    }

    /// And when nothing suggests hidden tabs, the original blunt message is the
    /// correct one -- hedging every failure would make the hedge meaningless.
    #[test]
    fn single_tab_windows_do_not_trigger_the_hedge() {
        assert!(!background_tabs_in(&titles(&[
            "Paradigm",
            "Untitled spreadsheet - Google Sheets - Personal - Microsoft Edge",
        ])));
        assert!(!background_tabs_in(&titles(&[])));
        // "more" alone must not be enough -- a document can be named anything.
        assert!(!background_tabs_in(&titles(&["Read more about pages.docx"])));
    }
}

#[cfg(test)]
mod export_guard_tests {
    use super::is_export_url;

    /// The exact address that poisoned matching, verbatim from a live window.
    #[test]
    fn an_export_url_is_recognised() {
        assert!(is_export_url(
            "https://docs.google.com/spreadsheets/d/1ko7z65TnzI5suwvu3LGv8yoemmhOs5siSQe9KBB8NZs/export?format=csv&gid=0"
        ));
        assert!(is_export_url("https://docs.google.com/spreadsheets/d/ID/export?format=tsv"));
        // Case is not guaranteed by anything.
        assert!(is_export_url("HTTPS://DOCS.GOOGLE.COM/SPREADSHEETS/D/ID/EXPORT?FORMAT=CSV"));
    }

    /// And a document actually being worked in must still match.
    ///
    /// The guard is only safe because a document is never legitimately edited
    /// through its export URL -- if this ever failed, the guard would be
    /// hiding real windows rather than dead ones.
    #[test]
    fn a_document_being_viewed_is_not_excluded() {
        assert!(!is_export_url(
            "https://docs.google.com/spreadsheets/d/1ko7z65TnzI5suwvu3LGv8yoemmhOs5siSQe9KBB8NZs/edit?gid=0#gid=0"
        ));
        assert!(!is_export_url("https://docs.google.com/spreadsheets/d/ID/edit"));
        assert!(!is_export_url("https://example.com"));
        assert!(!is_export_url(""));
    }
}
