//! A [`SourceReader`] that re-exports the source once per record.
//!
//! ## It fetches synchronously, and that is the point
//!
//! An earlier version bridged the synchronous [`SourceReader`] trait to an
//! async fetch by holding a runtime and calling `block_on`. That works on the
//! run own `std::thread`, which has no reactor, and panics outright inside any
//! async caller -- which the §4.3 preview is:
//!
//! ```text
//! Cannot start a runtime from within a runtime.
//! ```
//!
//! So the fetch is [`fetch_export_blocking`], which needs no runtime. The only
//! async part was closing the leftover export window; that is dropped, and
//! [`crate::run::surfaces::is_export_url`] already refuses to match a document
//! by a window sitting on its export URL, which is the harm such a window
//! actually does.
//!
//! ## Why this exists rather than reusing [`CsvSnapshot`]
//!
//! [`CsvSnapshot`] parses one body and serves everything from it. That is right
//! for a scan -- one read-only question about one consistent moment -- and
//! wrong for a run, which interleaves reads with writes, pauses for §4.5
//! corrections, and can wait on a human for minutes. A snapshot taken at spawn
//! would let a run write values the source no longer holds.
//!
//! So the two differ in *when they fetch*, which is their entire substance:
//! this one fetches fresh for each record and throws the body away when the
//! reader moves on.
//!
//! ## Why the run's source cannot use the formula bar
//!
//! A Sheets formula bar exposes cell contents only after the page has been
//! **typed into** -- measured, and documented in
//! `docs/known-issues/the-formula-bar-only-reports-after-the-page-is-typed-into.md`.
//! `SpreadsheetWriter` never hits it because it types first and reads back
//! after. A source document is one nobody ever types into, so
//! `SpreadsheetReader` opened on a freshly loaded source refuses -- correctly,
//! since the element genuinely reports nothing.
//!
//! An export needs no window, no formula bar and no provocation. The scan
//! proved that by reading correctly with every browser process closed.
//!
//! ## The cost, measured rather than assumed
//!
//! ~2.12s per record, against ~1.95s for the two-column formula-bar read it
//! replaces -- and **flat in the number of columns** where the formula bar is
//! linear, so a five-column mapping costs the same 2.12s instead of ~4.9s.
//! Five consecutive exports left the titled-window count and the Downloads
//! count unmoved, so per-record fetching needs no window reuse.

use super::{Advance, FieldRef, SourceError, SourcePosition, SourceReader, SourceRecord, SourceShape};
use crate::source::csv_snapshot::{fetch_export_blocking, CsvSnapshot};

/// Re-fetches the source for each record it is asked about.
pub struct CsvLiveReader {
    /// The document to export. Bare id -- see [`super::csv_snapshot::exportable_doc_id`].
    doc_id: String,
    /// What positions are stamped with. May carry a sheet qualifier even when
    /// `doc_id` cannot, because a position is recorded against the surface the
    /// template names, not against the thing being fetched.
    source_id: String,
    gid: String,
    row: u64,
    header_row: u64,
    scan_columns: Vec<String>,
    /// The body most recently fetched, and the row it was fetched for.
    ///
    /// `peek` and `read` are both called for the same record, and re-fetching in
    /// each would double the cost for no extra freshness -- the two questions
    /// are asked at the same moment about the same row. So the fetch happens
    /// once per record and both are served from it.
    cached: Option<(u64, CsvSnapshot)>,
}

impl CsvLiveReader {
    pub fn new(
        doc_id: impl Into<String>,
        source_id: impl Into<String>,
        gid: impl Into<String>,
        first_row: u64,
        header_row: u64,
        scan_columns: Vec<String>,
    ) -> Result<Self, SourceError> {
        Ok(Self {
            doc_id: doc_id.into(),
            source_id: source_id.into(),
            gid: gid.into(),
            row: first_row.max(1),
            header_row,
            scan_columns,
            cached: None,
        })
    }

    /// Ensure the cached body was fetched for `row`, fetching if it was not.
    fn ensure(&mut self, row: u64) -> Result<&mut CsvSnapshot, SourceError> {
        let stale = match &self.cached {
            Some((cached_row, _)) => *cached_row != row,
            None => true,
        };
        if stale {
            // Synchronous by design. See `fetch_export_blocking` -- the async
            // variant forced a runtime into a sync trait method, which panics
            // inside any async caller, and the preview is one.
            let body = fetch_export_blocking(&self.doc_id, &self.gid)?;
            // `first_row` is the row being asked about, so the snapshot is
            // positioned exactly where this reader is.
            let snapshot = CsvSnapshot::new(
                self.source_id.clone(),
                &body,
                row,
                self.header_row,
                self.scan_columns.clone(),
            );
            self.cached = Some((row, snapshot));
        }
        Ok(&mut self
            .cached
            .as_mut()
            .expect("just populated")
            .1)
    }
}

impl SourceReader for CsvLiveReader {
    fn position(&self) -> SourcePosition {
        SourcePosition {
            source_id: self.source_id.clone(),
            row_key: self.row.to_string(),
        }
    }

    fn peek(&mut self, fields: &[FieldRef]) -> Result<Advance, SourceError> {
        let row = self.row;
        self.ensure(row)?.peek(fields)
    }

    fn read(&mut self, fields: &[FieldRef]) -> Result<SourceRecord, SourceError> {
        let row = self.row;
        self.ensure(row)?.read(fields)
    }

    fn advance(&mut self) -> Result<(), SourceError> {
        self.row += 1;
        // The body described the row just finished with. Dropping it here is
        // what makes the next record a fresh read rather than a snapshot.
        self.cached = None;
        Ok(())
    }

    fn shape(&mut self) -> Result<SourceShape, SourceError> {
        let row = self.row;
        self.ensure(row)?.shape()
    }
}
