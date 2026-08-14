//! Opt-in supervision: stop and ask when a record is missing a mapped field.
//!
//! ## Why this is opt-in, and why that is not a hedge
//!
//! §4.4 is explicit about a record that is missing a field the source normally
//! has: **continue**, because "a blank cell isn't damaging", and log it for the
//! summary. §4.5 wants the user to be able to correct exactly that kind of
//! record — "this one order was weird" — which means stopping on it.
//!
//! Those two instructions are about the same condition and they disagree.
//! Making the loop always stop would overturn a rule §4.4 states in as many
//! words; never stopping leaves §4.5's one-off scope unreachable. So the
//! condition is unchanged and the *response* is the user's choice, defaulting
//! to the behaviour §4.4 documents.
//!
//! With supervision off — the default, and what every existing caller gets —
//! nothing here runs and the loop behaves exactly as it did.
//!
//! ## Asked once per record, not once per attempt
//!
//! Resuming re-reads the record, which is the existing clean-redo guarantee
//! (§4.6) doing its job: a correction applied while paused takes effect because
//! the record is read again from the start. But a record that is *still*
//! incomplete after the user resumed must not pause again — that is a loop that
//! never ends and a user who cannot get past a row they have decided to accept.
//!
//! So a record is asked about at most once. Resuming without correcting means
//! "write it as it is", which is precisely §4.4's behaviour, arrived at by the
//! user's decision rather than by default.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// The record a supervised run has stopped on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitingRecord {
    pub row_key: String,
    /// The mapped source columns that had nothing in them.
    pub missing_fields: Vec<String>,
}

/// Whether a run stops on incomplete records, and which one it stopped on.
///
/// Cloning shares the state, so the command layer can read what the loop is
/// waiting on -- the same shape as [`RunControl`] and [`RunCorrections`].
///
/// [`RunControl`]: crate::run::RunControl
/// [`RunCorrections`]: crate::run::correction::RunCorrections
#[derive(Clone)]
pub struct RunSupervision {
    enabled: bool,
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    awaiting: Option<AwaitingRecord>,
    asked: BTreeSet<String>,
}

impl Default for RunSupervision {
    /// Off. §4.4's documented behaviour is what a caller gets by not asking.
    fn default() -> Self {
        Self::off()
    }
}

impl std::fmt::Debug for RunSupervision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunSupervision")
            .field("enabled", &self.enabled)
            .field("awaiting", &self.awaiting())
            .finish()
    }
}

impl RunSupervision {
    pub fn off() -> Self {
        Self {
            enabled: false,
            inner: Arc::new(Mutex::new(State::default())),
        }
    }

    pub fn on() -> Self {
        Self {
            enabled: true,
            inner: Arc::new(Mutex::new(State::default())),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Should the loop stop on this record?
    ///
    /// False when supervision is off, and false for a record already asked
    /// about -- see the module docs on why asking twice is a trap rather than
    /// thoroughness.
    pub fn should_ask(&self, row_key: &str) -> bool {
        if !self.enabled {
            return false;
        }
        !self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .asked
            .contains(row_key)
    }

    /// Record that the loop has stopped on this record, and mark it asked.
    pub fn begin(&self, record: AwaitingRecord) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.asked.insert(record.row_key.clone());
        state.awaiting = Some(record);
    }

    /// The loop is moving on. Clears what it was waiting on, never the
    /// already-asked set -- that is what stops it asking again.
    pub fn finish(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .awaiting = None;
    }

    /// What the run is stopped on, for the correction panel.
    pub fn awaiting(&self) -> Option<AwaitingRecord> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .awaiting
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(row: &str) -> AwaitingRecord {
        AwaitingRecord {
            row_key: row.into(),
            missing_fields: vec!["C".into()],
        }
    }

    #[test]
    fn supervision_is_off_by_default() {
        // §4.4's behaviour is what a caller gets by not asking for anything.
        let s = RunSupervision::default();
        assert!(!s.enabled());
        assert!(!s.should_ask("2"));
    }

    #[test]
    fn an_off_supervision_never_asks_however_many_records_arrive() {
        let s = RunSupervision::off();
        for row in ["1", "2", "3"] {
            assert!(!s.should_ask(row));
        }
    }

    #[test]
    fn an_on_supervision_asks_about_a_record_once() {
        // Asking twice would be a loop the user cannot get past on a row they
        // have already decided to accept.
        let s = RunSupervision::on();
        assert!(s.should_ask("2"));
        s.begin(record("2"));
        assert!(!s.should_ask("2"), "already asked about row 2");
        assert!(s.should_ask("3"), "but row 3 has not been asked about");
    }

    #[test]
    fn finishing_clears_what_it_waits_on_but_not_what_it_has_asked() {
        let s = RunSupervision::on();
        s.begin(record("2"));
        assert_eq!(s.awaiting(), Some(record("2")));

        s.finish();
        assert_eq!(s.awaiting(), None, "nothing is being waited on now");
        assert!(
            !s.should_ask("2"),
            "but row 2 must not be asked about a second time"
        );
    }

    #[test]
    fn clones_share_the_state() {
        // The command layer reads what the run loop is waiting on.
        let a = RunSupervision::on();
        let b = a.clone();
        a.begin(record("7"));
        assert_eq!(b.awaiting(), Some(record("7")));
        b.finish();
        assert_eq!(a.awaiting(), None);
    }
}
