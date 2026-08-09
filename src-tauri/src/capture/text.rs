//! Typed-text capture that does not depend on `TextInputCompleted`.
//!
//! ## Why this exists
//!
//! `terminator-workflow-recorder` emits `WorkflowEvent::TextInputCompleted`
//! carrying the text the user typed. Measured against ground truth, that event
//! arrived for **1 of 20** typed actions (`examples/text_capture_probe`), and
//! the two times it did arrive during earlier steps it carried a truncated
//! prefix (`"p"` for `"probe-user"`, `"ip"` for `"ipc-pipeline-test"`).
//!
//! The cause is in the recorder, not in the text. Its keystroke path is:
//!
//! ```ignore
//! if let Ok(mut tracker) = current_text_input.try_lock() {   // recorder
//!     text_input.add_keystroke(key_code);
//! }
//! ```
//!
//! A `try_lock` that fails drops the keystroke silently, and the UIA thread
//! holds that same mutex across slow element resolution. Lose every keystroke
//! and `keystroke_count` stays 0, so `should_emit_completion` refuses and no
//! event is produced at all. Lose some and the count is wrong. Separately, the
//! event's `text_value` is not an accumulated buffer at all — the recorder
//! calls `element.text(0)` when the completion fires, so a read landing
//! mid-typing yields a prefix.
//!
//! ## What this does instead
//!
//! The same underlying read — `element.text(0)` — but on our own trigger and
//! with no `try_lock` anywhere. Measured 24/24 exact across every probe run and
//! every inter-keystroke delay including 0ms.
//!
//! We watch which text field has focus by following the `Click` events we
//! already receive, and read its value when focus leaves, when a trigger key is
//! pressed, or when the session stops. Emitting only on a *change* from the
//! value observed on entry means merely clicking through fields records
//! nothing.
//!
//! ## What this does NOT fix
//!
//! Typing into a field that is never left — no click elsewhere, no Enter/Tab,
//! no session stop — is still not recorded, because nothing has signalled that
//! the text is final. `stop_session` flushes, so the realistic version of that
//! case is covered.
//!
//! Keyboard-only focus movement is not yet a trigger: focus moved by Tab is
//! caught (Tab is a trigger key), but a click is what starts watching a field,
//! so a field first focused by Tab alone is not watched. See the known-issues
//! doc for the follow-up.

use terminator::UIElement;

use super::stream::{ActionCandidate, ActionKind};

/// Roles we treat as editable text.
///
/// Deliberately narrower than the recorder's `is_text_input_element`, which the
/// probe trace caught starting a tracker on a `Document` and on a `Button`
/// named "Done". A false positive here costs a wasted UIA read and, if the
/// value happens to differ, a spurious action.
const TEXT_ROLES: &[&str] = &[
    "edit",
    "textbox",
    "passwordbox",
    "password",
    "searchbox",
    "combobox",
    // Notepad's editing surface. Excluded originally because the recorder's
    // trace was seen starting a tracker on a browser page's Document, which
    // looked like a false positive -- but a real Step 12 session then typed two
    // full lines into Notepad and captured ZERO type actions, because
    // `focus_moved` refused to watch the only element there is to watch.
    // Confirmed by probe: Notepad reports role "Document" and 76 typed
    // characters produced no action at all.
    "document",
];

/// Compared case-insensitively: Terminator reports `"Document"` from
/// `UIElement::role()` but `"document"` on a click event's `element_role`, and
/// both must match the same entry.
pub fn is_text_role(role: &str) -> bool {
    let role = role.trim().to_lowercase();
    TEXT_ROLES.iter().any(|r| role == *r)
}

