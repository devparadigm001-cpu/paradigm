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

use std::sync::atomic::{AtomicU64, Ordering};

use terminator::{Desktop, UIElement};

use super::stream::{ActionCandidate, ActionKind};

// ------------------------------------------------------------------ timing --
//
// Two relaxed atomic adds per keystroke. This exists because the cost of
// resolving the focused element on every key-down was flagged as suspected --
// the observation came from a machine in an unusually loaded state, with no
// user-facing symptom -- and "suspected" is not a number.
static GRID_CALLS: AtomicU64 = AtomicU64::new(0);
static GRID_MICROS: AtomicU64 = AtomicU64::new(0);

/// (calls, total microseconds) spent in `observe_key` since the last reset.
pub fn timing() -> (u64, u64) {
    (
        GRID_CALLS.load(Ordering::Relaxed),
        GRID_MICROS.load(Ordering::Relaxed),
    )
}

/// Zero the counters, so one run's numbers are its own.
pub fn reset_timing() {
    GRID_CALLS.store(0, Ordering::Relaxed);
    GRID_MICROS.store(0, Ordering::Relaxed);
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

/// `C` and `V`. Copy and paste are the only clipboard keys this tracks; cut
/// (`X`) is deliberately excluded, because a cut REMOVES the source row and a
/// pattern inferred from vanishing sources would replay against data that is no
/// longer there.
const KEY_C: u32 = 0x43;
const KEY_V: u32 = 0x56;

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
    pub fn note_click(
        &mut self,
        role: &str,
        name: Option<&str>,
        identifiers: Vec<String>,
        process_name: Option<String>,
    ) {
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
    fn read_position(&mut self) -> Option<(String, String)> {
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
        let mut cell = None;
        let mut document = None;
        collect_position(&root, 0, &mut budget, &mut cell, &mut document);
        Some((document?, cell?))
    }

    /// A key went down. Samples the editor, and emits when an edit finishes.
    ///
    /// Two things end an edit: a trigger key (Enter or Tab), and the editor
    /// reporting a *different* cell than the one being tracked, which is how a
    /// click into another cell mid-edit shows up.
    pub fn observe_key(
        &mut self,
        key_code: u32,
        ctrl_pressed: bool,
        timestamp_ms: u64,
    ) -> Option<ActionCandidate> {
        let started = std::time::Instant::now();
        // Clipboard first: a copy has no editor to sample, and a paste's
        // destination must be read BEFORE the sampling below can disturb
        // anything. Only ever runs on Ctrl+C / Ctrl+V, so the ~50ms walk it
        // costs is not on the typing path.
        if ctrl_pressed && (key_code == KEY_C || key_code == KEY_V) {
            if let Some(position) = self.read_position() {
                self.pair_clipboard(key_code, position, timestamp_ms);
            }
        }
        let out = self.observe_key_inner(key_code, timestamp_ms);
        GRID_CALLS.fetch_add(1, Ordering::Relaxed);
        GRID_MICROS.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        out
    }

    fn observe_key_inner(
        &mut self,
        key_code: u32,
        timestamp_ms: u64,
    ) -> Option<ActionCandidate> {
        // Sample first: on a trigger key the editor is typically still alive at
        // key-down, but relying on that is the mistake this module exists to
        // avoid, so the sample that gets emitted is from an earlier keystroke.
        let observed = self.sample();

        if let Some((cell, text, ids)) = observed {
            // Prefer identity from a click when there was one; fall back to the
            // element's own, so keyboard-only editing is still identifiable.
            let identifiers = if self.identifiers.is_empty() {
                ids
            } else {
                self.identifiers.clone()
            };
            match self.current.as_mut() {
                Some(edit) if edit.cell == cell => {
                    edit.text = text;
                    edit.keystrokes += 1;
                }
                Some(_) => {
                    // Moved to a different cell without a trigger key.
                    let finished = self.emit(timestamp_ms);
                    self.current = Some(GridEdit {
                        cell,
                        text,
                        identifiers,
                        process_name: self.process_name.clone(),
                        keystrokes: 1,
                        started_ms: timestamp_ms,
                    });
                    return finished;
                }
                None => {
                    self.current = Some(GridEdit {
                        cell,
                        text,
                        identifiers,
                        process_name: self.process_name.clone(),
                        keystrokes: 1,
                        started_ms: timestamp_ms,
                    });
                }
            }
        }

        if is_trigger_key(key_code) {
            return self.emit(timestamp_ms);
        }
        None
    }

    /// Emit whatever edit is in flight. Called when the session stops.
    pub fn flush(&mut self, timestamp_ms: u64) -> Option<ActionCandidate> {
        self.emit(timestamp_ms)
    }

    /// The current editor's (cell, text, app identifiers), if one is open.
    ///
    /// Returns `None` for every non-grid context, which is the common case, so
    /// this must stay cheap: one focused-element resolution and two string
    /// checks.
    ///
    /// The identifiers matter more than they look. `CapturedStream::admit` fails
    /// closed on an action whose source app cannot be named, and a cell edit
    /// driven purely from the keyboard produces no `Click`, so `note_context`
    /// never runs and there is nothing to name it with. Measured: four real cell
    /// edits were detected, sampled, and emitted, then all four were dropped as
    /// `UnidentifiedSource`. Reading the identity off the element we have
    /// already resolved is the same identification a click performs -- it
    /// supplies the missing fact rather than weakening the gate.
    fn sample(&mut self) -> Option<(String, String, Vec<String>)> {
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
        Some((cell, text, ids))
    }

    fn emit(&mut self, timestamp_ms: u64) -> Option<ActionCandidate> {
        let edit = self.current.take()?;
        if edit.text.is_empty() {
            return None;
        }
        Some(ActionCandidate {
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
/// which is what keeps the measured cost at tens of milliseconds rather than a
/// full-window walk.
fn collect_position(
    el: &UIElement,
    depth: usize,
    budget: &mut usize,
    cell: &mut Option<String>,
    document: &mut Option<String>,
) {
    if *budget == 0 || depth > 14 || (cell.is_some() && document.is_some()) {
        return;
    }
    *budget -= 1;

    let name = el.name().unwrap_or_default();
    let trimmed = name.trim();

    // The Name Box group holds an Edit reporting the selected cell reference.
    if cell.is_none() && trimmed.starts_with("Name box") {
        if let Ok(children) = el.children() {
            if let Some(edit) = children.into_iter().find(|c| c.role() == "Edit") {
                let text = edit.text(0).unwrap_or_default();
                let reference = text.trim();
                if looks_like_cell_ref(split_sheet_ref(reference).1) {
                    *cell = Some(reference.to_string());
                }
            }
        }
    }

    // The address bar carries /d/<id>/, which is the only thing that separates
    // two documents both titled "Untitled spreadsheet".
    if document.is_none() && trimmed == "Address and search bar" {
        let url = el.text(0).unwrap_or_default();
        if let Some(rest) = url.split("/d/").nth(1) {
            if let Some(id) = rest.split('/').next() {
                if !id.is_empty() {
                    *document = Some(id.to_string());
                }
            }
        }
    }

    if let Ok(children) = el.children() {
        for child in children {
            collect_position(&child, depth + 1, budget, cell, document);
            if cell.is_some() && document.is_some() {
                return;
            }
        }
    }
}

/// Enter and Tab, the two keys that commit a cell edit.
fn is_trigger_key(key_code: u32) -> bool {
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
            vec!["msedge.exe".into()],
            Some("msedge.exe".into()),
        );
        assert_eq!(w.tracked_sheet(), Some("Sheet2"));
        w.current = Some(GridEdit {
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
        w.note_click("text", Some("Sheet2"), vec!["msedge.exe".into()], None);
        w.note_click("text", Some("Windows PowerShell"), vec!["pwsh.exe".into()], None);
        w.note_click("Button", Some("Add Sheet"), vec!["msedge.exe".into()], None);
        w.note_click("ComboBox", Some("A1"), vec!["msedge.exe".into()], None);
        assert_eq!(
            w.tracked_sheet(),
            Some("Sheet2"),
            "only a sheet-tab click may retarget the sheet"
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
        assert!(is_trigger_key(0x0D));
        assert!(is_trigger_key(0x09));
        for other in [0x41u32, 0x30, 0x1B, 0x08] {
            assert!(!is_trigger_key(other));
        }
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
}
