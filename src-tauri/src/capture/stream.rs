//! The captured stream and the single gate into it.
//!
//! The structural guarantee this module exists to provide:
//!
//!   * `CapturedStream::actions` is private to this module, and there is no
//!     `push`, no `&mut` accessor, and no public constructor for the Vec.
//!   * [`CapturedStream::admit`] is the only code path that can append, and it
//!     runs the exclusion check before it constructs anything.
//!   * [`CapturedAction`] carries a private field, so it cannot be built by
//!     external code and smuggled in even if another insertion path were added.
//!
//! Together those mean an action from an excluded application is never held --
//! the payload is dropped with the candidate, not stored and filtered later.

use super::exclusion::ExclusionList;

/// Matches the `action_type` enum in migration 20260803000001.
///
/// `Read` is part of the locked schema enum but is never produced by the Step 3
/// recorder mapping -- reading a value is not an input event, so low-level
/// hooks cannot observe it. It exists here so the variant set matches the
/// schema and so downstream policy can handle it, rather than being bolted on
/// when a later phase starts emitting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Click,
    Type,
    Navigate,
    Read,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Click => "click",
            ActionKind::Type => "type",
            ActionKind::Navigate => "navigate",
            ActionKind::Read => "read",
        }
    }
}

/// An observed action that has NOT yet passed the gate.
///
/// Deliberately a distinct type from [`CapturedAction`]: holding an ungated
/// observation must never be mistakable for holding a captured one.
#[derive(Debug, Clone)]
pub struct ActionCandidate {
    pub kind: ActionKind,
    /// Every identifier we have for the source application: process name,
    /// window title, UIA application name. All are checked.
    pub identifiers: Vec<String>,
    /// The owning process's executable name, when the event reported one.
    ///
    /// Kept as its own field rather than left in `identifiers`, because
    /// `identifiers` does not survive the gate: `admit` keeps only the first
    /// non-empty entry as `source_app` and drops the rest. Replay needs this
    /// specifically -- `Locator::all()` refuses a desktop-wide selector and
    /// demands a `process:` prefix, so without it ambiguity cannot be counted.
    pub process_name: Option<String>,
    pub element_role: Option<String>,
    pub element_name: Option<String>,
    /// Typed text and similar. Dropped, never stored, when excluded.
    pub payload: Option<String>,
    pub detail: Option<String>,
    /// The acted-on element's rectangle, `(x, y, width, height)`.
    ///
    /// **Width and height, not a right and bottom edge.** `terminator-rs`
    /// returns `rect.get_left(), get_top(), get_width(), get_height()`
    /// (`platforms/windows/element.rs:630`). Mislabelled as right/bottom when
    /// this field was added on 2026-08-18, and corrected on 2026-08-20 after an
    /// analysis read the third value as a right edge and got a number smaller
    /// than the left one on every row.
    ///
    /// A positional identity that does not collapse when two elements say the
    /// same thing. `element_name` cannot do that job, and neither can the
    /// platform's element id: `terminator-rs` synthesises the id by hashing
    /// role + name, so two records sharing a value share an id -- see
    /// `docs/known-issues/element-id-is-a-hash-of-the-text.md`.
    ///
    /// **A position, never content**, so it is clean under §3 for the same
    /// reason a cell reference is.
    ///
    /// `None` whenever the element could not be resolved or would not report
    /// bounds, which is a real and ordinary outcome rather than an error.
    pub element_bounds: Option<(f64, f64, f64, f64)>,
    pub timestamp_ms: u64,
}

/// Proof-of-gate token. Private, so `CapturedAction` cannot be constructed
/// outside this module.
#[derive(Debug, Clone, Copy)]
struct Gated;

/// An action that has passed the exclusion gate.
#[derive(Debug, Clone)]
pub struct CapturedAction {
    pub kind: ActionKind,
    pub source_app: String,
    /// Executable name of the owning process, when known. `source_app` is a
    /// display string -- for a window switch it is the window TITLE -- and a
    /// title changes as the user works. This is the stable half.
    pub process_name: Option<String>,
    pub element_role: Option<String>,
    pub element_name: Option<String>,
    pub payload: Option<String>,
    pub detail: Option<String>,
    /// See [`ActionCandidate::element_bounds`]. Positional identity, kept
    /// because names and platform element ids both collapse on equal text.
    pub element_bounds: Option<(f64, f64, f64, f64)>,
    pub timestamp_ms: u64,
    _gated: Gated,
}