/// The field currently being watched, and what it held when we started.
struct Watched {
    element: UIElement,
    role: String,
    name: Option<String>,
    /// Identifiers of the app that owns the field, captured from the click that
    /// focused it. The exclusion gate needs these, and resolving them later
    /// risks the element being gone.
    identifiers: Vec<String>,
    /// Value when we started watching, so we can emit only on a real change.
    initial: String,
    started_ms: u64,
    /// Printable/editing key-downs seen while watching. Reported as provenance;
    /// unlike the recorder's counter this never gates whether we emit.
    keystrokes: u32,
    /// Whether `initial` can be trusted as "the text before this edit".
    ///
    /// False when it came from a click, because the baseline is read when the
    /// click event is *processed* and that can land after typing has begun --
    /// measured at roughly one character late even with a 400ms settle. True
    /// only when we set it ourselves, immediately after emitting a flush, where
    /// there is no gap to lose characters in.
    ///
    /// Only a trusted baseline may be subtracted to form a delta. Subtracting
    /// an untrusted one silently drops the leading characters, which is how the
    /// first version of this regressed every A-E trial to `"0123456789..."`
    /// with the leading letter eaten.
    baseline_trusted: bool,
}

/// Follows focus across text fields and produces `Type` candidates.
///
/// Produces *candidates*, never `CapturedAction`s: everything it emits still
/// goes through `CapturedStream::admit` and the exclusion gate, exactly like an
/// action the recorder reported.
#[derive(Default)]
pub struct TextFieldWatcher {
    watching: Option<Watched>,
}

