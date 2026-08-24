//! Capture for spreadsheet-style grids, where the edited cell is not an element.
//!
//! ## Why this cannot reuse `TextFieldWatcher`
//!
//! `TextFieldWatcher` watches an element and reads its text **when the edit
//! ends** -- on focus change, on a trigger key, on session stop. That works
//! because an `<input>` still exists after Enter.
//!
//! A Google Sheets cell has no element at all. Measured against a live document:
//! the whole browser window is ~110 accessible nodes and the grid contributes
//! none of them. Typing creates a transient `ComboBox` overlay named after the
//! cell (`"D3"`), and **committing destroys it**. A read taken at flush time --
//! which is the only time the existing watcher reads -- lands after the editor
//! is recycled and returns `U+FEFF` attributed to a *different* cell. That is
//! not a hypothetical: it is the corruption seen in real recordings, where
//! payloads came back empty or as a stray invisible character and clicks and
//! types disagreed about which cell they belonged to.
//!
//! So the read model is inverted. Instead of *read at the end*, this samples
//! **while the editor is alive** and emits the last good sample when the edit
//! finishes. Everything else -- producing `ActionCandidate`s that go through
//! `CapturedStream::admit` and the exclusion gate -- is unchanged.
//!
//! ## Why it resolves focus itself
//!
//! The editor is created by *typing*, so no `Click` event ever names it, and
//! `focus_moved` is never called for it. Measured: `KeyboardEvent`s carry
//! `metadata.ui_element: None` for **every** key-down (0 of 15 in a driven
//! session), so the element cannot be recovered from the event stream either.
//! This therefore holds a `Desktop` and resolves the focused element itself, on
//! key-down. That is new machinery, not a tuning of existing machinery, and the
//! measurement above is why.
//!
//! ## Why keystrokes are the clock
//!
//! No event marks "the editor is about to be destroyed". The prototype used a
//! 25 ms polling loop; that is unnecessary here because key-downs already arrive
//! at exactly the moments the value can change. Sampling on key-down is
//! event-driven, costs one focused-element resolution per printable key, and
//! guarantees a sample exists from before the commit keystroke.
//!
//! ## What this does NOT do
//!
//! Replay. A captured Sheets edit records which cell was typed into and what was
//! typed, but nothing here makes that replayable -- there is no element for a
//! selector to resolve to. See the known-issues doc.

use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, Ordering};

use terminator::{Desktop, UIElement};

use super::stream::{ActionCandidate, ActionKind};

// ------------------------------------------------------------------ timing --
//
// Two relaxed atomic adds per key event. This exists because the cost of
// resolving the focused element on every key-down was flagged as suspected --
// the observation came from a machine in an unusually loaded state, with no
// user-facing symptom -- and "suspected" is not a number.
//
// Key-down and key-up are counted SEPARATELY because key-up sampling was added
// to close the one-keystroke lag, and "it roughly doubles the calls" is exactly
// the kind of assumption this counter exists to replace. Reported at session
// stop, so a real recording produces the real number.
static GRID_CALLS: AtomicU64 = AtomicU64::new(0);
static GRID_MICROS: AtomicU64 = AtomicU64::new(0);
static GRID_UP_CALLS: AtomicU64 = AtomicU64::new(0);
static GRID_UP_MICROS: AtomicU64 = AtomicU64::new(0);

// The two position reads that do NOT happen on a key event, and were therefore
// invisible to everything above.
//
// `observe_key` is what GRID_* wraps, so a `read_position` reached any other way
// contributed nothing to any number this module reported. Both of these run the
// same full walk a `Ctrl+C` does -- the Name Box scan, or up to 3000 nodes for
// element identity -- and a menu copy performed one in a real session on
// 2026-08-19 that appears in none of that session's figures.
//
// Kept apart rather than folded into one "off-key" total because they fire for
// different reasons and at different rates: the clipboard monitor polls at
// 200ms and fires on any copy anywhere, while a mark is one deliberate keypress.
// A single number would hide which one is costing anything.
static CLIP_CALLS: AtomicU64 = AtomicU64::new(0);
static CLIP_MICROS: AtomicU64 = AtomicU64::new(0);
static MARK_CALLS: AtomicU64 = AtomicU64::new(0);
static MARK_MICROS: AtomicU64 = AtomicU64::new(0);

/// Reading the clicked element's id, which is four cross-process UI Automation
/// property reads -- automation id, control type, name, class name -- hashed
/// together. Timed because it is NEW cost on the click path, added when the
/// source of a copy moved from focus to the last click, and this project has
/// already measured one read that turned out to cost 200ms a keystroke.
static CLICK_ID_CALLS: AtomicU64 = AtomicU64::new(0);
static CLICK_ID_MICROS: AtomicU64 = AtomicU64::new(0);

/// (calls, total microseconds) spent in `observe_key` since the last reset,
/// both directions together.
pub fn timing() -> (u64, u64) {
    let (down, up) = timing_split();
    (down.0 + up.0, down.1 + up.1)
}

/// The same numbers split as `(key_down, key_up)`, which is what says whether
/// key-up sampling cost what it was expected to cost.
pub fn timing_split() -> ((u64, u64), (u64, u64)) {
    (
        (
            GRID_CALLS.load(Ordering::Relaxed),
            GRID_MICROS.load(Ordering::Relaxed),
        ),
        (
            GRID_UP_CALLS.load(Ordering::Relaxed),
            GRID_UP_MICROS.load(Ordering::Relaxed),
        ),
    )
}

/// The position reads taken OFF the key path, as
/// `((clipboard_calls, clipboard_us), (mark_calls, mark_us))`.
///
/// Neither appears in [`timing_split`], which wraps `observe_key` only.
pub fn off_key_timing() -> ((u64, u64), (u64, u64)) {
    (
        (
            CLIP_CALLS.load(Ordering::Relaxed),
            CLIP_MICROS.load(Ordering::Relaxed),
        ),
        (
            MARK_CALLS.load(Ordering::Relaxed),
            MARK_MICROS.load(Ordering::Relaxed),
        ),
    )
}

/// Zero the counters, so one run's numbers are its own.
/// (calls, microseconds) spent reading clicked element ids.
pub fn click_id_timing() -> (u64, u64) {
    (
        CLICK_ID_CALLS.load(Ordering::Relaxed),
        CLICK_ID_MICROS.load(Ordering::Relaxed),
    )
}

/// Record one click-id read. Called from the pump, where the element is.
pub fn note_click_id_cost(micros: u64) {
    CLICK_ID_CALLS.fetch_add(1, Ordering::Relaxed);
    CLICK_ID_MICROS.fetch_add(micros, Ordering::Relaxed);
}

pub fn reset_timing() {
    CLICK_ID_CALLS.store(0, Ordering::Relaxed);
    CLICK_ID_MICROS.store(0, Ordering::Relaxed);
    GRID_CALLS.store(0, Ordering::Relaxed);
    GRID_MICROS.store(0, Ordering::Relaxed);
    GRID_UP_CALLS.store(0, Ordering::Relaxed);
    GRID_UP_MICROS.store(0, Ordering::Relaxed);
    CLIP_CALLS.store(0, Ordering::Relaxed);
    CLIP_MICROS.store(0, Ordering::Relaxed);
    MARK_CALLS.store(0, Ordering::Relaxed);
    MARK_MICROS.store(0, Ordering::Relaxed);
}

/// Does this name look like a spreadsheet cell reference (`A1`, `BC12`)?
///
/// Deliberately strict. The editor overlay is a `ComboBox`, and so are ordinary
/// dropdowns; the cell-reference shape is what separates them. A false positive
/// would attach a `Type` action to a menu.
/// The letter run is capped at 3 because a spreadsheet column reference cannot
/// be longer -- Sheets tops out at `ZZZ`. Without that cap `"Sheet1"` parses as
/// a cell, and `"Sheet1"` is a real element name sitting in the same window as
/// the editor. The unit test below pins it.
pub fn looks_like_cell_ref(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 2 || s.len() > 10 {
        return false;
    }
    let mut letters = 0usize;
    let mut digits = 0usize;
    for c in s.chars() {
        if c.is_ascii_alphabetic() && digits == 0 {
            letters += 1;
        } else if c.is_ascii_digit() {
            digits += 1;
        } else {
            return false;
        }
    }
    (1..=3).contains(&letters) && (1..=7).contains(&digits)
}

/// Split a possibly-qualified reference into its sheet and cell parts.
///
/// `"Sheet2!B2"` -> `(Some("Sheet2"), "B2")`, `"B2"` -> `(None, "B2")`.
///
/// The qualified form is what a recording carries once the sheet is known, and
/// what replay hands to the Name Box. Keeping it as one string means the sheet
/// travels the pipeline in `element_name`, on exactly the path the bare cell
/// reference already took -- no new field in `ActionCandidate`, no change to
/// compile or the stored payload shape.
pub fn split_sheet_ref(s: &str) -> (Option<&str>, &str) {
    match s.split_once('!') {
        Some((sheet, cell)) if !sheet.is_empty() && !cell.is_empty() => (Some(sheet), cell),
        _ => (None, s),
    }
}

/// Does this name look like a sheet TAB, as opposed to anything else clickable?
///
/// Measured shape: a human clicking the tab is captured as
/// `role="text" name="Sheet1"`. The `Button` that owns it is also accepted,
/// since the recorder attributing the click one level up is a plausible
/// variation rather than something to depend on either way.
///
/// **Known limit, and it is not small.** The name test is the default Sheets
/// naming, `Sheet` followed by digits. A sheet the user renamed to `Data` is
/// captured as `role="text" name="Data"`, which is indistinguishable from a
/// click on any other text. There is no way to tell them apart from the event
/// alone, and the accessibility tree carries no sheet-selection state to
/// cross-check against -- measured, see
/// docs/known-issues/complex-web-grid-capture-unreliable.md. So renamed sheets
/// are not tracked, and a recording that switches to one is stamped with
/// whatever sheet was last recognised, or nothing.
pub fn looks_like_sheet_tab(role: &str, name: &str) -> bool {
    let n = name.trim();
    let role_ok = role.eq_ignore_ascii_case("text") || role.eq_ignore_ascii_case("button");
    role_ok
        && n.len() > 5
        && n.starts_with("Sheet")
        && n[5..].chars().all(|c| c.is_ascii_digit())
}

