//! Stop and Pause. §4.6 treats them as genuinely different things, and so does
//! this.
//!
//! ## The two are not degrees of the same control
//!
//! | | Pause | Stop |
//! |---|---|---|
//! | duration | temporary | permanent |
//! | in-progress record | abandoned, redone from the start on resume | finished cleanly |
//! | can be undone | yes, by resuming | **no** |
//!
//! Stop being permanent is enforced here rather than asked of callers:
//! [`RunControl::resume`] moves `Paused -> Running` and nothing else, so a
//! resume arriving after a stop -- a double-click, a stale button, a frontend
//! that lost track -- cannot restart a run the user ended.
//!
//! ## Why pause blocks rather than returning
//!
//! A paused run holds its position in the source and its place in the loop. If
//! pausing returned control to the caller, resuming would mean reconstructing
//! all of that, and §4.6's guarantee -- resume redoes the in-progress record
//! cleanly from the beginning -- would become the caller's problem to get
//! right. Blocking on a condition variable keeps it the loop's problem, which
//! is the only place that knows where the record started.
//!
//! This is safe precisely because runs execute on the app's own background
//! thread (§4.10). Nothing on the UI thread waits on this.

use std::sync::{Arc, Condvar, Mutex};

/// What a run has been told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    Running,
    Paused,
    /// Terminal. Nothing moves out of this state.
    Stopped,
}

/// A handle to a run's Stop/Pause controls.
///
/// Cloning shares the same underlying state, so the UI holds one and the
/// running loop holds another.
#[derive(Clone)]
pub struct RunControl {
    inner: Arc<(Mutex<ControlState>, Condvar)>,
}

impl Default for RunControl {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RunControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunControl")
            .field("state", &self.state())
            .finish()
    }
}

impl RunControl {
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(ControlState::Running), Condvar::new())),
        }
    }

    /// A control that is already stopped. For a run that must not start.
    pub fn stopped() -> Self {
        let c = Self::new();
        c.stop();
        c
    }

    pub fn state(&self) -> ControlState {
        *self.inner.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ask the run to pause at its next safe point.
    ///
    /// Ignored once stopped: a paused run can be stopped, but a stopped run
    /// cannot be pushed back into a pausable state, which would imply it might
    /// still resume.
    pub fn pause(&self) {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *state == ControlState::Running {
            *state = ControlState::Paused;
        }
        cvar.notify_all();
    }

    /// Resume a paused run. `Paused -> Running` only.
    ///
    /// Returns whether it actually resumed, so a caller can tell "resumed" from
    /// "that run was already over" rather than assuming.
    pub fn resume(&self) -> bool {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        let resumed = *state == ControlState::Paused;
        if resumed {
            *state = ControlState::Running;
        }
        cvar.notify_all();
        resumed
    }

    /// End the run. Permanent, from any state.
    pub fn stop(&self) {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        *state = ControlState::Stopped;
        // Wakes a loop blocked in `wait_while_paused`, which is how Stop
        // reaches a run that is already paused.
        cvar.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        self.state() == ControlState::Stopped
    }

    /// Cheap, non-blocking: has a pause been asked for?
    ///
    /// Separate from [`wait_while_paused`](Self::wait_while_paused) so the
    /// write path can ask without any chance of blocking on the common answer.
    pub fn is_paused(&self) -> bool {
        self.state() == ControlState::Paused
    }

    /// Block while paused. Returns `true` if the run should carry on.
    ///
    /// `false` means stopped -- either stopped outright, or stopped while it
    /// sat paused, which is §4.6's "fully discarded" arm.
    pub fn wait_while_paused(&self) -> bool {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        while *state == ControlState::Paused {
            state = cvar.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        *state != ControlState::Stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn a_new_control_is_running_and_does_not_block() {
        let c = RunControl::new();
        assert_eq!(c.state(), ControlState::Running);
        assert!(!c.is_paused());
        assert!(c.wait_while_paused(), "a running control must not block");
    }

    #[test]
    fn stop_is_permanent_and_resume_cannot_undo_it() {
        // §4.6: "Stop: hard, permanent." Enforced here so a stale resume from
        // the UI cannot restart a run the user ended.
        let c = RunControl::new();
        c.stop();
        assert!(c.is_stopped());
        assert!(!c.resume(), "resume must report that it did nothing");
        assert_eq!(c.state(), ControlState::Stopped);
        assert!(!c.wait_while_paused());
    }

    #[test]
    fn pausing_a_stopped_run_does_not_revive_it() {
        let c = RunControl::new();
        c.stop();
        c.pause();
        assert_eq!(
            c.state(),
            ControlState::Stopped,
            "pause must not move a run out of the terminal state"
        );
    }

    #[test]
    fn a_paused_control_blocks_until_resumed() {
        let c = RunControl::new();
        c.pause();
        assert!(c.is_paused());

        let (tx, rx) = mpsc::channel();
        let waiter = c.clone();
        let handle = thread::spawn(move || {
            let carry_on = waiter.wait_while_paused();
            tx.send(carry_on).expect("send");
        });

        // It really is blocked: nothing arrives while it stays paused.
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "wait_while_paused returned while still paused"
        );

        c.resume();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "resuming must release the waiter and tell it to carry on"
        );
        handle.join().expect("join");
    }

    #[test]
    fn stopping_a_paused_control_releases_it_with_a_halt() {
        // §4.6's "fully discarded" arm: Stop reaching a run that is sitting
        // paused mid-record.
        let c = RunControl::new();
        c.pause();

        let (tx, rx) = mpsc::channel();
        let waiter = c.clone();
        let handle = thread::spawn(move || {
            tx.send(waiter.wait_while_paused()).expect("send");
        });

        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
        c.stop();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(false),
            "a stop must release the waiter and tell it to halt"
        );
        handle.join().expect("join");
    }

    #[test]
    fn resume_reports_whether_there_was_anything_to_resume() {
        let c = RunControl::new();
        assert!(!c.resume(), "a running control was not resumed");
        c.pause();
        assert!(c.resume(), "a paused control was");
    }

    #[test]
    fn clones_share_one_state() {
        let a = RunControl::new();
        let b = a.clone();
        a.pause();
        assert_eq!(b.state(), ControlState::Paused);
        b.stop();
        assert_eq!(a.state(), ControlState::Stopped);
    }
}