impl TextFieldWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Focus moved to `element`. Returns a candidate for the field being left.
    ///
    /// `identifiers` describes the app owning the newly focused element and is
    /// stored for use when that field is itself flushed later.
    pub fn focus_moved(
        &mut self,
        element: Option<&UIElement>,
        role: &str,
        name: Option<String>,
        identifiers: Vec<String>,
        timestamp_ms: u64,
    ) -> Option<ActionCandidate> {
        // Re-focusing the same field is not a transition; keep accumulating.
        if let (Some(new), Some(current)) = (element, self.watching.as_ref()) {
            if same_element(new, &current.element) {
                return None;
            }
        }

        let leaving = self.flush(timestamp_ms);

        self.watching = match element {
            Some(el) if is_text_role(role) => Some(Watched {
                initial: read_text(el).unwrap_or_default(),
                element: el.clone(),
                role: role.to_string(),
                name,
                identifiers,
                started_ms: timestamp_ms,
                keystrokes: 0,
                // Read at click-processing time, which can be later than the
                // click itself. Not safe to subtract.
                baseline_trusted: false,
            }),
            _ => None,
        };

        leaving
    }

    /// A key was pressed. Returns a candidate when the key finalises the field.
    ///
    /// Takes the raw key code rather than a pre-computed flag because Enter and
    /// Tab must be handled differently afterwards: Enter leaves focus where it
    /// is, Tab moves it to the next control. Re-watching after Tab would credit
    /// the *next* field's keystrokes to this one.
    pub fn key_pressed(&mut self, key_code: u32, timestamp_ms: u64) -> Option<ActionCandidate> {
        if !is_trigger_key(key_code) {
            if is_typing_key(key_code) {
                if let Some(w) = self.watching.as_mut() {
                    w.keystrokes += 1;
                }
            }
            return None;
        }

        if key_code == 0x09 {
            // Tab: focus is leaving. Flush and stop watching -- a click will
            // start watching whatever is focused next.
            return self.flush(timestamp_ms);
        }

        // Trigger key: emit what is there, then keep watching the same field
        // with a fresh baseline, so continued typing is recorded as a second
        // action rather than being lost or double-counted.
        //
        // The field details must be copied out FIRST: `flush` consumes the
        // watcher, so reading `self.watching` afterwards always yields None and
        // the re-watch below would silently never happen.
        let carry = self.watching.as_ref().map(|w| {
            (
                w.element.clone(),
                w.role.clone(),
                w.name.clone(),
                w.identifiers.clone(),
            )
        });

        let candidate = self.flush(timestamp_ms);

        if let Some((element, role, name, identifiers)) = carry {
            self.watching = Some(Watched {
                initial: read_text(&element).unwrap_or_default(),
                element,
                role,
                name,
                identifiers,
                started_ms: timestamp_ms,
                keystrokes: 0,
                // Read here, immediately after emitting, with no gap for
                // characters to slip into. This is the only baseline safe to
                // subtract -- and the only one that needs to be, since
                // duplication arises precisely from this re-watch.
                baseline_trusted: true,
            });
        }

        candidate
    }

    /// Read the watched field and produce a candidate if its text changed.
    ///
    /// Leaves the watcher empty. Called on focus change, on a trigger key, and
    /// once when the session stops.
    pub fn flush(&mut self, timestamp_ms: u64) -> Option<ActionCandidate> {
        let watched = self.watching.take()?;

        let current = read_text(&watched.element)?;

        // Two independent reasons to believe this field was typed into.
        //
        // A changed value is the obvious one. Observed keystrokes are the
        // necessary second one: `initial` is read when we PROCESS the click
        // that focused the field, and the recorder's click events can arrive
        // after the text has already landed -- measured deterministically with
        // `tests/ipc_pipeline.rs`-shaped input, where the baseline captured the
        // full typed string and the change test then suppressed the action.
        //
        // Requiring both would lose that case; requiring neither would invent
        // actions for fields merely clicked through. Either-or keeps the
        // pre-filled-field case safe, since clicking through one produces no
        // keystrokes and no change.
        let changed = current != watched.initial;
        let typed_into = watched.keystrokes > 0;
        if !changed && !typed_into {
            return None;
        }

        if current.trim().is_empty() {
            // Clearing a field is a real edit, but an empty payload carries no
            // information a replay could use, and the recorder skipped these
            // too. Treated the same way rather than silently differing.
            return None;
        }

        // Record what was ADDED since watching began, not the whole field.
        //
        // Replay types payloads at the caret without clearing first
        // (`element.type_text` -> `send_text`, key by key), so a field flushed
        // more than once would otherwise have every action repeat all the text
        // before it. Enter on a multi-line surface is the common way that
        // happens: it flushes and re-baselines, and the next flush re-reads the
        // whole document.
        //
        // Measured on a <textarea> before this change: two actions carrying
        // "alpha line" and "alpha line\nbeta line", which a replay writes out
        // as "alpha linealpha line\nbeta line" -- 30 characters for a 20
        // character field. A real user hit this as a doubled draft.
        //
        // This is deliberately not keyed on the field being multi-line. There
        // is no signal for that: a <textarea> reports role "Edit", identical to
        // a single-line <input>, and terminator exposes no multiline property.
        // Emitting the delta needs no such distinction.
        //
        // Only an append has a well-defined delta. Editing in the middle,
        // deleting, or replacing a selection falls back to the whole value --
        // which is exactly what this did before, so no case gets worse.
        let payload = if changed && watched.baseline_trusted {
            match current.strip_prefix(watched.initial.as_str()) {
                Some(added) => added.to_string(),
                None => current.clone(),
            }
        } else {
            // Either nothing changed (so the baseline was read too late to be
            // meaningful) or the baseline came from a click and cannot be
            // subtracted without risking the leading characters. Record the
            // whole value, exactly as before this change.
            current.clone()
        };

        if payload.is_empty() {
            return None;
        }

        let appended = payload.len() < current.len();
        let duration = timestamp_ms.saturating_sub(watched.started_ms);
        Some(ActionCandidate {
            kind: ActionKind::Type,
            identifiers: watched.identifiers,
            element_role: Some(watched.role),
            element_name: watched.name,
            payload: Some(payload),
            detail: Some(format!(
                "read from element ({}), {} keystroke(s) over {}ms",
                if appended { "appended" } else { "full value" },
                watched.keystrokes,
                duration
            )),
            timestamp_ms,
        })
    }

    /// Whether a field is currently being watched. For diagnostics.
    pub fn is_watching(&self) -> bool {
        self.watching.is_some()
    }

}

/// Read an element's text, or `None` if it cannot be read.
///
/// A failure here is expected rather than exceptional: the element may be gone
/// by the time focus leaves (dialog closed, page navigated). Losing the action
/// is strictly better than recording a wrong one, so there is no fallback.
fn read_text(element: &UIElement) -> Option<String> {
    element.text(0).ok()
}

/// Whether two handles refer to the same element.
///
/// Compared by id when both have one, falling back to role+name. Terminator
/// hands back a fresh `UIElement` per event, so pointer identity is never
/// available.
fn same_element(a: &UIElement, b: &UIElement) -> bool {
    match (a.id(), b.id()) {
        (Some(x), Some(y)) => x == y,
        _ => a.role() == b.role() && a.name() == b.name(),
    }
}