/// Is this focused element the transient cell editor?
///
/// The single decision that keeps this watcher out of every non-grid
/// application. Extracted as a pure function so the property can be tested
/// directly: a `Document` surface (Notepad), an `Edit` (`<input>`,
/// `<textarea>`), and a `ComboBox` that is an ordinary dropdown must all be
/// ignored, or capture would start attaching spreadsheet actions to them.
///
/// Accepts a sheet-qualified reference as well as a bare one, so a step
/// recorded as `Sheet2!B2` still takes the grid path at replay.
pub fn is_cell_editor(role: &str, name: &str) -> bool {
    role == "ComboBox" && looks_like_cell_ref(split_sheet_ref(name).1)
}

/// Sheets seeds its hidden editor with `U+FEFF`, and the value carries a
/// trailing newline while the editor is open. Neither belongs in a payload.
///
/// The `U+FEFF` is what a real recording reported as "`19` plus a stray
/// invisible character".
pub fn clean_cell_text(raw: &str) -> String {
    raw.replace('\u{feff}', "")
        .trim_end_matches('\n')
        .to_string()
}

/// A cell edit in progress, as last observed.
struct GridEdit {
    cell: String,
    /// Last non-empty text seen in the editor. This, not a fresh read, is what
    /// gets emitted -- by the time the edit ends the editor is gone.
    text: String,
    identifiers: Vec<String>,
    process_name: Option<String>,
    /// The cell editor's rectangle, read when the edit STARTED.
    ///
    /// Not read at emit, for the same reason `text` is not re-read there: by the
    /// time an edit ends the editor is gone, and re-reading it is exactly the
    /// mistake that produced `U+FEFF` payloads.
    bounds: Option<(f64, f64, f64, f64)>,
    keystrokes: u32,
    started_ms: u64,
}

/// Where a value came from, paired with where it went.
///
/// **Transient by design.** §3 of the templated-workflows design permits source
/// positions only during detection and during a live run, never as a durable
/// position-plus-content pair. So this is a side channel on `CaptureReport`,
/// consumed at stop and dropped -- it never enters the action stream, is never
/// compiled, and is never written to the database. It also carries no VALUE:
/// what was copied is not read, only where it was copied from and to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLink {
    /// Order within the session, so detection can collapse corrections by
    /// keeping the last write to a destination.
    pub seq: u64,
    pub source_document: String,
    pub source_cell: String,
    pub destination_document: String,
    pub destination_cell: String,
}

/// What an explicit source mark managed to read.
///
/// Carries the **route** and the position, and deliberately **not the
/// document**. A cell reference and an `el/<ordinal>/<label>` are positions, the
/// same class of thing `SourceLink` already holds. A document is a URL *or a
/// window title*, and a browser title routinely contains page content --
/// `"Invoice #1234 for Acme - Google Docs"`. `SourceLink` keeps that transient
/// by design; a log file on disk is not transient, so it does not go there.
///
/// The route is what makes a log line readable: it says which of the three
/// surfaces a mark landed on without naming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkOutcome {
    /// Read through the Name Box. A spreadsheet.
    Spreadsheet { cell: String },
    /// Read through element identity, on a page with no Name Box. Note that
    /// this includes surfaces resolved only by the window-title fallback in
    /// `page_identity`, where the document half is weak even though the
    /// position is real.
    Element { reference: String },
    /// Nothing could be read. Common, and not an error.
    Unavailable,
}

impl MarkOutcome {
    pub fn armed(&self) -> bool {
        !matches!(self, MarkOutcome::Unavailable)
    }

    /// One line for the log, carrying route and position and no identity.
    pub fn describe(&self) -> String {
        match self {
            MarkOutcome::Spreadsheet { cell } => {
                format!("armed via Name Box, cell {cell}")
            }
            MarkOutcome::Element { reference } => {
                format!("armed via element identity, {reference}")
            }
            MarkOutcome::Unavailable => "NOT armed -- no position could be read \
                 from the focused surface"
                .to_string(),
        }
    }
}

/// `C` and `V`. Copy and paste are the only clipboard keys this tracks; cut
/// (`X`) is deliberately excluded, because a cut REMOVES the source row and a
/// pattern inferred from vanishing sources would replay against data that is no
/// longer there.
const KEY_C: u32 = 0x43;
const KEY_V: u32 = 0x56;

/// How long a `Ctrl+C` keeps the clipboard monitor from reading a position of
/// its own. The monitor polls every 200ms and then resolves the focused element
/// with a 200ms timeout, so its report of a keystroke copy can trail the
/// keystroke by roughly 400ms before pump latency. One second covers that with
/// room to spare, and the cost of covering it is stated on
/// [`GridCellWatcher::clipboard_needs_own_read`].
const COPY_KEY_GRACE_MS: u64 = 1000;

/// How long a click stays eligible to be the source of a copy.
///
/// Modelled on [`COPY_KEY_GRACE_MS`], and chosen rather than measured -- the one
/// number in this mechanism that is a judgement. It has to span a deliberate
/// select-then-read-then-copy, which is slower than the ~800ms an automated
/// double-click-then-`Ctrl+C` takes, while staying far short of the gap that
/// separates one record's work from the next. Five seconds is the first
/// calibration, not a finding.
///
/// The failure it exists to prevent is specific: without it, a `Ctrl+C` with no
/// click of its own would silently adopt whatever was clicked minutes earlier
/// and report it as the source with full confidence.
const CLICK_SOURCE_GRACE_MS: u64 = 5000;

/// Follows the transient cell editor and produces `Type` candidates.
#[derive(Default)]
pub struct GridCellWatcher {
    desktop: Option<Desktop>,
    current: Option<GridEdit>,
    identifiers: Vec<String>,
    process_name: Option<String>,
    /// The sheet the user last switched to by clicking its tab, if any. Session
    /// -scoped: it lives as long as the watcher, which is one capture session.
    current_sheet: Option<String>,
    /// Where the last Ctrl+C happened: (document, cell). Held until the next
    /// copy replaces it, so one copy can legitimately feed several pastes.
    pending_source: Option<(String, String)>,
    /// When the `Ctrl+C` hook last read a position, so the clipboard monitor
    /// does not overwrite it with a worse one. See [`GridCellWatcher::
    /// clipboard_needs_own_read`].
    last_copy_key_ms: Option<u64>,
    /// The element id of the most recent click, and when it happened.
    ///
    /// This is what the source of a copy is resolved from on a page. It is NOT
    /// the focused element: selecting text in a browser does not move focus, so
    /// `Desktop::focused_element()` returns the same Document for every copy in
    /// a session and reports nine different values as one source. Measured in
    /// session record-fe88fb0d; see
    /// `docs/known-issues/the-source-of-a-copy-is-read-from-focus-which-a-web-selection-never-moves.md`.
    last_click: Option<(String, u64)>,
    /// Completed source -> destination pairs. Drained at stop.
    links: Vec<SourceLink>,
}

