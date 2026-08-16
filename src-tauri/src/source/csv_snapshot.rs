//! A [`SourceReader`] over a whole-sheet CSV export.
//!
//! ## Why this exists alongside [`SpreadsheetReader`], not instead of it
//!
//! `SpreadsheetReader` reads one cell per Name Box navigation, and each of those
//! costs ~0.98s of which ~900ms is `NAVIGATE_SETTLE` -- a correctness control,
//! not a comfort margin. At the scan's own `SCAN_LIMIT = 200` that is ~391s.
//! One CSV export of the same sheet takes ~2.0s regardless of row count, and a
//! cell edited 1.3s earlier is already in it. Both measured; see
//! `docs/known-issues/batch-scan-cost-is-linear-in-rows.md`.
//!
//! This began as a **scan-only** reader. It now serves the run as well, which
//! is a decision the user made explicitly and with the cost stated. Both halves
//! of the original argument are worth keeping, because only one of them turned
//! out to be true:
//!
//! * **A scan is a single read-only question** -- "which rows are unprocessed"
//!   -- asked once and answered from one consistent moment. A snapshot is
//!   exactly the right shape for it. That still holds.
//! * **A run is not.** It interleaves reads with writes, pauses for §4.5
//!   corrections, and can sit waiting on a human for minutes. Answering its
//!   reads from a snapshot taken at spawn means writing values the source no
//!   longer holds, silently. That risk is real and has not gone away -- it was
//!   weighed against the cost of the alternative and deliberately accepted.
//!
//! The alternative was a sibling reader that re-exported the sheet before every
//! record. Measured over a nine-record sheet it cost ~9-10s per record and
//! **failed to finish the sheet in either trial**, because back-to-back export
//! windows interfere with each other. One export costs ~1-2s for the whole run.
//! See `docs/known-issues/the-run-reads-its-source-once.md` for the numbers and
//! for what a run gives up in exchange.
//!
//! One clause of the old argument was simply wrong and is corrected here: a
//! snapshot does **not** leave §4.5's drift check "looking at the same stale
//! copy and finding nothing wrong." The source shape is read once, in
//! `run::background`, before the record loop starts -- and the loop never reads
//! it again. A body exported at `open_for` is contemporaneous with that check,
//! so it compares the source's real shape at run start, as it always did. What
//! the per-record fetch actually bought was fresher *values*, not fresher drift
//! detection.
//!
//! §2's "generator interface, spreadsheet-only for now" anticipated a second
//! implementation behind this interface, and the trait needed no change to
//! accept it.
//!
//! ## What it does NOT solve
//!
//! Fetching. Obtaining the CSV needs the user's signed-in browser session -- a
//! plain request for the export URL returns 401 -- so the body is passed in
//! rather than fetched here. That keeps every rule below testable without a
//! browser, and keeps the awkward part (a window, a download, a file to delete)
//! in one place at the edge.

use super::{
    classify_row, Advance, ColumnShape, FieldRef, SourceError, SourcePosition, SourceReader,
    SourceRecord, SourceShape,
};

/// How many rows past a blank one to look before calling the source exhausted.
///
/// Matches `SpreadsheetReader::LOOKAHEAD_ROWS`. The two readers must answer
/// §4.10's "end of data or a gap?" identically, or swapping one for the other
/// changes what a scan concludes -- which is the whole thing this must not do.
const LOOKAHEAD_ROWS: u64 = 5;

/// A parsed CSV export, addressed the way a spreadsheet is.
pub struct CsvSnapshot {
    source_id: String,
    /// Row-major, 0-based. `rows[0]` is sheet row 1.
    rows: Vec<Vec<String>>,
    /// The sheet row the reader is on, 1-based, matching `SpreadsheetReader`.
    row: u64,
    header_row: u64,
    scan_columns: Vec<String>,
}