/// Key codes that finalise a field's contents. Matches the recorder's own
/// trigger set so behaviour is not gratuitously different.
pub fn is_trigger_key(key_code: u32) -> bool {
    key_code == 0x0D || key_code == 0x09 // Enter, Tab
}

/// Whether a key-down should count towards the reported keystroke total.
///
/// These are Windows *virtual key codes*, not ASCII. The distinction matters:
/// an ASCII range like `32..=126` looks right and quietly includes VK_LEFT
/// (0x25) and the other arrow keys, which are navigation rather than typing.
pub fn is_typing_key(key_code: u32) -> bool {
    matches!(
        key_code,
        0x30..=0x39     // 0-9
        | 0x41..=0x5A   // A-Z
        | 0x60..=0x69   // numpad 0-9
        | 0x6A..=0x6F   // numpad * + - . /
        | 0xBA..=0xC0   // OEM ; = , - . / `
        | 0xDB..=0xDF   // OEM [ \ ] '
        | 0x20          // space
        | 0x08          // backspace
        | 0x2E // delete
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_editable_roles() {
        assert!(is_text_role("Edit"));
        assert!(is_text_role("edit"));
        assert!(is_text_role("PasswordBox"));
        assert!(is_text_role("  combobox  "));

        // Notepad's editing surface. This assertion was previously inverted --
        // Document was excluded as a suspected false positive, and that is
        // exactly why a real session typing into Notepad captured nothing. The
        // role is matched case-insensitively because `UIElement::role()` gives
        // "Document" while a click event's `element_role` gives "document".
        assert!(is_text_role("Document"), "Notepad's editing surface");
        assert!(is_text_role("document"), "same role, as clicks report it");

        assert!(!is_text_role("Button"));
        assert!(!is_text_role("Text"));
        assert!(!is_text_role(""));
    }

    #[test]
    fn trigger_keys_match_the_recorders_set() {
        assert!(is_trigger_key(0x0D), "Enter");
        assert!(is_trigger_key(0x09), "Tab");
        assert!(!is_trigger_key(0x20), "Space is not a trigger");
        assert!(!is_trigger_key(0x41), "'A' is not a trigger");
    }

    #[test]
    fn typing_keys_cover_printables_and_editing_keys() {
        assert!(is_typing_key(0x41), "'A'");
        assert!(is_typing_key(0x39), "'9'");
        assert!(is_typing_key(0x60), "numpad 0");
        assert!(is_typing_key(0xBC), "OEM comma");
        assert!(is_typing_key(0x20), "space");
        assert!(is_typing_key(0x08), "backspace");
        assert!(is_typing_key(0x2E), "delete");

        assert!(!is_typing_key(0x11), "ctrl");
        assert!(!is_typing_key(0x10), "shift");
    }

    #[test]
    fn navigation_keys_are_not_typing() {
        // The whole arrow block sits inside the ASCII range 32..=126, which is
        // what an ASCII-shaped test would have wrongly accepted. These are
        // virtual key codes, so the ranges must be VK ranges.
        for (code, name) in [
            (0x25u32, "left"),
            (0x26, "up"),
            (0x27, "right"),
            (0x28, "down"),
            (0x24, "home"),
            (0x23, "end"),
            (0x21, "page up"),
            (0x22, "page down"),
        ] {
            assert!(!is_typing_key(code), "{name} arrow/navigation counted as typing");
        }
    }

    #[test]
    fn a_watcher_with_nothing_watched_flushes_to_nothing() {
        let mut w = TextFieldWatcher::new();
        assert!(!w.is_watching());
        assert!(w.flush(1).is_none());
        // Keys with no field watched must not panic or emit, whatever they are.
        assert!(w.key_pressed(0x41, 2).is_none(), "'A'");
        assert!(w.key_pressed(0x0D, 3).is_none(), "Enter");
        assert!(w.key_pressed(0x09, 4).is_none(), "Tab");
        assert!(!w.is_watching());
    }
}