impl GridCellWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember which app is in play, from the most recent click.
    ///
    /// Keyboard events carry no element and therefore no app identity, so the
    /// exclusion gate would otherwise have nothing to match on.
    pub fn note_context(&mut self, identifiers: Vec<String>, process_name: Option<String>) {
        if !identifiers.is_empty() {
            self.identifiers = identifiers;
        }
        if process_name.is_some() {
            self.process_name = process_name;
        }
    }

    /// A click happened. Notes the app context, and tracks the active sheet.
    ///
    /// The sheet half is the only way capture can know which sheet an edit
    /// belongs to. Google Sheets exposes **no** active-sheet state: a
    /// whole-window diff across a real sheet switch returns zero differing
    /// elements, so there is nothing to read at edit time. What can be observed
    /// is the *event* -- the user clicking a tab -- and that is what this
    /// records. See docs/known-issues/complex-web-grid-capture-unreliable.md.
    ///
    /// Consequence worth stating at the call site: a recording that merely
    /// STARTS on a non-default sheet contains no tab click, so `current_sheet`
    /// stays `None` and its edits are recorded bare, exactly as before this
    /// existed. That case is not fixed by this and cannot be.
    ///
    /// It also arms the source of the next copy. `element_id` is the clicked
    /// element's own id, and it is remembered because on a page the click is
    /// what says which value the user is acting on -- focus does not move when
    /// text is selected. An id that is empty arms nothing rather than arming a
    /// blank, so a click on something the tree cannot name leaves the previous
    /// source in place to age out on its own.
    pub fn note_click(
        &mut self,
        role: &str,
        name: Option<&str>,
        element_id: Option<&str>,
        timestamp_ms: u64,
        identifiers: Vec<String>,
        process_name: Option<String>,
    ) {
        if let Some(id) = element_id {
            if !id.trim().is_empty() {
                self.last_click = Some((id.trim().to_string(), timestamp_ms));
            }
        }
        if let Some(n) = name {
            if looks_like_sheet_tab(role, n) {
                // An in-flight edit belongs to the sheet it started on, so it is
                // emitted before the switch takes effect rather than being
                // restamped with the new one.
                self.current_sheet = Some(n.trim().to_string());
            }
        }
        self.note_context(identifiers, process_name);
    }

    /// The sheet edits are currently being attributed to, if one is known.
    pub fn tracked_sheet(&self) -> Option<&str> {
        self.current_sheet.as_deref()
    }

    /// Everything the session paired, drained.
    pub fn take_links(&mut self) -> Vec<SourceLink> {
        std::mem::take(&mut self.links)
    }

    /// The same links WITHOUT draining, for reporting at stop.
    ///
    /// Separate from [`Self::take_links`] because draining is load-bearing --
    /// the watcher must not hand the same positions to a second reader -- and a
    /// diagnostic must not be able to consume them by accident.
    pub fn peek_links(&self) -> Vec<SourceLink> {
        self.links.clone()
    }

    /// A clipboard key was pressed and a position read was attempted.
    ///
    /// **A copy whose position could not be read DISARMS the source.** That is
    /// the whole point of this function existing, and it is the fix for a
    /// measured defect: `pending_source` was only ever assigned on a successful
    /// read and never cleared, so a `Ctrl+C` whose read failed left the
    /// PREVIOUS copy's position armed and the next paste paired with it.
    ///
    /// Session record-f15e933d (2026-08-21) is what that looks like from the
    /// outside. The user copied Free/Go/Plus and $0/$8/$20 into three
    /// spreadsheet rows -- plainly distinct sources -- and detection returned
    /// `SourceDidNotAdvance`, because enough destinations had been paired with
    /// one stale source that `prove_advance` saw a repeat.
    ///
    /// The principle was already written down three lines from the bug: *a
    /// paste with no source records no link, which §4.1 already excludes -- a
    /// wrong source it would not*. This applies it.
    ///
    /// A failed **paste** read is deliberately different: it disarms nothing,
    /// because one copy may legitimately feed several pastes and a destination
    /// that could not be read is not evidence about the source.
    fn note_clipboard_key(
        &mut self,
        key_code: u32,
        position: Option<(String, String)>,
        seq: u64,
    ) {
        // Every copy reports what it resolved to, as it happens.
        //
        // The stop-time report can only show sources that reached a PASTE, so a
        // copy that armed nothing -- or armed the same thing as the last one --
        // was invisible until a pair existed to expose it. That is a slow way to
        // learn that source resolution is broken, and it is how a session got as
        // far as nine pastes sharing one source before anyone could see it.
        //
        // The document is deliberately left out, here as at stop: a URL can
        // carry content in its query string, and §3 does not bend for a
        // diagnostic.
        if key_code == KEY_C {
            match position.as_ref() {
                Some((_, reference)) => eprintln!("[paradigm] copy source: {reference}"),
                None => eprintln!("[paradigm] copy source: (none -- declined)"),
            }
        }
        match position {
            Some(position) => self.pair_clipboard(key_code, position, seq),
            None if key_code == KEY_C => self.pending_source = None,
            None => {}
        }
    }

    /// Record a copy, or pair a paste with the copy that preceded it.
    ///
    /// Separated from the reading of the position so the PAIRING -- which is
    /// where the correctness is -- can be tested without a live spreadsheet.
    /// `position` is `(document, cell)`, already read.
    ///
    /// A paste with no preceding copy produces nothing. That is the honest
    /// outcome rather than a guess: the value came from somewhere this session
    /// did not see, and inventing a source would be worse than recording none.
    fn pair_clipboard(&mut self, key_code: u32, position: (String, String), seq: u64) {
        match key_code {
            KEY_C => self.pending_source = Some(position),
            KEY_V => {
                if let Some((source_document, source_cell)) = self.pending_source.clone() {
                    self.links.push(SourceLink {
                        seq,
                        source_document,
                        source_cell,
                        destination_document: position.0,
                        destination_cell: position.1,
                    });
                }
            }
            _ => {}
        }
    }

    /// Whether a clipboard change should read a position of its own.
    ///
    /// `Ctrl+C` fires **both** paths: the keystroke hook, and ~200ms later the
    /// clipboard monitor noticing the content changed. Only the first is worth
    /// having. The keystroke hook reads the position *synchronously, at the
    /// keypress*; the monitor polls on a 200ms timer, so by the time it reports,
    /// the user may already be moving to the destination and the focused element
    /// is whatever they moved to. Letting the late one overwrite the early one
    /// would make every `Ctrl+C` copy *worse* than before the monitor existed.
    ///
    /// So the monitor's job is strictly to cover gestures the keystroke hook
    /// cannot see -- menu Copy, a right-click, a toolbar button -- and it stands
    /// down whenever a copy keystroke has just been handled.
    ///
    /// The window is generous on purpose. Under it, a second copy made by menu
    /// within the window is missed; over it, a plain `Ctrl+C` gets its accurate
    /// position replaced by a stale one. Missing a source is recoverable -- the
    /// paste simply records no link, which §4.1 already treats as an
    /// observation to exclude. A *wrong* source is not: it puts a fabricated
    /// example into a Rule-of-3 count.
    /// The click eligible to be the source of a copy happening now, if any.
    ///
    /// Split out for the same reason [`Self::clipboard_needs_own_read`] is: the
    /// decision is a rule about time, it decides whether a link exists at all,
    /// and a rule that important should be testable without a live desktop.
    ///
    /// Returning `None` is the correct, common answer, not a failure. It means
    /// this copy gets no source rather than an unrelated one.
    fn click_source_id(&self, timestamp_ms: u64) -> Option<&str> {
        let (id, click_ms) = self.last_click.as_ref()?;
        (timestamp_ms.saturating_sub(*click_ms) <= CLICK_SOURCE_GRACE_MS).then_some(id.as_str())
    }

    fn clipboard_needs_own_read(&self, timestamp_ms: u64) -> bool {
        match self.last_copy_key_ms {
            Some(key_ms) => timestamp_ms.saturating_sub(key_ms) > COPY_KEY_GRACE_MS,
            None => true,
        }
    }

    /// A copy happened by some gesture other than `Ctrl+C`.
    ///
    /// **Carries no content, and cannot.** `max_clipboard_content_length` is set
    /// to 0 in `capture::CaptureSession::start_session`, so the event this
    /// responds to holds an empty string. This reads a *position* and nothing
    /// else, exactly as the keystroke path does.
    ///
    /// The position is up to ~200ms stale -- the monitor's poll interval -- and
    /// that is the honest cost of covering gestures with no keystroke. It is a
    /// worse reading than the keystroke path's, which is why
    /// [`Self::clipboard_needs_own_read`] keeps it out of the way when a real
    /// keystroke was seen.
    pub fn note_clipboard_copy(&mut self, timestamp_ms: u64) {
        // Counted only when a read actually happens: a suppressed clipboard
        // event costs nothing, and counting it would dilute the mean with zeros
        // and hide what a real one costs.
        if !self.clipboard_needs_own_read(timestamp_ms) {
            return;
        }
        let started = std::time::Instant::now();
        let position = self.read_position(timestamp_ms);
        CLIP_CALLS.fetch_add(1, Ordering::Relaxed);
        CLIP_MICROS.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        // Same rule as the keystroke path: a copy whose position could not be
        // read disarms the source rather than leaving a stale one armed.
        self.note_clipboard_key(KEY_C, position, timestamp_ms);
    }

    /// The user said, explicitly, that this is a source value.
    ///
    /// The general marker. Every other route into `pending_source` is
    /// copy-shaped -- a `Ctrl+C`, or the clipboard changing -- which means a
    /// workflow with no copying in it can mark nothing, however repetitive it
    /// is. Reading a value off a page and typing it somewhere else is a real
    /// workflow and produced no source link at all before this existed.
    ///
    /// Returns whether a position could actually be read. **`false` is common
    /// and is not an error**: `read_position` resolves a spreadsheet through the
    /// Name Box and any other page through element identity, and a surface that
    /// offers neither -- a PDF viewer, a native app with a shallow tree -- has
    /// no position to give. The caller is expected to tell the user, because a
    /// marker that silently does nothing is worse than no marker.
    ///
    /// Sets `last_copy_key_ms` for the same reason the `Ctrl+C` path does: an
    /// explicit mark is at least as authoritative as an inferred copy, and must
    /// not be overwritten by a clipboard event arriving behind it.
    pub fn note_marked_source(&mut self, timestamp_ms: u64) -> MarkOutcome {
        self.last_copy_key_ms = Some(timestamp_ms);
        // Timed for the same reason the clipboard path is: this walk is the
        // full one, and it happens while the user waits for the mark to take.
        let started = std::time::Instant::now();
        let position = self.read_position(timestamp_ms);
        MARK_CALLS.fetch_add(1, Ordering::Relaxed);
        MARK_MICROS.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        match position {
            Some((document, reference)) => {
                let outcome = if decode_element_ref(&reference).is_some() {
                    MarkOutcome::Element {
                        reference: reference.clone(),
                    }
                } else {
                    MarkOutcome::Spreadsheet {
                        cell: reference.clone(),
                    }
                };
                self.note_clipboard_key(KEY_C, Some((document, reference)), timestamp_ms);
                outcome
            }
            None => {
                // Same rule again. A mark the user deliberately made, on a
                // surface that yields no position, must not silently leave the
                // PREVIOUS source armed -- that would pair the next paste with
                // something the user did not mark, which is worse than the
                // honest `Unavailable` they are told about.
                self.note_clipboard_key(KEY_C, None, timestamp_ms);
                MarkOutcome::Unavailable
            }
        }
    }

    /// The focused window's document id and selected cell, read synchronously.
    ///
    /// Measured, and the reason this is a small change rather than a
    /// restructuring: a sync walk from the focused element finds the Name Box in
    /// ~50ms over 67 nodes, while the async locator takes ~450ms. `observe_grid`
    /// holds a `std::sync::Mutex` inside an async pump, so an await here would
    /// have meant taking the pump apart; it is not needed.
    ///
    /// Both facts come from one walk. The document id is required because two
    /// blank spreadsheets share the window title "Untitled spreadsheet" -- the
    /// same ambiguity that bit replay earlier -- so the title cannot tell a
    /// source from a destination and the address bar's `/d/<id>/` can.
    fn read_position(&mut self, timestamp_ms: u64) -> Option<(String, String)> {
        if self.desktop.is_none() {
            self.desktop = Desktop::new_default().ok();
        }
        let focused = self.desktop.as_ref()?.focused_element().ok()?;

        // Up to the window: the Name Box and the address bar are siblings of
        // the focused element's subtree, not ancestors of it.
        let mut root = focused.clone();
        for _ in 0..12 {
            match root.parent() {
                Ok(Some(parent)) => {
                    let reached_window = parent.role() == "Window";
                    root = parent;
                    if reached_window {
                        break;
                    }
                }
                _ => break,
            }
        }

        let mut budget = 3000usize;
        let mut scan = WindowScan::default();
        collect_position(&root, 0, &mut budget, &mut scan);
        let scan_url = scan.url.clone();

        match scan.decide() {
            PositionRead::Spreadsheet { document, cell } => Some((document, cell)),
            // A spreadsheet that would not read, or a walk that saw too little
            // to say. Both return nothing, which is what this did before the
            // element-identity path existed. Element identity cannot address a
            // canvas grid -- it has no per-cell elements to address -- so
            // reaching for it here yields the same ordinal for every cell and
            // silently collapses distinct destinations into one.
            PositionRead::SpreadsheetUnreadable | PositionRead::Inconclusive => None,
            // A page that was never a spreadsheet. Everything above is
            // untouched: this runs only where the Name Box was genuinely absent.
            PositionRead::NotASpreadsheet => {
                // The page identity comes out of the walk that already
                // happened, not a second one. Falls back to the window title
                // exactly as `page_identity` did.
                let page = scan_url.or_else(|| {
                    let title = root.name().unwrap_or_default();
                    (!title.trim().is_empty()).then_some(title)
                })?;
                self.read_element_position(&root, timestamp_ms, page)
            }
        }
    }

    /// A position for a page that has no Name Box, expressed generally.
    ///
    /// The general rule lives in [`crate::identity::tree`] and is shared with
    /// everything else that reconstructs records; this only gathers the tree and
    /// asks where the user's element sits.
    ///
    /// **That element is the last CLICK, not the focused element**, and the
    /// difference is the whole of this function's correctness. Selecting text on
    /// a page does not move focus -- measured directly: three double-click
    /// selections at three positions, each followed by `Ctrl+C`, produced *one*
    /// focused element across 48 samples, the Document. Resolving from focus
    /// therefore returned the same reference for every copy in a session and
    /// reported nine distinct values as one source (record-fe88fb0d).
    ///
    /// **It declines rather than guessing, in four separate ways**, and each is
    /// a case where the old code returned a confident wrong answer:
    ///
    /// 1. no click at all this session -- a keyboard-only selection;
    /// 2. a click older than [`CLICK_SOURCE_GRACE_MS`];
    /// 3. a click whose id is absent from THIS window's node list, which is what
    ///    a click in the destination spreadsheet followed by a copy on the page
    ///    looks like;
    /// 4. a click on a STRUCTURAL element -- [`crate::identity::tree::locate`]
    ///    refuses those, because a label is not a position. The three identical
    ///    `/ month` lines on the pricing page are exactly this, so the element
    ///    the earlier hypothesis suspected does decline, just not for the
    ///    reason that hypothesis proposed.
    ///
    /// Declining costs a link; §4.1 already treats a paste with no source as an
    /// observation to exclude. A wrong link cannot be excluded, because nothing
    /// downstream can tell it from a real one.
    ///
    /// The recency check runs BEFORE the walk, so a copy that is going to
    /// decline no longer pays for three traversals to find that out --
    /// incidentally relieving some of what
    /// `position-reads-dominate-an-ordinary-copy-paste-session.md` measures.
    ///
    /// **Carries no content.** The returned pair is a page identity and a
    /// `record ordinal + field label` -- the same kind of information a cell
    /// reference carries. The value the user copied is never read, which is the
    /// invariant `a_link_carries_positions_and_never_a_value` enforces.
    ///
    /// The ordinal is a position *within this recording*, and that is all
    /// detection needs: it has to know the examples referred to DIFFERENT
    /// records, not which records they were. A durable identity is a separate
    /// question, resolved against the live page at run time, where a Tier 1 key
    /// or a user-designated Tier 2 field is available. Storing an ordinal as
    /// though it were an identity would be the silent-wrong-target failure this
    /// project keeps finding.
    fn read_element_position(
        &mut self,
        root: &UIElement,
        timestamp_ms: u64,
        page: String,
    ) -> Option<(String, String)> {
        // Cases 1 and 2, both before any walk. A copy with nothing recent behind
        // it has no source, and saying so costs one comparison.
        let click_id = self.click_source_id(timestamp_ms)?.to_string();

        let mut nodes = Vec::new();
        let mut budget = 3000usize;
        let mut timed_out = false;
        let deadline = Instant::now() + Duration::from_millis(WALK_TIME_BUDGET_MS as u64);
        collect_nodes(root, &mut budget, &deadline, &mut timed_out, &mut nodes);

        // Case 5: the page was too large to read inside the time budget.
        //
        // **A partial node list must never be used**, however much of it there
        // is. `identity::tree` separates structural from content by
        // multiplicity, so a tree cut short reports elements as occurring once
        // that occur many times, reclassifies them as content, and yields a
        // position that is wrong rather than absent.
        //
        // Declining here is what stops a large page destroying the whole
        // recording: the walk gives up after 400ms instead of blocking the pump
        // for four seconds while the recorder's broadcast channel overflows. See
        // `docs/known-issues/a-large-page-starves-the-pump-and-loses-half-the-recording.md`.
        if timed_out {
            return None;
        }

        // Cases 3 and 4, both of them `locate` declining: an id this window does
        // not contain is not in `nodes`, and a structural element is refused by
        // the shared rule rather than by anything written here.
        let located = crate::identity::tree::locate(&nodes, &click_id)?;
        Some((page, encode_element_ref(located.record, located.label.as_deref())))
    }

    /// A key event. Samples the editor, and emits when an edit finishes.
    ///
    /// Called for key-**down** and key-**up** alike, and the difference matters.
    /// A key-down sample is taken before the OS has processed that key, so it
    /// shows the editor as it was one keystroke ago; the character just typed is
    /// only visible once the key comes back up. Sampling both ways is what keeps
    /// the last character from being lost when an edit ends without a trigger
    /// key -- by a window switch, by a click into another cell, or by the
    /// session stopping. Measured: a value typed and then abandoned for another
    /// window was recorded one character short, every time.
    ///
    /// Only a key-down can COMMIT. A key-up folds in what it sees and nothing
    /// more, so a released Enter cannot end an edit its press already ended.
    pub fn observe_key(
        &mut self,
        key_code: u32,
        is_key_down: bool,
        ctrl_pressed: bool,
        alt_pressed: bool,
        timestamp_ms: u64,
    ) -> Option<ActionCandidate> {
        let started = std::time::Instant::now();
        let out = if is_key_down {
            // Clipboard first: a copy has no editor to sample, and a paste's
            // destination must be read BEFORE the sampling below can disturb
            // anything. Only ever runs on Ctrl+C / Ctrl+V, so the ~50ms walk it
            // costs is not on the typing path -- and never on key-up, so the
            // walk still happens once per clipboard action, not twice.
            if ctrl_pressed && (key_code == KEY_C || key_code == KEY_V) {
                if key_code == KEY_C {
                    // Recorded whether or not the read below succeeds. The
                    // point is that a copy KEYSTROKE happened and was handled
                    // here; if its position could not be read, the clipboard
                    // monitor reading a later, staler one is not an improvement.
                    // A paste with no source records no link, which §4.1
                    // already excludes -- a wrong source it would not.
                    self.last_copy_key_ms = Some(timestamp_ms);
                }
                let position = self.read_position(timestamp_ms);
                self.note_clipboard_key(key_code, position, timestamp_ms);
            }
            self.observe_key_inner(key_code, alt_pressed, timestamp_ms)
        } else {
            self.observe_key_up(timestamp_ms)
        };
        if is_key_down {
            GRID_CALLS.fetch_add(1, Ordering::Relaxed);
            GRID_MICROS.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        } else {
            GRID_UP_CALLS.fetch_add(1, Ordering::Relaxed);
            GRID_UP_MICROS.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
        out
    }

    fn observe_key_inner(
        &mut self,
        key_code: u32,
        alt_pressed: bool,
        timestamp_ms: u64,
    ) -> Option<ActionCandidate> {
        // Sample first, exactly as before. The key-up sample means this is no
        // longer the only chance to see the last character, but a click into
        // another cell still shows up here and nowhere else.
        if let Absorbed::Switched(finished) = self.absorb_sample(timestamp_ms, true) {
            return finished;
        }

        if is_trigger_key(key_code, alt_pressed) {
            return self.emit(timestamp_ms);
        }
        None
    }

    /// A key came back up: the character it produced is now in the editor.
    ///
    /// Folds that in and nothing else. A key-up never commits, so an edit ends
    /// only where it ended before this existed.
    fn observe_key_up(&mut self, timestamp_ms: u64) -> Option<ActionCandidate> {
        match self.absorb_sample(timestamp_ms, false) {
            Absorbed::Switched(finished) => finished,
            Absorbed::Continued | Absorbed::Nothing => None,
        }
    }

    /// Take a fresh sample and fold it into the edit in flight.
    ///
    /// `counts_as_keystroke` is false for key-up, so the keystroke count in the
    /// emitted detail keeps meaning "keys the user pressed" rather than doubling
    /// now that both directions sample.
    ///
    /// The `is_cell_editor` gate inside `sample` is what keeps this from
    /// inventing an edit out of a cell the user only moved through: with no
    /// editor overlay open there is nothing to sample and this does nothing.
    fn absorb_sample(&mut self, timestamp_ms: u64, counts_as_keystroke: bool) -> Absorbed {
        let Some((cell, text, el)) = self.sample() else {
            return Absorbed::Nothing;
        };
        match self.current.as_mut() {
            Some(edit) if edit.cell == cell => {
                // The common path, and the reason identity is not resolved
                // above: this branch does not use it. Every keystroke after the
                // first in a cell lands here.
                edit.text = text;
                if counts_as_keystroke {
                    edit.keystrokes += 1;
                }
                Absorbed::Continued
            }
            Some(_) => {
                // Moved to a different cell without a trigger key.
                let identifiers = self.identity_for(&el);
                let finished = self.emit(timestamp_ms);
                self.current = Some(GridEdit {
                    bounds: super::bounds_of(Some(&el)),
                    cell,
                    text,
                    identifiers,
                    process_name: self.process_name.clone(),
                    keystrokes: 1,
                    started_ms: timestamp_ms,
                });
                Absorbed::Switched(finished)
            }
            None => {
                let identifiers = self.identity_for(&el);
                self.current = Some(GridEdit {
                    bounds: super::bounds_of(Some(&el)),
                    cell,
                    text,
                    identifiers,
                    process_name: self.process_name.clone(),
                    keystrokes: 1,
                    started_ms: timestamp_ms,
                });
                Absorbed::Continued
            }
        }
    }
    /// Emit whatever edit is in flight. Called when the session stops.
    pub fn flush(&mut self, timestamp_ms: u64) -> Option<ActionCandidate> {
        self.emit(timestamp_ms)
    }

    /// The focused cell editor's reference and text, and the element itself.
    ///
    /// Returns `None` for every non-grid context, which is the common case, so
    /// this must stay cheap: one focused-element resolution and two string
    /// checks before the `is_cell_editor` gate rejects.
    ///
    /// Runs on **every** key event in both directions, so what is not done here
    /// matters as much as what is. It reads four properties at most and returns
    /// the element rather than deriving anything further from it: the owning
    /// application and window are two more round trips each and are needed only
    /// when an edit is being STARTED, which is a small minority of samples.
    /// See [`Self::identity_for`].
    fn sample(&mut self) -> Option<(String, String, UIElement)> {
        if self.desktop.is_none() {
            self.desktop = Desktop::new_default().ok();
        }
        let desktop = self.desktop.as_ref()?;
        let el = desktop.focused_element().ok()?;
        let role = el.role();
        let cell = el.name().unwrap_or_default();
        if !is_cell_editor(&role, &cell) {
            return None;
        }
        let text = clean_cell_text(&el.text(0).ok()?);
        if text.is_empty() {
            return None;
        }
        Some((cell, text, el))
    }

    /// Identity for an edit that is starting: the click's, when there was one,
    /// and otherwise the element's own so keyboard-only editing stays
    /// identifiable.
    ///
    /// The fallback matters more than it looks, and the reason is measured.
    /// `CapturedStream::admit` fails closed on an action whose source app
    /// cannot be named, and a cell edit driven purely from the keyboard
    /// produces no `Click`, so `note_context` never runs and there is nothing
    /// to name it with. Measured: four real cell edits were detected, sampled
    /// and emitted, then all four were dropped as `UnidentifiedSource`. Reading
    /// the identity off the element already resolved is the same
    /// identification a click performs -- it supplies the missing fact rather
    /// than weakening the gate. Moving this out of `sample` changes *when* it
    /// is read, never *whether*.
    ///
    /// Deliberately not computed on the common path. `application()` and
    /// `window()` are UI Automation round trips with a name read each, and
    /// until now they ran on every sample and were then discarded twice over:
    /// once whenever a click had already populated `self.identifiers`, and once
    /// more because the "same cell as before" branch -- every keystroke after
    /// the first in a cell -- never reads the result at all. Typing a
    /// twenty-character value paid for them about forty times and used them
    /// once.
    fn identity_for(&self, el: &UIElement) -> Vec<String> {
        if !self.identifiers.is_empty() {
            return self.identifiers.clone();
        }
        let mut ids = Vec::new();
        if let Ok(Some(app)) = el.application() {
            if let Some(n) = app.name().filter(|n| !n.trim().is_empty()) {
                ids.push(n);
            }
        }
        if let Ok(Some(win)) = el.window() {
            if let Some(n) = win.name().filter(|n| !n.trim().is_empty()) {
                ids.push(n);
            }
        }
        ids
    }

    fn emit(&mut self, timestamp_ms: u64) -> Option<ActionCandidate> {
        let edit = self.current.take()?;
        if edit.text.is_empty() {
            return None;
        }
        Some(ActionCandidate {
            // From when the edit started. The editor no longer exists here.
            element_bounds: edit.bounds,
            kind: ActionKind::Type,
            identifiers: edit.identifiers,
            process_name: edit.process_name,
            // What was actually observed: the overlay's role, and the cell it
            // named. Recording the cell reference as the element name is the
            // only cell identity that exists.
            element_role: Some("ComboBox".to_string()),
            // Qualified with the sheet when one is known, bare otherwise. The
            // bare form is byte-identical to what this emitted before sheet
            // tracking existed, so a recording that never switches sheets is
            // unchanged all the way through compile, store and replay.
            element_name: Some(match &self.current_sheet {
                Some(sheet) => format!("{sheet}!{}", edit.cell),
                None => edit.cell,
            }),
            payload: Some(edit.text),
            window: None,
            detail: Some(format!(
                "grid cell editor, {} keystroke(s) over {}ms",
                edit.keystrokes,
                timestamp_ms.saturating_sub(edit.started_ms)
            )),
            timestamp_ms,
        })
    }
}