impl CsvSnapshot {
    /// Parse an export body and address it from `first_row`.
    pub fn new(
        source_id: impl Into<String>,
        body: &str,
        first_row: u64,
        header_row: u64,
        scan_columns: Vec<String>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            rows: parse_csv(body),
            row: first_row.max(1),
            header_row,
            scan_columns,
        }
    }

    /// One cell, by spreadsheet column letter and 1-based row.
    ///
    /// A cell past the end of the export is empty rather than an error: Sheets
    /// exports a rectangle, and a row that is simply not in it is blank in
    /// exactly the sense `classify_row` means.
    fn cell(&self, column: &str, row: u64) -> String {
        let Some(index) = column_index(column) else {
            return String::new();
        };
        self.rows
            .get((row.saturating_sub(1)) as usize)
            .and_then(|r| r.get(index))
            .cloned()
            .unwrap_or_default()
    }

    fn read_row(&self, fields: &[FieldRef], row: u64) -> Vec<String> {
        fields.iter().map(|f| self.cell(&f.locator, row)).collect()
    }
}

impl SourceReader for CsvSnapshot {
    fn position(&self) -> SourcePosition {
        SourcePosition {
            source_id: self.source_id.clone(),
            row_key: self.row.to_string(),
        }
    }

    fn peek(&mut self, fields: &[FieldRef]) -> Result<Advance, SourceError> {
        let current = self.read_row(fields, self.row);
        if current.iter().any(|v| !v.trim().is_empty()) {
            return Ok(Advance::Record);
        }
        let ahead: Vec<Vec<String>> = (1..=LOOKAHEAD_ROWS)
            .map(|offset| self.read_row(fields, self.row + offset))
            .collect();
        Ok(classify_row(&current, &ahead))
    }

    fn read(&mut self, fields: &[FieldRef]) -> Result<SourceRecord, SourceError> {
        let values = self.read_row(fields, self.row);
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
            let label = self.cell(column, self.header_row);
            if !label.trim().is_empty() {
                columns.push(ColumnShape {
                    locator: column.clone(),
                    label: label.trim().to_string(),
                });
            }
        }
        Ok(SourceShape { columns })
    }
}

/// `"A"` -> 0, `"Z"` -> 25, `"AA"` -> 26. `None` for anything that is not a
/// column reference.
///
/// Spreadsheet columns are bijective base-26 -- there is no zero digit, so `AA`
/// follows `Z` rather than `BA`. Getting this wrong would read the wrong column
/// silently, which is the failure this whole module has to avoid.
fn column_index(column: &str) -> Option<usize> {
    let column = column.trim();
    if column.is_empty() || !column.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut index = 0usize;
    for c in column.chars() {
        let digit = (c.to_ascii_uppercase() as usize) - ('A' as usize) + 1;
        index = index.checked_mul(26)?.checked_add(digit)?;
    }
    Some(index - 1)
}