/// Why an observation was refused. Records what matched, never the payload --
/// an exclusion record must not become the leak it exists to prevent.
#[derive(Debug, Clone)]
pub struct ExclusionRecord {
    pub kind: ActionKind,
    pub reason: ExclusionReason,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone)]
pub enum ExclusionReason {
    /// Source application matched an exclusion pattern.
    Pattern { pattern: String, matched_on: String },
    /// The source application could not be identified at all.
    ///
    /// Refused rather than admitted: the gate can only exclude what it can
    /// name, so an unidentifiable source is precisely the case where we cannot
    /// prove the action is safe to keep. Admitting it "because nothing
    /// matched" is a bypass -- an empty identifier set matches no pattern by
    /// definition.
    UnidentifiedSource,
}

impl ExclusionReason {
    pub fn describe(&self) -> String {
        match self {
            ExclusionReason::Pattern {
                pattern,
                matched_on,
            } => format!("matched pattern {pattern:?} on {matched_on:?}"),
            ExclusionReason::UnidentifiedSource => {
                "source application could not be identified (fail closed)".to_string()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum Admission {
    Admitted,
    Excluded(ExclusionReason),
}

pub struct CapturedStream {
    actions: Vec<CapturedAction>,
    exclusions: Vec<ExclusionRecord>,
    list: ExclusionList,
}

impl CapturedStream {
    pub fn new(list: ExclusionList) -> Self {
        Self {
            actions: Vec::new(),
            exclusions: Vec::new(),
            list,
        }
    }

    /// The ONLY way an action can enter the stream.
    ///
    /// Runs the exclusion check first and returns without storing anything if
    /// it matches. `candidate` is consumed either way, so an excluded payload
    /// has no surviving owner.
    pub fn admit(&mut self, candidate: ActionCandidate) -> Admission {
        // Fail closed BEFORE pattern matching. An empty identifier set matches
        // no pattern, so checking patterns first would silently admit exactly
        // the actions we know least about.
        let source_app = candidate
            .identifiers
            .iter()
            .find(|s| !s.trim().is_empty())
            .cloned();

        let Some(source_app) = source_app else {
            self.exclusions.push(ExclusionRecord {
                kind: candidate.kind,
                reason: ExclusionReason::UnidentifiedSource,
                timestamp_ms: candidate.timestamp_ms,
            });
            return Admission::Excluded(ExclusionReason::UnidentifiedSource);
        };

        let borrowed: Vec<&str> = candidate.identifiers.iter().map(String::as_str).collect();

        if let Some(hit) = self.list.matches(borrowed) {
            let reason = ExclusionReason::Pattern {
                pattern: hit.pattern,
                matched_on: hit.matched_on,
            };
            self.exclusions.push(ExclusionRecord {
                kind: candidate.kind,
                reason: reason.clone(),
                timestamp_ms: candidate.timestamp_ms,
            });
            // candidate (and its payload) is dropped here, unstored.
            return Admission::Excluded(reason);
        }

        self.actions.push(CapturedAction {
            kind: candidate.kind,
            source_app,
            process_name: candidate.process_name,
            element_role: candidate.element_role,
            element_name: candidate.element_name,
            payload: candidate.payload,
            detail: candidate.detail,
            element_bounds: candidate.element_bounds,
            timestamp_ms: candidate.timestamp_ms,
            _gated: Gated,
        });

        Admission::Admitted
    }

    pub fn actions(&self) -> &[CapturedAction] {
        &self.actions
    }

    pub fn exclusions(&self) -> &[ExclusionRecord] {
        &self.exclusions
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(app: &str, payload: &str) -> ActionCandidate {
        ActionCandidate {
            element_bounds: None,
            kind: ActionKind::Type,
            identifiers: vec![app.to_string()],
            process_name: None,
            element_role: Some("Edit".into()),
            element_name: Some("field".into()),
            payload: Some(payload.to_string()),
            detail: None,
            timestamp_ms: 1,
        }
    }

    #[test]
    fn ordinary_application_is_admitted() {
        let mut s = CapturedStream::new(ExclusionList::placeholder());
        assert!(matches!(
            s.admit(candidate("notepad.exe", "hello")),
            Admission::Admitted
        ));
        assert_eq!(s.len(), 1);
        assert_eq!(s.actions()[0].payload.as_deref(), Some("hello"));
    }

    #[test]
    fn excluded_application_never_enters_the_stream() {
        let mut s = CapturedStream::new(ExclusionList::placeholder());
        let admission = s.admit(candidate("KeePassXC.exe", "hunter2"));

        assert!(matches!(admission, Admission::Excluded { .. }));
        assert!(s.is_empty(), "an excluded action was stored");
        assert_eq!(s.exclusions().len(), 1);
    }

    #[test]
    fn excluded_payload_is_not_retained_anywhere() {
        let mut s = CapturedStream::new(ExclusionList::placeholder());
        s.admit(candidate("my-password-manager.exe", "s3cret-passphrase"));

        // The whole point: the secret must not survive in the stream OR in the
        // exclusion audit trail.
        let anywhere = format!("{:?}{:?}", s.actions(), s.exclusions());
        assert!(
            !anywhere.contains("s3cret-passphrase"),
            "excluded payload leaked into retained state: {anywhere}"
        );
    }

    #[test]
    fn window_title_exclusion_beats_innocuous_process_name() {
        let mut s = CapturedStream::new(ExclusionList::placeholder());
        let mut c = candidate("chrome.exe", "account number");
        c.identifiers.push("Barclays Bank - Google Chrome".into());

        assert!(matches!(s.admit(c), Admission::Excluded { .. }));
        assert!(s.is_empty());
    }

    #[test]
    fn exclusion_record_names_what_triggered_it() {
        let mut s = CapturedStream::new(ExclusionList::from_patterns(["bank"]));
        let mut c = candidate("chrome.exe", "x");
        c.identifiers.push("Bank of X".into());
        s.admit(c);

        match &s.exclusions()[0].reason {
            ExclusionReason::Pattern {
                pattern,
                matched_on,
            } => {
                assert_eq!(pattern, "bank");
                assert_eq!(matched_on, "Bank of X");
            }
            other => panic!("wrong reason: {other:?}"),
        }
    }

    #[test]
    fn unidentifiable_source_is_refused_not_admitted() {
        // Observed live: a click whose source application could not be
        // resolved. It previously sailed through as "<unknown>", because an
        // empty identifier set matches no exclusion pattern. That is a bypass.
        let mut s = CapturedStream::new(ExclusionList::placeholder());
        let mut c = candidate("", "whatever was typed here");
        c.identifiers = vec![];

        let admission = s.admit(c);

        assert!(
            matches!(
                admission,
                Admission::Excluded(ExclusionReason::UnidentifiedSource)
            ),
            "unidentified source was not refused: {admission:?}"
        );
        assert!(s.is_empty(), "an unidentifiable action entered the stream");
    }

    #[test]
    fn blank_identifiers_count_as_unidentified() {
        let mut s = CapturedStream::new(ExclusionList::placeholder());
        let mut c = candidate("", "payload");
        c.identifiers = vec!["".into(), "   ".into()];

        assert!(matches!(
            s.admit(c),
            Admission::Excluded(ExclusionReason::UnidentifiedSource)
        ));
        assert!(s.is_empty());
    }

    #[test]
    fn unidentified_refusal_does_not_retain_the_payload() {
        let mut s = CapturedStream::new(ExclusionList::placeholder());
        let mut c = candidate("", "unattributed-secret");
        c.identifiers = vec![];
        s.admit(c);

        let anywhere = format!("{:?}{:?}", s.actions(), s.exclusions());
        assert!(!anywhere.contains("unattributed-secret"), "{anywhere}");
    }
}