/// One descent collecting both facts a position needs: the selected cell from
/// the Name Box, and the document id from the browser's address bar.
///
/// Bounded and depth-limited. It stops descending a branch once both are found,
/// Marks a position as an element reference rather than a cell reference.
///
/// Chosen so it cannot be confused with one: `parse_cell_ref` requires letters
/// followed by digits, and this contains a `/`, so a cell reference can never
/// decode as an element reference or the reverse.
pub const ELEMENT_REF_PREFIX: &str = "el/";

/// `el/<record ordinal>/<field label>`.
///
/// A label is schema, not data -- the same kind of thing a column letter is --
/// so including it keeps the resulting mapping legible to a user without
/// carrying what was copied. Content with no label beside it encodes an empty
/// field rather than inventing one.
pub fn encode_element_ref(record: usize, label: Option<&str>) -> String {
    format!("{ELEMENT_REF_PREFIX}{record}/{}", label.unwrap_or_default())
}

/// The inverse. `None` for anything that is not an element reference.
pub fn decode_element_ref(reference: &str) -> Option<(usize, String)> {
    let rest = reference.trim().strip_prefix(ELEMENT_REF_PREFIX)?;
    let (ordinal, label) = rest.split_once('/')?;
    Some((ordinal.parse().ok()?, label.to_string()))
}