/// Parse a CSV body into rows of fields.
///
/// Quote-aware, because the alternative is not. A cell holding a comma --
/// "Northwind Traders, Inc." is an ordinary customer name -- or a newline would
/// otherwise split into extra fields or extra rows, and every row below it
/// would shift. The probe helper this replaces made exactly that mistake and
/// reported a value in the wrong cell as absent; see
/// `an-open-cell-editor-turns-a-write-into-an-append.md`.
fn parse_csv(body: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = body.chars().peekable();
    let mut saw_any = false;

    while let Some(c) = chars.next() {
        saw_any = true;
        if quoted {
            match c {
                // "" inside a quoted field is one literal quote.
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    // A final row with no trailing newline still counts.
    if !field.is_empty() || !row.is_empty() || (saw_any && rows.is_empty()) {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(names: &[&str]) -> Vec<FieldRef> {
        names
            .iter()
            .map(|n| FieldRef {
                name: (*n).to_string(),
                locator: (*n).to_string(),
            })
            .collect()
    }

    /// The export shape this actually meets: a header, some rows, and the
    /// allocated-but-blank rows Sheets emits as bare commas.
    const SHEET: &str = "Customer,Amount\r\nAcme,100\r\nGlobex,200\r\nInitech,300\r\n,\r\n,\r\n,\r\n,\r\n,\r\n,\r\n";

    /// The run's source reader walks every record from the body it was built
    /// with, and nothing in the read path reaches for a fresh one.
    ///
    /// This is the contract `run::surfaces::open_for` now depends on: it
    /// exports once and hands the body here. The reader this replaced dropped
    /// its cache in `advance` to force a re-fetch per record; if that idea ever
    /// comes back, `advance` will have to do more than increment and this test
    /// is where it shows up. See
    /// `docs/known-issues/the-run-reads-its-source-once.md`.
    #[test]
    fn one_export_body_serves_the_whole_run() {
        let f = fields(&["A", "B"]);
        let mut reader = CsvSnapshot::new("doc!Sheet1", SHEET, 2, 1, vec!["A".into(), "B".into()]);

        let mut seen = Vec::new();
        while matches!(reader.peek(&f), Ok(Advance::Record)) {
            let record = reader.read(&f).expect("read");
            seen.push(record.fields.get("A").cloned().unwrap_or_default());
            reader.advance().expect("advance");
        }

        assert_eq!(seen, vec!["Acme", "Globex", "Initech"]);
        // The shape still reads the header out of that same body, which is what
        // keeps the run-start drift check meaningful.
        let shape = reader.shape().expect("shape");
        let labels: Vec<&str> = shape.columns.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["Customer", "Amount"]);
    }

    #[test]
    fn a_column_letter_maps_to_the_right_index() {
        assert_eq!(column_index("A"), Some(0));
        assert_eq!(column_index("B"), Some(1));
        assert_eq!(column_index("Z"), Some(25));
        // Bijective base-26: AA follows Z, and is 26 rather than 27.
        assert_eq!(column_index("AA"), Some(26));
        assert_eq!(column_index("AB"), Some(27));
        assert_eq!(column_index("BA"), Some(52));
        assert_eq!(column_index(""), None);
        assert_eq!(column_index("A1"), None);
    }

    #[test]
    fn a_comma_or_newline_inside_a_cell_does_not_split_it() {
        let rows = parse_csv("Customer,Amount\n\"Northwind Traders, Inc.\",305.75\n\"two\nlines\",9\n");
        assert_eq!(rows[1][0], "Northwind Traders, Inc.");
        assert_eq!(rows[1][1], "305.75");
        assert_eq!(rows[2][0], "two\nlines");
        assert_eq!(rows[2][1], "9");
    }

    #[test]
    fn a_doubled_quote_is_one_literal_quote() {
        let rows = parse_csv("a,\"say \"\"hi\"\"\"\n");
        assert_eq!(rows[0][1], "say \"hi\"");
    }

    #[test]
    fn cells_are_addressed_the_way_the_sheet_is() {
        let snap = CsvSnapshot::new("doc", SHEET, 2, 1, vec!["A".into(), "B".into()]);
        assert_eq!(snap.cell("A", 1), "Customer");
        assert_eq!(snap.cell("A", 2), "Acme");
        assert_eq!(snap.cell("B", 4), "300");
        // Past the export's rectangle is blank, not an error.
        assert_eq!(snap.cell("A", 999), "");
        assert_eq!(snap.cell("ZZ", 2), "");
    }

    /// The property that makes this swappable: the same walk must classify the
    /// same rows the same way `SpreadsheetReader` does.
    #[test]
    fn a_walk_finds_the_records_then_reports_exhausted() {
        let f = fields(&["A", "B"]);
        let mut snap = CsvSnapshot::new("doc", SHEET, 2, 1, vec!["A".into(), "B".into()]);

        let mut seen = Vec::new();
        for _ in 0..10 {
            match snap.peek(&f).expect("peek") {
                Advance::Record => {
                    let record = snap.read(&f).expect("read");
                    seen.push((
                        record.position.row_key.clone(),
                        record.fields["A"].clone(),
                        record.fields["B"].clone(),
                    ));
                    snap.advance().expect("advance");
                }
                Advance::Exhausted => break,
                other => panic!("unexpected {other:?}"),
            }
        }

        assert_eq!(
            seen,
            vec![
                ("2".to_string(), "Acme".to_string(), "100".to_string()),
                ("3".to_string(), "Globex".to_string(), "200".to_string()),
                ("4".to_string(), "Initech".to_string(), "300".to_string()),
            ]
        );
    }

    /// §4.10's gap, not the end of the data -- and it must be reported as such
    /// from a snapshot exactly as it is from a live read.
    #[test]
    fn a_gap_with_data_below_is_not_exhaustion() {
        let body = "H,H\nAcme,100\n,\n,\nInitech,300\n";
        let f = fields(&["A", "B"]);
        let mut snap = CsvSnapshot::new("doc", body, 3, 1, vec!["A".into()]);
        match snap.peek(&f).expect("peek") {
            Advance::SuspiciousGap { .. } => {}
            other => panic!("a gap above real data must not read as {other:?}"),
        }
    }

    #[test]
    fn the_shape_reads_the_header_row_and_skips_blanks() {
        let body = "Customer,,Amount\nAcme,x,100\n";
        let mut snap =
            CsvSnapshot::new("doc", body, 2, 1, vec!["A".into(), "B".into(), "C".into()]);
        let shape = snap.shape().expect("shape");
        assert_eq!(shape.columns.len(), 2, "the blank header must be skipped");
        assert_eq!(shape.columns[0].locator, "A");
        assert_eq!(shape.columns[0].label, "Customer");
        assert_eq!(shape.columns[1].locator, "C");
        assert_eq!(shape.columns[1].label, "Amount");
    }

    #[test]
    fn the_position_carries_no_content() {
        let snap = CsvSnapshot::new("doc", SHEET, 2, 1, vec!["A".into()]);
        let position = snap.position();
        assert_eq!(position.source_id, "doc");
        assert_eq!(position.row_key, "2");
    }
}

/// Fetch a document's CSV export through the user's signed-in browser.
///
/// ## Why the browser, and why that is not a workaround
///
/// The export URL is not publicly readable -- a session-less request returns
/// **401**. The browser holds the user's real Google session, so opening the URL
/// in it is the whole of the authentication story: no OAuth, no stored
/// credentials, nothing for this app to keep or leak.
///
/// Measured from a genuinely cold start -- every browser process killed, no tab,
/// no window -- and the export still returned real CSV rather than a login page.
/// The profile's saved sign-in survives having no window open, which is what
/// makes this usable for an unattended scan rather than only when the user
/// happens to have the sheet in front of them.
///
/// ## The file is removed, and only the one that appeared
///
/// The download lands in Downloads. This snapshots the folder first and deletes
/// exactly what is new, rather than "the newest .csv" -- a user's own download
/// arriving mid-scan must not be collateral. Tonight's probes, which had no
/// cleanup, left 63 export files behind; that is the mistake this avoids.
pub async fn fetch_export(
    desktop: &terminator::Desktop,
    doc_id: &str,
    gid: &str,
) -> Result<String, SourceError> {
    let body = fetch_export_blocking(doc_id, gid)?;
    close_export_window(desktop).await;
    Ok(body)
}

/// [`fetch_export`] without the window close, and without needing a runtime.
///
/// The download was always synchronous -- spawn the browser, poll the Downloads
/// folder -- and only closing the leftover window needed `async`, because the
/// locator API is async.
///
/// That async-ness was fatal for the one caller that matters most.
/// [`SourceReader`](super::SourceReader) is a **synchronous** trait, so a
/// per-record reader bridging to an async fetch had to hold a runtime and
/// `block_on` it. That works on the run's own `std::thread`, which has no
/// reactor, and panics outright anywhere already inside one:
///
/// ```text
/// Cannot start a runtime from within a runtime.
/// ```
///
/// The §4.3 preview reads the source from an async command, so that panic was
/// not a corner case -- it was every preview.
///
/// Dropping the close is safe because the case it cleans up after is already
/// covered defensively: [`crate::run::surfaces::is_export_url`] refuses to
/// match a document by a window sitting on its export URL, which is the actual
/// harm a leftover window does. The guard was written for exactly this and was
/// deliberately kept even when the close was added, on the grounds that the
/// close is best-effort and the guard is not.
///
/// What is genuinely given up is tidiness: on a **cold** browser, where the
/// export window has no sibling tab to fall back to, one "Untitled" window can
/// be left behind. With a browser already running the export tab closes itself.
pub fn fetch_export_blocking(doc_id: &str, gid: &str) -> Result<String, SourceError> {
    let unreachable = |m: String| SourceError::Unreachable(m);

    let downloads = dirs_downloads()
        .ok_or_else(|| unreachable("cannot locate the Downloads folder".into()))?;
    let before = csv_files(&downloads);

    let url =
        format!("https://docs.google.com/spreadsheets/d/{doc_id}/export?format=csv&gid={gid}");
    // NOT `cmd /C start`: the URL contains `&`, which cmd treats as a command
    // separator -- measured twice in this repo, once silently dropping the gid
    // so every export returned the default sheet while reporting success.
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Start-Process",
            "msedge",
            "-ArgumentList",
            &format!("'{url}'"),
        ])
        .status()
        .map_err(|e| unreachable(format!("could not open the export URL: {e}")))?;

    for _ in 0..45 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let now = csv_files(&downloads);
        if let Some(fresh) = now.iter().find(|p| !before.contains(p)) {
            let body = std::fs::read_to_string(fresh)
                .map_err(|e| unreachable(format!("could not read the export: {e}")))?;
            let _ = std::fs::remove_file(fresh);
            return Ok(body);
        }
    }
    Err(unreachable(
        "the export never downloaded. The browser may be signed out, or the document may not \
         be reachable by this account"
            .into(),
    ))
}

