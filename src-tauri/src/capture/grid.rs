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

use terminator::Desktop;

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

/// Is this focused element the transient cell editor?
///
/// The single decision that keeps this watcher out of every non-grid
/// application. Extracted as a pure function so the property can be tested
/// directly: a `Document` surface (Notepad), an `Edit` (`<input>`,
/// `<textarea>`), and a `ComboBox` that is an ordinary dropdown must all be
/// ignored, or capture would start attaching spreadsheet actions to them.
pub fn is_cell_editor(role: &str, name: &str) -> bool {
    role == "ComboBox" && looks_like_cell_ref(name)
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

/// Follows the transient cell editor and produces `Type` candidates.
#[derive(Default)]
pub struct GridCellWatcher {
    desktop: Option<Desktop>,
    current: Option<GridEdit>,
    identifiers: Vec<String>,
    process_name: Option<String>,
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

    /// A key went down. Samples the editor, and emits when an edit finishes.
    ///
    /// Two things end an edit: a trigger key (Enter or Tab), and the editor
    /// reporting a *different* cell than the one being tracked, which is how a
    /// click into another cell mid-edit shows up.
    pub fn observe_key(&mut self, key_code: u32, timestamp_ms: u64) -> Option<ActionCandidate> {
        let started = std::time::Instant::now();
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
            element_name: Some(edit.cell),
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

/// Enter and Tab, the two keys that commit a cell edit.
fn is_trigger_key(key_code: u32) -> bool {
    key_code == 0x0D || key_code == 0x09
}

#[cfg(test)]
mod tests {
    use super::*;

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