#[allow(dead_code)]
/// REPLACED 2026-08-22 by `WindowScan::url`, which the position walk fills in
/// on the same visit that reads the document id -- same element, same
/// `text(0)`, one traversal instead of two.
///
/// Kept only because the fallback rule it documents still applies at the call
/// site: the address bar is the general answer, and the window title is the
/// fallback when there is no address bar at all.
fn page_identity(root: &UIElement) -> Option<String> {
    let mut budget = 3000usize;
    let mut url = None;
    collect_page_identity(root, 0, &mut budget, &mut url);
    url.or_else(|| {
        let title = root.name().unwrap_or_default();
        (!title.trim().is_empty()).then_some(title)
    })
}

#[allow(dead_code)]
fn collect_page_identity(el: &UIElement, depth: usize, budget: &mut usize, url: &mut Option<String>) {
    if depth > 14 || *budget == 0 || url.is_some() {
        return;
    }
    *budget -= 1;
    if el.name().unwrap_or_default().trim() == "Address and search bar" {
        let text = el.text(0).unwrap_or_default();
        if !text.trim().is_empty() {
            *url = Some(text.trim().to_string());
            return;
        }
    }
    if let Ok(children) = el.children() {
        for c in &children {
            collect_page_identity(c, depth + 1, budget, url);
        }
    }
}

/// How long the node walk may run before it gives up and reports nothing.
///
/// **A node budget alone cannot bound this, because the cost per node is a
/// property of the page and not of paradigm.** Measured 2026-08-22 against a
/// Wikipedia article with a 3000-node budget: the bare traversal -- `children()`
/// and no property reads at all -- took **1163ms**, and the full walk 3828ms.
/// The same walk on OrderFlow's 46-element tree finishes in ~150ms.
///
/// The threshold is bounded by direct measurement of the WALK on each surface,
/// via `examples/read_cost_probe.rs`:
///
/// | window | nodes | full walk |
/// |---|---|---|
/// | OrderFlow dashboard | 78 | **113ms** |
/// | Wikipedia article | 3000 (budget spent) | **3828ms** |
///
/// 400ms is three times what a page like OrderFlow needs and a tenth of what
/// Wikipedia would take, so it separates the two cases with room on both sides.
///
/// **Not to be confused with the per-key-down means in the session logs** --
/// 12ms on Sheets, ~150ms on OrderFlow, 202-274ms on the pricing page, 1297ms on
/// Wikipedia. Those are averages over ALL key-downs, most of which are ordinary
/// typing that does no walk at all, so they understate the per-walk cost by an
/// unknown factor. Reading them as walk costs is a mistake this comment made in
/// its first draft.
const WALK_TIME_BUDGET_MS: u128 = 400;

/// Flatten a window into the document-order node list the general rule takes.
///
/// Stops on **either** budget: 3000 nodes, or [`WALK_TIME_BUDGET_MS`]. Sets
/// `timed_out` when it is the clock that stopped it, because a partial node list
/// is not a smaller tree -- it is a tree with the wrong multiplicities, and
/// `identity::tree` classifies structural against content by multiplicity. Using
/// half a walk would silently reclassify content as unique and produce a
/// confident wrong position.
fn collect_nodes(
    el: &UIElement,
    budget: &mut usize,
    deadline: &Instant,
    timed_out: &mut bool,
    out: &mut Vec<crate::identity::tree::TreeNode>,
) {
    if *budget == 0 || *timed_out {
        return;
    }
    // Checked per node rather than per level: the whole problem is a tree deep
    // and wide enough that a level can take seconds on its own.
    if Instant::now() >= *deadline {
        *timed_out = true;
        return;
    }
    *budget -= 1;
    out.push(crate::identity::tree::TreeNode::new(
        el.id().unwrap_or_default(),
        el.role(),
        el.name().unwrap_or_default(),
    ));
    if let Ok(children) = el.children() {
        for c in &children {
            collect_nodes(c, budget, deadline, timed_out, out);
        }
    }
}

/// What one window walk established, kept together because one walk answers
/// several questions at once.
#[derive(Debug, Default, Clone)]
struct WindowScan {
    /// The Name Box's cell reference, if it read.
    cell: Option<String>,
    /// The address bar's full URL, captured on the same visit that reads the
    /// document id. What `page_identity` used to run a second walk to find.
    url: Option<String>,
    /// The `/d/<id>/` document id from the address bar, if there was one.
    document: Option<String>,
    /// A Name Box group was **present in the tree**, whether or not it read.
    ///
    /// This is the fact that separates "not a spreadsheet" from "a spreadsheet
    /// having a bad moment", and they are not the same page at all.
    name_box_seen: bool,
    /// The walk stopped early on budget, so it saw only part of the tree and
    /// the absence of a Name Box proves nothing.
    truncated: bool,
}

/// What a scan licenses the caller to do.
#[derive(Debug, PartialEq, Eq)]
enum PositionRead {
    /// A spreadsheet, read successfully.
    Spreadsheet { document: String, cell: String },
    /// A Name Box is there and the walk could not turn it into a position --
    /// no readable reference, or no document id to disambiguate two windows
    /// both titled "Untitled spreadsheet".
    ///
    /// The honest answer is nothing, exactly as it was before the
    /// element-identity path existed. Falling back to element identity here is
    /// what produced the live failure this distinction exists to prevent: see
    /// `a_sheets_window_whose_name_box_will_not_read_yields_no_position`.
    SpreadsheetUnreadable,
    /// No Name Box anywhere in a walk that saw the whole tree. A page that was
    /// never a spreadsheet, and the only case element identity may address.
    NotASpreadsheet,
    /// The walk was truncated, so nothing can be concluded from what it did
    /// not find. Treated as a failed read rather than as a non-spreadsheet,
    /// because "I did not look everywhere" is not evidence of absence.
    Inconclusive,
}

impl WindowScan {
    fn decide(&self) -> PositionRead {
        if let (Some(document), Some(cell)) = (self.document.clone(), self.cell.clone()) {
            return PositionRead::Spreadsheet { document, cell };
        }
        // A Name Box in the tree settles what kind of surface this is, whatever
        // else the walk did or did not manage to read.
        if self.name_box_seen {
            return PositionRead::SpreadsheetUnreadable;
        }
        if self.truncated {
            return PositionRead::Inconclusive;
        }
        PositionRead::NotASpreadsheet
    }
}