fn csv_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("csv"))
        .collect()
}

fn dirs_downloads() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE").map(|home| std::path::Path::new(&home).join("Downloads"))
}

/// Can this source be scanned from a CSV export?
///
/// Only when the source names a document and no sheet within it. The export URL
/// selects a sheet by **gid**, a number, while a template stores a sheet by
/// NAME -- and there is no way to map one to the other without opening the
/// document, which is the cost this exists to avoid.
///
/// So a qualified source (`<doc>!Sheet2`) returns `None` and the caller keeps
/// the per-cell reader. Exporting gid=0 and hoping would read a different sheet
/// than the workflow was built against, silently, and report a confident answer
/// about the wrong data.
pub fn exportable_doc_id(source_id: &str) -> Option<&str> {
    let (sheet, doc) = crate::capture::grid::split_sheet_ref(source_id);
    match sheet {
        Some(_) => None,
        None if doc.trim().is_empty() => None,
        None => Some(doc),
    }
}

/// Close the window left showing the export URL, if closing it is safe.
///
/// ## Why this is needed at all
///
/// The export is fetched by opening its URL in the browser. When a browser is
/// already running the export lands in a tab that closes itself once the
/// download completes, and nothing is left behind. When the browser is **cold**
/// -- the unattended case this reader exists for -- there is no other tab to
/// fall back to, so the window stays, titled "Untitled", parked on the export
/// URL. Measured: one leftover window per cold scan.
///
/// That is litter, and worse than litter: the export URL contains the document
/// id, so `run::surfaces::window_for` matched the dead window in preference to
/// the real document and a run would have read from a window with no
/// spreadsheet in it.
///
/// ## The edge case, and why it decides the behaviour
///
/// The export may be a TAB inside a window holding the user's own tabs.
/// Closing that window would take their work with it, to tidy up after
/// ourselves -- which is not a trade this is allowed to make.
///
/// So a window is closed only when it is showing an export URL **and** its
/// title does not report other pages behind it. A browser titled
/// "… and N more pages" is telling us it has tabs we would destroy, and it is
/// also exactly the case that does not need us: those windows already close
/// their own export tab.
///
/// When the window IS alone, closing it ends that browser process -- and that
/// is correct here rather than merely acceptable. The scan started that browser
/// itself, seconds earlier, purely to fetch a file. Leaving a browser running
/// that the user did not open, on a URL they did not visit, is the more
/// intrusive option.
///
/// Best-effort throughout. A scan that produced a correct answer must not fail
/// because a window would not close; the `window_for` guard covers what this
/// misses.
async fn close_export_window(desktop: &terminator::Desktop) {
    let Ok(windows) = desktop
        .locator("role:Window")
        .within(desktop.root())
        .all(Some(std::time::Duration::from_secs(5)), Some(3))
        .await
    else {
        return;
    };

    for w in windows {
        // TITLE FIRST, and this ordering is the whole cost of the function.
        //
        // `address_of` runs a locator with a 4s timeout, and a window with no
        // address bar burns all four seconds before returning nothing. Sweeping
        // every window that way was measured at **30.6s per export** against a
        // desktop where 8 of 11 windows had no address bar -- which is 23x the
        // fetch itself, and made per-record fetching impossible.
        //
        // The title is free and rules almost everything out: an export window
        // is a browser window, and the one this opens is titled "Untitled"
        // because its only tab is a download.
        let title = w.name().unwrap_or_default();
        let lower = title.to_lowercase();
        if !lower.contains("edge") && !lower.contains("chrome") {
            continue;
        }
        if lower.contains(" more page") || lower.contains(" more tab") {
            // Someone else's tabs live here. Leave it; the guard handles it.
            continue;
        }
        if address_of(desktop, &w).await.contains("/export?") {
            let _ = w.close();
        }
    }
}

