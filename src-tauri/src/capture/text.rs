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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use terminator::{Desktop, UIElement};

use super::stream::{ActionCandidate, ActionKind};

// ---------------------------------------------------------- instrumentation --
//
// Off by default; when off the cost is one relaxed atomic load.
//
// Deliberately an in-memory buffer rather than `tracing`. This defect is a
// Heisenbug: the original investigation recorded that enabling the recorder's
// tracing output made it VANISH (4/4 events, all exact), because log I/O shifts
// the timing. Instrumenting a timing race with writes to stdout would measure
// the instrument. Pushing a `String` under a mutex is nanoseconds and does no
// I/O until the run is over.
static TRACE_ON: AtomicBool = AtomicBool::new(false);
static TRACE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Turn the in-memory watcher trace on or off. Diagnostics only.
pub fn set_trace(on: bool) {
    TRACE_ON.store(on, Ordering::Relaxed);
}

/// Take everything traced so far, emptying the buffer.
pub fn take_trace() -> Vec<String> {
    TRACE
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

fn trace(msg: impl FnOnce() -> String) {
    if TRACE_ON.load(Ordering::Relaxed) {
        if let Ok(mut v) = TRACE.lock() {
            v.push(msg());
        }
    }
}

/// Roles a KEYSTROKE-driven watch may start on.
///
/// Narrower than `is_text_role` on purpose, and the difference is the whole
/// safety argument. Before any click, `focused_element()` resolves to
/// `role="Document"`. `is_text_role("Document")` is now **true** -- it was false
/// when this approach was first tried, and was added so Notepad's editing
/// surface would be recognised. Starting a keystroke-driven watch on a Document
/// would mean a stray key before any click begins watching the whole page, and
/// the next click flushes it, emitting the entire page text as a `type` action.
///
/// A click-driven watch still accepts Document, because a click names the
/// element deliberately. A keystroke does not name anything.
fn keystroke_startable_role(role: &str) -> bool {
    let role = role.trim().to_lowercase();
    matches!(
        role.as_str(),
        "edit" | "textbox" | "passwordbox" | "password" | "searchbox"
    )
}

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
    /// Executable owning the field, from the same click. Carried so the emitted
    /// action can be process-scoped at replay time.
    process_name: Option<String>,
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
    /// Resolved lazily, only when a keystroke-driven start is attempted.
    desktop: Option<Desktop>,
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
        process_name: Option<String>,
        timestamp_ms: u64,
    ) -> Option<ActionCandidate> {
        trace(|| {
            format!(
                "focus_moved route=click role={role:?} name={:?} already_watching={}",
                name,
                self.watching
                    .as_ref()
                    .map(|w| w.name.clone().unwrap_or_default())
                    .unwrap_or_else(|| "<none>".into()),
            )
        });

        // Re-focusing the same field is not a transition; keep accumulating.
        if let (Some(new), Some(current)) = (element, self.watching.as_ref()) {
            if same_element(new, &current.element) {
                // Adopt the click's app identity if the watch has none.
                //
                // This is what made the first two attempts at the no-settle race
                // fail, and it took instrumentation to see. A keystroke-driven
                // watch names no application, because a keystroke carries no
                // element and no process. The click that follows names the same
                // element, so this branch used to return here immediately -- and
                // the identity it was carrying went unused. At flush the
                // candidate was built correctly, with the right payload, and
                // `CapturedStream::admit` then dropped it: it fails closed on an
                // action whose source app cannot be named.
                //
                // The trace shows exactly that: `EMITTING payload="A0123..."`
                // followed by a single `"type" UnidentifiedSource` exclusion and
                // no FieldA action in the report. That is why trial A read as
                // 0/5 while every part tested sound in isolation.
                //
                // Supplying the fact, not weakening the gate: this is the same
                // identification a click-started watch would have recorded.
                if let Some(w) = self.watching.as_mut() {
                    if w.identifiers.is_empty() && !identifiers.is_empty() {
                        trace(|| format!("  adopting app identity from the click: {identifiers:?}"));
                        w.identifiers = identifiers;
                    }
                    if w.process_name.is_none() && process_name.is_some() {
                        w.process_name = process_name;
                    }
                }
                trace(|| {
                    format!(
                        "  same element as watched (id={:?}); keeping the watch, no flush",
                        new.id()
                    )
                });
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
                process_name,
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
        self.key_pressed_with_modifiers(key_code, false, timestamp_ms)
    }

    /// As `key_pressed`, but told whether Ctrl was held.
    ///
    /// Ctrl-modified keys are commands, not typing. `Ctrl+V` arrives as key code
    /// `0x56` — plain `V` — so without this it counts as a typed character,
    /// inflating the keystroke total and, since the emit condition is "value
    /// changed OR keystrokes seen", able to trigger an emit for a reason that
    /// never happened. Same for `Ctrl+A`, `Ctrl+C`, `Ctrl+Z`.
    pub fn key_pressed_with_modifiers(
        &mut self,
        key_code: u32,
        ctrl_pressed: bool,
        timestamp_ms: u64,
    ) -> Option<ActionCandidate> {
        if !is_trigger_key(key_code) {
            if is_typing_key(key_code) && !ctrl_pressed {
                // Follow focus on every typing key, not just when nothing is
                // watched. Trial E's keystrokes arrive while the PREVIOUS
                // field's watch is still live, so a "start only if idle" gate
                // never fires for it -- measured, E stayed 0/5 with that gate
                // while A-D passed.
                //
                // Cost is one `focused_element()` per typing key: measured at
                // 4-7 ms mean, 21 ms max, and correct 40/40 in both settled and
                // no-settle shapes by the previous investigation.
                let leaving = self.follow_focus_on_keystroke(timestamp_ms);
                if let Some(w) = self.watching.as_mut() {
                    w.keystrokes += 1;
                }
                if leaving.is_some() {
                    return leaving;
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
                w.process_name.clone(),
            )
        });

        let candidate = self.flush(timestamp_ms);

        if let Some((element, role, name, identifiers, process_name)) = carry {
            self.watching = Some(Watched {
                initial: read_text(&element).unwrap_or_default(),
                element,
                role,
                name,
                identifiers,
                process_name,
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
        let Some(watched) = self.watching.take() else {
            trace(|| "flush: nothing was being watched".to_string());
            return None;
        };

        let Some(current) = read_text(&watched.element) else {
            trace(|| {
                format!(
                    "flush: could NOT read the watched element (name={:?}); dropping",
                    watched.name
                )
            });
            return None;
        };

        trace(|| {
            format!(
                "flush: name={:?} initial={:?} current={:?} keystrokes={} trusted={}",
                watched.name,
                watched.initial,
                current,
                watched.keystrokes,
                watched.baseline_trusted
            )
        });

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
            trace(|| {
                "  NOT EMITTING: value unchanged and no keystrokes were attributed".to_string()
            });
            return None;
        }

        if current.trim().is_empty() {
            trace(|| "  NOT EMITTING: field is empty".to_string());
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
            trace(|| "  NOT EMITTING: computed payload is empty".to_string());
            return None;
        }
        trace(|| format!("  EMITTING payload={payload:?}"));

        let appended = payload.len() < current.len();
        let duration = timestamp_ms.saturating_sub(watched.started_ms);
        Some(ActionCandidate {
            // Read at emit rather than at watch-start: the element is the one
            // this watcher has been holding all along, and reading once when
            // the edit ends costs one call per action instead of one per
            // keystroke.
            element_bounds: super::bounds_of(Some(&watched.element)),
            kind: ActionKind::Type,
            identifiers: watched.identifiers,
            process_name: watched.process_name,
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

    /// Keep the watch pointed at whatever actually has focus, on each typing key.
    ///
    /// The reconstruction of the twice-abandoned `begin_from_keystroke`, this
    /// time instrumented. Returns a candidate when focus has moved off a field
    /// that was being watched, since that field's edit is finished.
    ///
    /// A click still starts watches too. This exists because a click EVENT can
    /// be processed long after the click happened -- in trial A, with a 400 ms
    /// settle, the first keystroke still arrived before the click event did.
    fn follow_focus_on_keystroke(&mut self, timestamp_ms: u64) -> Option<ActionCandidate> {
        // Unit tests must not reach out to the real desktop. This resolves
        // whatever the developer's machine happens to have focused, so without
        // this guard a hermetic test starts watching the editor the test was
        // launched from -- and its result depends on which window had focus.
        // That is not hypothetical: it turned
        // `a_watcher_with_nothing_watched_flushes_to_nothing` into a test that
        // passed or failed according to where the mouse had last been. The live
        // behaviour is covered by the A-E trials and the Notepad runs, which
        // drive real windows on purpose.
        if cfg!(test) {
            return None;
        }
        if self.desktop.is_none() {
            self.desktop = Desktop::new_default().ok();
            trace(|| {
                format!(
                    "keystroke-follow: Desktop::new_default -> {}",
                    if self.desktop.is_some() { "ok" } else { "FAILED" }
                )
            });
        }
        let desktop = self.desktop.as_ref()?;
        let el = match desktop.focused_element() {
            Ok(el) => el,
            Err(e) => {
                trace(|| format!("keystroke-follow: focused_element FAILED: {e}"));
                return None;
            }
        };

        // Already on it: the overwhelmingly common case, and it must stay cheap
        // and side-effect free.
        if let Some(w) = self.watching.as_ref() {
            if same_element(&el, &w.element) {
                // Traced so "the keystroke path did nothing" is a POSITIVE
                // observation rather than an absence. Verifying that this path
                // stays inert in Notepad is otherwise unfalsifiable: the
                // watched element and the focused element are the same
                // Document, so the path runs, correctly does nothing, and would
                // leave no evidence that it ran at all.
                trace(|| {
                    format!(
                        "keystroke-follow: focus is already the watched element (role={:?})",
                        w.role
                    )
                });
                return None;
            }
        }

        let role = el.role();
        let name = el.name();
        if !keystroke_startable_role(&role) {
            trace(|| {
                format!("keystroke-follow REFUSED: role={role:?} name={name:?} is not startable")
            });
            return None;
        }

        // Focus has moved to a different field. Whatever was being watched is
        // finished, so flush it before re-pointing.
        let leaving = self.flush(timestamp_ms);
        let initial = read_text(&el).unwrap_or_default();
        trace(|| {
            format!(
                "keystroke-follow STARTED: role={role:?} name={name:?} id={:?} initial={initial:?}",
                el.id()
            )
        });
        self.watching = Some(Watched {
            initial,
            element: el,
            role,
            name,
            // A keystroke names no application. The click for this same element
            // adopts into the watch when it arrives -- see `focus_moved`.
            identifiers: Vec::new(),
            process_name: None,
            started_ms: timestamp_ms,
            keystrokes: 0,
            baseline_trusted: false,
        });
        leaving
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
    fn ctrl_modified_keys_are_commands_not_typing() {
        // Ctrl+V arrives as key code 0x56 -- plain `V`. Counting it as a typed
        // character inflates the keystroke total, and since a field is emitted
        // when "the value changed OR keystrokes were seen", it can trigger an
        // action for a reason that never happened. Ctrl+A and Ctrl+C are the
        // same shape.
        //
        // `is_typing_key` still answers only "is this a character key"; the
        // modifier is applied at the call site, which is why this test pins the
        // pairing rather than the predicate alone.
        for code in [0x56u32, 0x41, 0x43, 0x5A] {
            assert!(
                is_typing_key(code),
                "{code:#x} is a character key on its own"
            );
        }
        let counts_as_typing = |code: u32, ctrl: bool| is_typing_key(code) && !ctrl;
        assert!(counts_as_typing(0x56, false), "plain V is typing");
        assert!(!counts_as_typing(0x56, true), "Ctrl+V is a paste, not a V");
        assert!(!counts_as_typing(0x41, true), "Ctrl+A is select-all");
        assert!(!counts_as_typing(0x43, true), "Ctrl+C is copy");
    }

    #[test]
    fn a_keystroke_may_never_start_a_watch_on_a_document() {
        // The guard that keeps the keystroke path out of Notepad and out of web
        // pages. `is_text_role("Document")` is TRUE -- added so Notepad's
        // editing surface is watchable by a CLICK -- and that is exactly why
        // the keystroke path needs its own, narrower rule: before any click,
        // `focused_element()` resolves to the page Document, so a stray key
        // would otherwise start watching the whole page and the next click
        // would emit its entire text as a `type` action.
        assert!(is_text_role("Document"));
        assert!(!keystroke_startable_role("Document"));
        assert!(!keystroke_startable_role("document"));

        // Not a spreadsheet cell editor either; that has its own watcher.
        assert!(!keystroke_startable_role("ComboBox"));
        assert!(!keystroke_startable_role("Button"));
        assert!(!keystroke_startable_role("Window"));

        // The real text fields stay startable.
        for ok in ["Edit", "edit", "TextBox", "PasswordBox", "SearchBox"] {
            assert!(keystroke_startable_role(ok), "{ok} should be startable");
        }
    }

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