/// Walk the window for the two facts a spreadsheet position needs, and for
/// whether a Name Box was there at all.
///
/// Stops as soon as both facts are in hand, which is what keeps the measured
/// cost at tens of milliseconds rather than a full-window walk.
///
/// Depth truncation is ordinary and is not recorded: real web trees are deeper
/// than 14 and the Name Box and address bar both sit well above that. Running
/// out of BUDGET is different -- it means whole branches went unvisited -- and
/// that is recorded, because a Name Box could have been in one of them.
fn collect_position(el: &UIElement, depth: usize, budget: &mut usize, scan: &mut WindowScan) {
    if *budget == 0 {
        scan.truncated = true;
        return;
    }
    if depth > 14 || (scan.cell.is_some() && scan.document.is_some()) {
        return;
    }
    *budget -= 1;

    let name = el.name().unwrap_or_default();
    let trimmed = name.trim();

    // The Name Box group holds an Edit reporting the selected cell reference.
    if trimmed.starts_with("Name box") {
        // Seen is recorded before the read is attempted, because the whole
        // point is that a Name Box which fails to read is still a Name Box.
        scan.name_box_seen = true;
        if scan.cell.is_none() {
            if let Ok(children) = el.children() {
                if let Some(edit) = children.into_iter().find(|c| c.role() == "Edit") {
                    let text = edit.text(0).unwrap_or_default();
                    let reference = text.trim();
                    if looks_like_cell_ref(split_sheet_ref(reference).1) {
                        scan.cell = Some(reference.to_string());
                    }
                }
            }
        }
    }

    // The address bar carries /d/<id>/, which is the only thing that separates
    // two documents both titled "Untitled spreadsheet".
    //
    // The FULL url is kept at the same time, which is what makes merging the
    // page-identity walk into this one free: `page_identity` used to run a
    // second traversal to reach this same element and read this same string.
    // Same element, same `text(0)`, no extra node visited, and the early-exit
    // condition below is untouched -- `document` and `url` are found together or
    // not at all.
    if trimmed == "Address and search bar" {
        let url = el.text(0).unwrap_or_default();
        let trimmed_url = url.trim();
        if scan.url.is_none() && !trimmed_url.is_empty() {
            scan.url = Some(trimmed_url.to_string());
        }
        if scan.document.is_none() {
            if let Some(rest) = url.split("/d/").nth(1) {
                if let Some(id) = rest.split('/').next() {
                    if !id.is_empty() {
                        scan.document = Some(id.to_string());
                    }
                }
            }
        }
    }

    if let Ok(children) = el.children() {
        for child in children {
            collect_position(&child, depth + 1, budget, scan);
            if scan.cell.is_some() && scan.document.is_some() {
                return;
            }
        }
    }
}

/// What one sample did to the edit in flight.
///
/// `Switched` is the only outcome that ends an edit, and it carries whatever
/// that ending produced -- which may be nothing, when the finished edit had no
/// text worth emitting. Keeping that `Option` inside the variant is what lets
/// the caller return early on a switch without a trigger key ever being
/// considered, exactly as it did before key-up sampling existed.
enum Absorbed {
    /// No editor open, so there was nothing to fold in.
    Nothing,
    /// Folded into the edit in flight.
    Continued,
    /// The sample named a different cell, so the previous edit ended here.
    Switched(Option<ActionCandidate>),
}