/// The address bar's text for a window, if it has one.
///
/// Duplicated from `run::surfaces` rather than shared: that one is private, and
/// exporting it to reach across from the source layer would couple two modules
/// that otherwise know nothing about each other for four lines of locator.
async fn address_of(desktop: &terminator::Desktop, window: &terminator::UIElement) -> String {
    let Ok(bars) = desktop
        .locator("role:Edit|name:Address and search bar")
        .within(window.clone())
        .all(Some(std::time::Duration::from_secs(4)), None)
        .await
    else {
        return String::new();
    };
    for b in &bars {
        let t = b.text(0).unwrap_or_default();
        if !t.is_empty() {
            return t;
        }
    }
    String::new()
}

#[cfg(test)]
mod routing_tests {
    use super::exportable_doc_id;

    /// Which reader the run uses hangs entirely on this predicate, so the two
    /// shapes are pinned rather than assumed.
    ///
    /// A bare document id exports -- that is the two-document case the formula
    /// bar cannot serve. A sheet-qualified surface does NOT, because the export
    /// URL selects a sheet by gid and a template names one by NAME; exporting
    /// gid=0 and hoping would answer confidently about the wrong sheet. Those
    /// are also the one-document workflows that already worked through
    /// `SpreadsheetReader`, so they keep using it.
    #[test]
    fn a_bare_document_exports_and_a_qualified_one_does_not() {
        assert_eq!(
            exportable_doc_id("1ko7z65TnzI5suwvu3LGv8yoemmhOs5siSQe9KBB8NZs"),
            Some("1ko7z65TnzI5suwvu3LGv8yoemmhOs5siSQe9KBB8NZs")
        );
        assert_eq!(exportable_doc_id("1ko7z65TnzI5suwvu3LGv8yoemmhOs5siSQe9KBB8NZs!Sheet1"), None);
        assert_eq!(exportable_doc_id("doc!Sheet2"), None);
        assert_eq!(exportable_doc_id(""), None);
    }
}