/// Enter and Tab, the two keys that commit a cell edit.
///
/// Alt+Tab is a window switch, not a commit, and the key code alone cannot tell
/// the two apart -- the recorder carries `alt_pressed` on the same event, so the
/// question is answerable rather than guessable. Treating a window switch as a
/// commit ends the edit at a moment the user did not choose.
fn is_trigger_key(key_code: u32, alt_pressed: bool) -> bool {
    if alt_pressed {
        return false;
    }
    key_code == 0x0D || key_code == 0x09
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(document: &str, cell: &str) -> (String, String) {
        (document.to_string(), cell.to_string())
    }

    #[test]
    fn a_copy_then_paste_is_paired() {
        let mut w = GridCellWatcher::new();
        w.pair_clipboard(KEY_C, pos("orders", "C2"), 10);
        w.pair_clipboard(KEY_V, pos("shipping", "B5"), 20);

        let links = w.take_links();
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0],
            SourceLink {
                seq: 20,
                source_document: "orders".into(),
                source_cell: "C2".into(),
                destination_document: "shipping".into(),
                destination_cell: "B5".into(),
            }
        );
        // Drained, not copied -- a second read must not re-deliver them.
        assert!(w.take_links().is_empty());
    }

    #[test]
    fn a_paste_with_no_preceding_copy_records_nothing() {
        // The value came from somewhere this session never saw. Inventing a
        // source would be worse than recording none.
        let mut w = GridCellWatcher::new();
        w.pair_clipboard(KEY_V, pos("shipping", "B5"), 10);
        assert!(w.take_links().is_empty());
    }

    #[test]
    fn each_copy_replaces_the_last_and_pairs_with_its_own_paste() {
        // The shape the design's example produces: copy a row, paste it, copy
        // the next, paste that. Each pair must be its own, or the mapping
        // detection infers would be nonsense.
        let mut w = GridCellWatcher::new();
        w.pair_clipboard(KEY_C, pos("orders", "C2"), 1);
        w.pair_clipboard(KEY_V, pos("shipping", "B5"), 2);
        w.pair_clipboard(KEY_C, pos("orders", "C3"), 3);
        w.pair_clipboard(KEY_V, pos("shipping", "B6"), 4);
        w.pair_clipboard(KEY_C, pos("orders", "C4"), 5);
        w.pair_clipboard(KEY_V, pos("shipping", "B7"), 6);

        let links = w.take_links();
        assert_eq!(links.len(), 3);
        assert_eq!(
            links
                .iter()
                .map(|l| (l.source_cell.as_str(), l.destination_cell.as_str()))
                .collect::<Vec<_>>(),
            vec![("C2", "B5"), ("C3", "B6"), ("C4", "B7")]
        );
    }

    #[test]
    fn one_copy_may_feed_several_pastes() {
        // Not an error: a user filling three cells from one source value is
        // doing something real, and the pairing should describe it rather than
        // silently drop the later pastes.
        let mut w = GridCellWatcher::new();
        w.pair_clipboard(KEY_C, pos("orders", "C2"), 1);
        w.pair_clipboard(KEY_V, pos("shipping", "B5"), 2);
        w.pair_clipboard(KEY_V, pos("shipping", "B6"), 3);
        assert_eq!(w.take_links().len(), 2);
    }

    #[test]
    fn keys_that_are_not_copy_or_paste_are_ignored() {
        let mut w = GridCellWatcher::new();
        w.pair_clipboard(0x58, pos("orders", "C2"), 1); // Ctrl+X, deliberately not tracked
        w.pair_clipboard(0x41, pos("orders", "C2"), 2); // Ctrl+A
        w.pair_clipboard(KEY_V, pos("shipping", "B5"), 3);
        assert!(
            w.take_links().is_empty(),
            "only a real copy may arm a paste"
        );
    }

    /// "One run's numbers are its own" has to hold for the off-key counters
    /// too, or a second recording reports the first one's position reads. The
    /// original four were reset and these two were added later, which is
    /// exactly how a counter gets missed.
    #[test]
    fn resetting_clears_the_off_key_counters_as_well() {
        CLIP_CALLS.store(7, Ordering::Relaxed);
        CLIP_MICROS.store(700, Ordering::Relaxed);
        MARK_CALLS.store(3, Ordering::Relaxed);
        MARK_MICROS.store(300, Ordering::Relaxed);

        reset_timing();

        assert_eq!(
            off_key_timing(),
            ((0, 0), (0, 0)),
            "a new session must start from zero on every counter, not just the \
             ones that existed first"
        );
    }

    /// A log line has to say WHICH surface a mark landed on. Four marks in one
    /// session all logged "armed" and nothing distinguished a spreadsheet from
    /// Notepad, which is what made the 2026-08-18 results unreadable.
    #[test]
    fn a_mark_outcome_names_its_route_and_position() {
        let sheet = MarkOutcome::Spreadsheet { cell: "A2".into() };
        assert!(sheet.armed());
        assert!(sheet.describe().contains("Name Box"));
        assert!(sheet.describe().contains("A2"));

        let page = MarkOutcome::Element {
            reference: encode_element_ref(2, Some("PRODUCT")),
        };
        assert!(page.armed());
        assert!(page.describe().contains("element identity"));
        assert!(page.describe().contains("PRODUCT"));

        assert!(!MarkOutcome::Unavailable.armed());
        assert!(MarkOutcome::Unavailable.describe().contains("NOT armed"));
    }

    /// The document half is a URL **or a window title**, and a browser title
    /// routinely carries page content. `SourceLink` keeps that transient; a log
    /// file is not transient, so it must not appear in a description.
    #[test]
    fn a_mark_outcome_never_describes_the_document() {
        let outcomes = [
            MarkOutcome::Spreadsheet { cell: "B7".into() },
            MarkOutcome::Element {
                reference: encode_element_ref(0, Some("QUANTITY")),
            },
            MarkOutcome::Unavailable,
        ];
        for outcome in outcomes {
            let described = outcome.describe();
            assert!(
                !described.contains("Invoice") && !described.contains("http"),
                "a description carries a position, never a document: {described:?}"
            );
        }
    }

    /// THE DEFECT, reproduced. A copy whose position could not be read used to
    /// leave the previous copy's position armed, so the next paste paired with
    /// a source the user never copied. Enough of those and every destination
    /// shares one source, `prove_advance` sees a repeat, and detection reports
    /// `SourceDidNotAdvance` about a recording whose source advanced fine.
    ///
    /// That is exactly what session record-f15e933d produced on 2026-08-21.
    #[test]
    fn a_copy_whose_position_cannot_be_read_disarms_the_source() {
        let mut w = GridCellWatcher::new();
        w.note_clipboard_key(KEY_C, Some(pos("pricing", "el/0/TIER")), 1);
        // Second copy: the read failed -- scrolled off-screen, structural
        // element, whatever the cause.
        w.note_clipboard_key(KEY_C, None, 2);
        w.note_clipboard_key(KEY_V, Some(pos("book", "B3")), 3);

        assert!(
            w.take_links().is_empty(),
            "the paste must record NO link rather than pair with the first \
             copy's position, which the user did not copy this time"
        );
    }

    /// The correction must not break the documented behaviour that one copy may
    /// feed several pastes -- a failed DESTINATION read says nothing about the
    /// source and must disarm nothing.
    #[test]
    fn a_paste_whose_position_cannot_be_read_leaves_the_source_armed() {
        let mut w = GridCellWatcher::new();
        w.note_clipboard_key(KEY_C, Some(pos("orders", "C2")), 1);
        w.note_clipboard_key(KEY_V, None, 2); // destination unreadable
        w.note_clipboard_key(KEY_V, Some(pos("book", "B2")), 3);

        let links = w.take_links();
        assert_eq!(links.len(), 1, "the second paste still pairs");
        assert_eq!(links[0].source_cell, "C2");
        assert_eq!(links[0].destination_cell, "B2");
    }

    /// The three sources of a three-record transfer must stay distinct, which
    /// is the property `prove_advance` checks and the one the defect destroyed.
    #[test]
    fn three_reads_that_all_succeed_still_produce_three_distinct_sources() {
        let mut w = GridCellWatcher::new();
        for (i, (src, dst)) in [("el/0/TIER", "A2"), ("el/1/TIER", "A3"), ("el/2/TIER", "A4")]
            .iter()
            .enumerate()
        {
            let seq = (i as u64) * 2;
            w.note_clipboard_key(KEY_C, Some(pos("pricing", src)), seq);
            w.note_clipboard_key(KEY_V, Some(pos("book", dst)), seq + 1);
        }
        let links = w.take_links();
        assert_eq!(links.len(), 3);
        let sources: std::collections::BTreeSet<&str> =
            links.iter().map(|l| l.source_cell.as_str()).collect();
        assert_eq!(sources.len(), 3, "three distinct sources, so the source advanced");
    }

    /// An explicit mark is the most authoritative statement of intent there is,
    /// so a clipboard event arriving behind it must not replace its position --
    /// and the marked source must still feed the paste that follows.
    ///
    /// Exercises what `note_marked_source` does either side of its live read,
    /// which is the part that cannot run without a real desktop.
    #[test]
    fn an_explicit_mark_survives_a_clipboard_event_behind_it() {
        let mut w = GridCellWatcher::new();
        w.last_copy_key_ms = Some(1_000);
        w.pair_clipboard(KEY_C, pos("orders", "C2"), 1_000);

        assert!(
            !w.clipboard_needs_own_read(1_300),
            "the monitor must stand down after a deliberate mark"
        );

        w.pair_clipboard(KEY_V, pos("book", "B2"), 1_400);
        let links = w.take_links();
        assert_eq!(links.len(), 1, "the mark armed exactly one paste");
        assert_eq!(links[0].source_cell, "C2");
        assert_eq!(links[0].destination_cell, "B2");
    }

    /// A copy made with no keystroke -- menu, right-click, a Copy button -- is
    /// the whole reason the clipboard monitor is consumed at all.
    #[test]
    fn a_clipboard_change_with_no_copy_keystroke_reads_its_own_position() {
        let w = GridCellWatcher::new();
        assert!(
            w.clipboard_needs_own_read(5_000),
            "nothing else marked this source, so the monitor must"
        );
    }

    /// `Ctrl+C` fires the keystroke hook AND, ~200ms later, the clipboard
    /// monitor. The keystroke read the position synchronously at the keypress;
    /// the monitor's would be later and possibly from another window. The early,
    /// accurate one must win.
    #[test]
    fn a_clipboard_change_just_after_ctrl_c_does_not_read_again() {
        let mut w = GridCellWatcher::new();
        w.last_copy_key_ms = Some(1_000);
        assert!(!w.clipboard_needs_own_read(1_000), "same instant");
        assert!(!w.clipboard_needs_own_read(1_200), "one poll interval later");
        assert!(
            !w.clipboard_needs_own_read(1_400),
            "poll plus the element-resolution timeout"
        );
        assert!(
            !w.clipboard_needs_own_read(2_000),
            "the grace window is inclusive at its edge"
        );
    }

    /// A later copy by menu is a genuinely new source and must be read, however
    /// the previous one arrived.
    #[test]
    fn a_clipboard_change_well_after_ctrl_c_reads_again() {
        let mut w = GridCellWatcher::new();
        w.last_copy_key_ms = Some(1_000);
        assert!(w.clipboard_needs_own_read(2_001), "past the grace window");
        assert!(w.clipboard_needs_own_read(30_000), "much later");
    }

    /// Timestamps are not guaranteed monotonic across the recorder's threads,
    /// and a subtraction that wrapped would suppress every later copy for the
    /// rest of the session.
    #[test]
    fn a_clipboard_change_before_the_copy_key_does_not_underflow() {
        let mut w = GridCellWatcher::new();
        w.last_copy_key_ms = Some(5_000);
        assert!(
            !w.clipboard_needs_own_read(4_000),
            "an earlier timestamp saturates to zero rather than wrapping to a \
             huge difference that would read again"
        );
    }

    /// The encoding must never be mistakable for a cell reference in either
    /// direction, or a spreadsheet position could decode as an element one.
    #[test]
    fn an_element_reference_cannot_be_confused_with_a_cell_reference() {
        let encoded = encode_element_ref(3, Some("QUANTITY"));
        assert_eq!(decode_element_ref(&encoded), Some((3, "QUANTITY".into())));
        assert!(!looks_like_cell_ref(&encoded));

        for cell in ["A1", "B2", "AA10", "C5"] {
            assert_eq!(decode_element_ref(cell), None, "{cell} is a cell reference");
        }
    }

    #[test]
    fn unlabelled_content_encodes_an_empty_field_rather_than_a_guess() {
        let encoded = encode_element_ref(2, None);
        assert_eq!(decode_element_ref(&encoded), Some((2, String::new())));
    }

    /// The §3 invariant, restated for the new path: an element reference is a
    /// position and a label, never the value that was copied.
    #[test]
    fn an_element_reference_carries_no_copied_value() {
        let encoded = encode_element_ref(1, Some("PRODUCT"));
        assert!(encoded.contains("PRODUCT"), "the label is schema, and kept");
        assert!(
            !encoded.contains("Ceramic Mug Set"),
            "no value may appear: {encoded}"
        );
    }

    #[test]
    fn a_link_carries_positions_and_never_a_value() {
        // §3's rule: the durable-shaped data is structural. A SourceLink has no
        // field that could hold what was copied, and this fails if one is
        // added -- the same guard as workflow_processed_rows.
        let mut w = GridCellWatcher::new();
        w.pair_clipboard(KEY_C, pos("orders", "C2"), 1);
        w.pair_clipboard(KEY_V, pos("shipping", "B5"), 2);
        let printed = format!("{:?}", w.take_links()[0]);
        assert!(printed.contains("C2") && printed.contains("B5"));
        assert!(
            !printed.to_lowercase().contains("value") && !printed.contains("text"),
            "a link must not carry content: {printed}"
        );
    }

    #[test]
    fn a_sheet_tab_click_is_recognised_and_ordinary_clicks_are_not() {
        // The measured shape of a real human tab click.
        assert!(looks_like_sheet_tab("text", "Sheet1"));
        assert!(looks_like_sheet_tab("text", "Sheet12"));
        assert!(looks_like_sheet_tab("Button", "Sheet2"));

        // Everything else a user might click. A false positive here would
        // silently misattribute every later cell edit to a sheet that was
        // never opened, which is worse than not tracking at all.
        assert!(!looks_like_sheet_tab("text", "Sheet"));
        assert!(!looks_like_sheet_tab("text", "Sheets home"));
        assert!(!looks_like_sheet_tab("text", "Sheet tab bar"));
        assert!(!looks_like_sheet_tab("text", "Add Sheet"));
        assert!(!looks_like_sheet_tab("text", "All Sheets"));
        assert!(!looks_like_sheet_tab("text", "SheetA"));
        assert!(!looks_like_sheet_tab("ComboBox", "Sheet1"));
        assert!(!looks_like_sheet_tab("text", "Windows PowerShell"));

        // The documented limit, pinned so it is not mistaken for a bug later:
        // a renamed sheet is indistinguishable from any other text click.
        assert!(!looks_like_sheet_tab("text", "Data"));
        assert!(!looks_like_sheet_tab("text", "Q3 Forecast"));
    }

    #[test]
    fn a_qualified_reference_splits_and_still_takes_the_grid_path() {
        assert_eq!(split_sheet_ref("Sheet2!B2"), (Some("Sheet2"), "B2"));
        assert_eq!(split_sheet_ref("B2"), (None, "B2"));
        // Degenerate forms fall back to "the whole thing is the cell", which
        // then fails `looks_like_cell_ref` rather than half-parsing.
        assert_eq!(split_sheet_ref("!B2"), (None, "!B2"));
        assert_eq!(split_sheet_ref("Sheet2!"), (None, "Sheet2!"));

        // Replay decides the grid path with this, so a qualified step must
        // still reach `grid_type`.
        assert!(is_cell_editor("ComboBox", "Sheet2!B2"));
        assert!(is_cell_editor("ComboBox", "B2"));
        assert!(!is_cell_editor("ComboBox", "Sheet2!Menus"));
    }

    #[test]
    fn edits_are_stamped_with_the_tracked_sheet_and_bare_without_one() {
        // No tab click seen: byte-identical to the pre-tracking behaviour.
        let mut w = GridCellWatcher::new();
        assert_eq!(w.tracked_sheet(), None);
        w.current = Some(GridEdit {
            bounds: None,
            cell: "B2".into(),
            text: "hello".into(),
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            keystrokes: 1,
            started_ms: 0,
        });
        let bare = w.flush(10).expect("an edit was in flight");
        assert_eq!(bare.element_name.as_deref(), Some("B2"));

        // After a tab click, the same edit carries the sheet.
        let mut w = GridCellWatcher::new();
        w.note_click(
            "text",
            Some("Sheet2"),
            None,
            0,
            vec!["msedge.exe".into()],
            Some("msedge.exe".into()),
        );
        assert_eq!(w.tracked_sheet(), Some("Sheet2"));
        w.current = Some(GridEdit {
            bounds: None,
            cell: "B2".into(),
            text: "hello".into(),
            identifiers: vec!["msedge.exe".into()],
            process_name: Some("msedge.exe".into()),
            keystrokes: 1,
            started_ms: 0,
        });
        let stamped = w.flush(10).expect("an edit was in flight");
        assert_eq!(stamped.element_name.as_deref(), Some("Sheet2!B2"));
    }

    #[test]
    fn an_ordinary_click_does_not_disturb_the_tracked_sheet() {
        // Clicks are how app identity is learned, so they arrive constantly.
        // Only a tab click may change which sheet edits are attributed to.
        let mut w = GridCellWatcher::new();
        w.note_click("text", Some("Sheet2"), None, 0, vec!["msedge.exe".into()], None);
        w.note_click("text", Some("Windows PowerShell"), None, 0, vec!["pwsh.exe".into()], None);
        w.note_click("Button", Some("Add Sheet"), None, 0, vec!["msedge.exe".into()], None);
        w.note_click("ComboBox", Some("A1"), None, 0, vec!["msedge.exe".into()], None);
        assert_eq!(
            w.tracked_sheet(),
            Some("Sheet2"),
            "only a sheet-tab click may retarget the sheet"
        );
    }

    /// The mechanism itself: a click arms the source of the next copy.
    #[test]
    fn a_click_arms_the_source_of_the_next_copy() {
        let mut w = GridCellWatcher::new();
        assert_eq!(
            w.click_source_id(0),
            None,
            "nothing has been clicked, so there is no source to offer"
        );
        w.note_click("Text", Some("Free"), Some("id-free"), 1_000, vec![], None);
        assert_eq!(w.click_source_id(1_200), Some("id-free"));
    }

    /// Each click replaces the last, so three selections give three sources.
    /// This is the whole of the fix for record-fe88fb0d, where nine copies of
    /// nine different values all resolved to one reference.
    #[test]
    fn three_clicks_offer_three_different_sources() {
        let mut w = GridCellWatcher::new();
        let mut seen = Vec::new();
        for (id, at) in [("id-free", 1_000), ("id-go", 4_000), ("id-plus", 7_000)] {
            w.note_click("Text", Some("tier"), Some(id), at, vec![], None);
            seen.push(w.click_source_id(at + 800).map(str::to_string));
        }
        assert_eq!(
            seen,
            vec![
                Some("id-free".to_string()),
                Some("id-go".to_string()),
                Some("id-plus".to_string())
            ],
            "each copy must take its own click, not the first one"
        );
    }

    /// LIMITATION 1: a copy with no click behind it at all -- a keyboard-only
    /// selection. It must decline, not fall back to the focused element, which
    /// is the Document and is the same for every copy in the session.
    #[test]
    fn a_keyboard_only_selection_has_no_source() {
        let w = GridCellWatcher::new();
        assert_eq!(w.click_source_id(50_000), None);
    }

    /// LIMITATION 2: a click too old to be this copy's selection. The user
    /// clicked something a minute ago and has since copied by some other means.
    #[test]
    fn a_stale_click_is_not_adopted_as_a_source() {
        let mut w = GridCellWatcher::new();
        w.note_click("Text", Some("Free"), Some("id-free"), 1_000, vec![], None);
        assert_eq!(
            w.click_source_id(1_000 + CLICK_SOURCE_GRACE_MS),
            Some("id-free"),
            "the boundary itself is still eligible"
        );
        assert_eq!(
            w.click_source_id(1_000 + CLICK_SOURCE_GRACE_MS + 1),
            None,
            "one millisecond past the window is not"
        );
        assert_eq!(w.click_source_id(60_000), None, "a minute later, certainly not");
    }

    /// A click the tree could not name arms nothing, and -- importantly -- does
    /// not disarm what was already there. The previous click stays, and ages out
    /// on its own schedule rather than being cancelled by an unrelated event.
    #[test]
    fn a_click_with_no_id_neither_arms_nor_disarms() {
        let mut w = GridCellWatcher::new();
        w.note_click("Text", Some("Free"), Some("id-free"), 1_000, vec![], None);
        w.note_click("Pane", None, Some("   "), 1_100, vec![], None);
        w.note_click("Pane", None, None, 1_200, vec![], None);
        assert_eq!(
            w.click_source_id(1_300),
            Some("id-free"),
            "a nameless click must not erase a real one"
        );
    }

    #[test]
    fn cell_references_are_recognised_and_other_names_are_not() {
        for good in ["A1", "B2", "Z9", "AA10", "BC12"] {
            assert!(looks_like_cell_ref(good), "{good} should parse");
        }
        // The editor overlay shares its role with ordinary dropdowns, so these
        // must not be mistaken for cells.
        for bad in ["Menus", "Zoom", "", "A", "1", "Sheet1", "Name box", "A1B"] {
            assert!(!looks_like_cell_ref(bad), "{bad} should NOT parse");
        }
    }

    #[test]
    fn the_stray_invisible_character_is_removed() {
        // Exactly what a real recording reported as "19 plus a stray invisible
        // character", and what the editor holds after commit.
        assert_eq!(clean_cell_text("\u{feff}19"), "19");
        assert_eq!(clean_cell_text("\u{feff}\n"), "");
        assert_eq!(clean_cell_text("apple\n"), "apple");
        assert_eq!(clean_cell_text("apple"), "apple");
    }

    #[test]
    fn a_cell_reference_is_not_confused_with_a_trailing_letter() {
        // "AA10" is a real column; "A1B" is not a cell and must not pass, or a
        // dropdown named like one would be captured as a spreadsheet edit.
        assert!(looks_like_cell_ref("AA10"));
        assert!(!looks_like_cell_ref("A1B"));
    }

    #[test]
    fn only_enter_and_tab_commit_a_cell() {
        assert!(is_trigger_key(0x0D, false));
        assert!(is_trigger_key(0x09, false));
        for other in [0x41u32, 0x30, 0x1B, 0x08] {
            assert!(!is_trigger_key(other, false));
        }
    }

    #[test]
    fn alt_tab_is_a_window_switch_and_never_a_commit() {
        // The live OrderFlow recording ended a cell edit by leaving for the
        // browser. A Tab arriving with Alt held is that switch, not a commit,
        // and committing there ends the edit on a keystroke the user did not
        // aim at the cell.
        assert!(!is_trigger_key(0x09, true), "Alt+Tab is not a Tab");
        assert!(!is_trigger_key(0x0D, true), "Alt+Enter is not a commit either");
    }

    #[test]
    fn every_non_grid_surface_is_ignored() {
        // This is the Notepad regression, pinned as an invariant. Notepad's
        // editing surface reports role "Document" -- measured by
        // `text_capture_probe -- notepad`, which is why "document" is in
        // `text::TEXT_ROLES`. If this watcher ever accepted it, Notepad typing
        // would gain a second, bogus action attributed to a spreadsheet cell.
        assert!(!is_cell_editor("Document", ""));
        assert!(!is_cell_editor("Document", "Untitled - Notepad"));

        // Web text fields, which `TextFieldWatcher` owns.
        assert!(!is_cell_editor("Edit", ""));
        assert!(!is_cell_editor("Edit", "FieldA"));
        assert!(!is_cell_editor("edit", "A1"));

        // A ComboBox is necessary but nowhere near sufficient: Sheets' own
        // window contains dropdowns named "Menus" and "Zoom".
        assert!(!is_cell_editor("ComboBox", "Menus"));
        assert!(!is_cell_editor("ComboBox", "Zoom"));
        assert!(!is_cell_editor("ComboBox", ""));
        assert!(!is_cell_editor("Button", "A1"));

        // Only the real thing.
        assert!(is_cell_editor("ComboBox", "A1"));
        assert!(is_cell_editor("ComboBox", "BC12"));
    }

    #[test]
    fn nothing_is_emitted_without_an_observed_edit() {
        // The watcher must be inert everywhere that is not a grid: no editor
        // observed means no candidate, whatever keys arrive.
        let mut w = GridCellWatcher::new();
        assert!(w.flush(0).is_none());
    }

    /// The exact case that produced the false collapse in the live OrderFlow
    /// recording, and the whole reason this distinction exists.
    ///
    /// A Google Sheets window whose Name Box will not read is still a
    /// spreadsheet. Routing it to element identity is not a harmless fallback:
    /// Sheets exposes no per-cell elements, so `identity::tree::locate` returns
    /// the same ordinal for every cell in the document -- measured as `el/0/`
    /// against that module's own canvas fixture. Three pastes into three
    /// different rows then encode ONE destination, `detect` collapses them in
    /// its corrections map, and the Rule of 3 reports `TooFewExamples` with one
    /// record where there were three. A wrong destination wearing the
    /// appearance of a right one.
    #[test]
    fn a_sheets_window_whose_name_box_will_not_read_yields_no_position() {
        let scan = WindowScan {
            cell: None,
            document: Some("1AbCdEfGhIjK".to_string()),
            name_box_seen: true,
            url: None,
            truncated: false,
        };
        assert_eq!(
            scan.decide(),
            PositionRead::SpreadsheetUnreadable,
            "a spreadsheet having a bad moment must never reach element identity"
        );
    }

    #[test]
    fn a_name_box_that_read_without_a_document_id_is_still_a_spreadsheet() {
        // Both halves are required for a position, but failing the second half
        // does not turn the page into something element identity may address.
        let scan = WindowScan {
            cell: Some("B7".to_string()),
            document: None,
            name_box_seen: true,
            url: None,
            truncated: false,
        };
        assert_eq!(scan.decide(), PositionRead::SpreadsheetUnreadable);
    }

    #[test]
    fn a_page_with_no_name_box_anywhere_routes_to_element_identity() {
        // The case the element path was built for: a dashboard, an inbox, a
        // listing page. Unchanged by this gate.
        let scan = WindowScan {
            cell: None,
            document: None,
            name_box_seen: false,
            url: None,
            truncated: false,
        };
        assert_eq!(scan.decide(), PositionRead::NotASpreadsheet);
    }

    #[test]
    fn a_readable_name_box_still_reads_exactly_as_before() {
        let scan = WindowScan {
            cell: Some("Sheet2!B7".to_string()),
            document: Some("1AbCdEfGhIjK".to_string()),
            name_box_seen: true,
            url: None,
            truncated: false,
        };
        assert_eq!(
            scan.decide(),
            PositionRead::Spreadsheet {
                document: "1AbCdEfGhIjK".to_string(),
                cell: "Sheet2!B7".to_string(),
            }
        );
    }

    #[test]
    fn a_truncated_walk_proves_nothing_about_a_missing_name_box() {
        // "I did not look everywhere" is not evidence of absence, so this fails
        // closed rather than claiming the page is not a spreadsheet.
        let scan = WindowScan {
            cell: None,
            url: None,
            document: None,
            name_box_seen: false,
            truncated: true,
        };
        assert_eq!(scan.decide(), PositionRead::Inconclusive);
    }
}
