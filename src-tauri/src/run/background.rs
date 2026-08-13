//! Running a workflow on the app's own background thread (§4.10).
//!
//! > "The user can keep working in the app while a run is in progress -- it
//! > does not lock the window. Minimizing the app is fine and does not
//! > interrupt a run; fully closing the app does, since this executes on the
//! > app's own background thread rather than surviving independently of it."
//!
//! Those three properties are not three features. They all follow from one
//! choice -- a plain `std::thread` owned by the process -- and it is worth
//! being explicit about why each holds:
//!
//! * **Non-blocking**: the command that starts a run returns as soon as the
//!   thread is spawned. Nothing on the UI thread waits on it, including a
//!   pause, which blocks only the run thread.
//! * **Survives minimize**: minimizing is a window-manager event. A thread
//!   that never touches the window loop cannot notice it.
//! * **Does not survive close**: the thread is not detached from the process,
//!   so process exit ends it. This is the property that has to be *preserved*
//!   rather than built -- writing the run into a service or a detached process
//!   would break it, which is exactly what §4.10 says not to do.
//!
//! ## Why the surfaces are built on the thread, not handed to it
//!
//! [`spawn`] takes a factory rather than a reader and a writer. `UIElement` is
//! `Send` -- measured, it compiles -- so moving them would type-check. But UI
//! Automation handles are COM objects, and COM cares which *apartment* a call
//! is made from, which the type system does not model. Building them on the
//! thread that will use them makes the question moot instead of relying on an
//! answer the compiler cannot check.
//!
//! It also fails in the right place: a source that cannot be opened becomes a
//! failed run with a reason, not a panic on a thread nobody is watching.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::compile::CompiledTemplate;
use crate::run::{
    run_with_control, set_run_state, DestinationWriter, RunControl, RunReport, RunState,
};
use crate::source::SourceReader;

/// What a run needs in order to touch the world, built on the run's own thread.
pub type Surfaces = (Box<dyn SourceReader>, Box<dyn DestinationWriter>);

/// Builds those surfaces. Fallible, because opening a live document is.
pub type SurfaceFactory = Box<dyn FnOnce() -> Result<Surfaces, String> + Send + 'static>;

/// How a run ended, once it has.
#[derive(Debug, Clone)]
pub enum RunOutcome {
    Finished(RunReport),
    /// The run never got as far as its loop -- the database or the source could
    /// not be opened. Distinct from a `RunStop`, which is a loop that ran and
    /// then ended for a reason.
    Failed(String),
}

/// A run in progress, and the controls for it.
pub struct ActiveRun {
    pub playbook_id: String,
    pub control: RunControl,
    outcome: Arc<Mutex<Option<RunOutcome>>>,
    handle: Option<JoinHandle<()>>,
}

impl ActiveRun {
    /// The outcome, if the run has ended. `None` while it is still going.
    pub fn outcome(&self) -> Option<RunOutcome> {
        self.outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }

    /// Wait for the run to end on its own and take its outcome.
    ///
    /// **Blocks for as long as the run is paused**, because a paused run has
    /// not ended -- that is what pause means. A caller that wants the answer
    /// regardless should [`resume`](RunControl::resume) or use
    /// [`stop_and_join`](Self::stop_and_join) first.
    ///
    /// This deliberately does NOT stop the run. An earlier version did, to rule
    /// out that block, and it silently turned every completed run into a
    /// stopped one: the stop landed before the loop had finished, so a run that
    /// should have reported `Exhausted` reported `Stopped` at its first record.
    /// Waiting and ending are different intentions and now have different
    /// methods.
    pub fn join(mut self) -> Option<RunOutcome> {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.outcome()
    }

    /// End the run, then wait for it. §4.6's Stop, followed by its answer.
    pub fn stop_and_join(self) -> Option<RunOutcome> {
        self.control.stop();
        self.join()
    }
}

/// Start a run on its own thread.
///
/// The thread opens its own database connection rather than sharing the app's.
/// A run can sit paused indefinitely, and holding the app's connection across
/// that would block every other command -- which is precisely the "it does not
/// lock the window" property §4.10 asks for.
pub fn spawn(
    db_path: PathBuf,
    key_path: PathBuf,
    playbook_id: String,
    template: CompiledTemplate,
    control: RunControl,
    make_surfaces: SurfaceFactory,
) -> ActiveRun {
    let outcome: Arc<Mutex<Option<RunOutcome>>> = Arc::new(Mutex::new(None));

    let handle = {
        let outcome = Arc::clone(&outcome);
        let control = control.clone();
        let playbook_id = playbook_id.clone();
        std::thread::spawn(move || {
            let result = execute(
                &db_path,
                &key_path,
                &playbook_id,
                &template,
                &control,
                make_surfaces,
            );
            *outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        })
    };

    ActiveRun {
        playbook_id,
        control,
        outcome,
        handle: Some(handle),
    }
}

/// The body of the thread, factored out so every exit runs through one place.
fn execute(
    db_path: &std::path::Path,
    key_path: &std::path::Path,
    playbook_id: &str,
    template: &CompiledTemplate,
    control: &RunControl,
    make_surfaces: SurfaceFactory,
) -> RunOutcome {
    let conn = match crate::db::open(db_path, key_path) {
        Ok(c) => c,
        Err(e) => return RunOutcome::Failed(format!("could not open the database: {e}")),
    };

    let (mut reader, mut writer) = match make_surfaces() {
        Ok(s) => s,
        Err(e) => {
            // The state is put back before returning: a workflow left saying
            // "running" after a failure to start would be unstartable, with
            // nothing running to explain why.
            let _ = set_run_state(&conn, playbook_id, RunState::Idle);
            return RunOutcome::Failed(e);
        }
    };

    let _ = set_run_state(&conn, playbook_id, RunState::Running);

    let result = run_with_control(
        &conn,
        playbook_id,
        template,
        reader.as_mut(),
        writer.as_mut(),
        control,
    );

    // Idle on every path, including a stop and including an error. A run that
    // ended is not running, whatever the reason.
    let _ = set_run_state(&conn, playbook_id, RunState::Idle);

    match result {
        Ok(report) => RunOutcome::Finished(report),
        Err(e) => RunOutcome::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::FieldMapping;
    use crate::run::{get_run_state, processed_count, RunStop};
    use crate::source::{Advance, FieldRef, SourceError, SourcePosition, SourceRecord, SourceShape};
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Minimal fakes, separate from the ones in `run::tests`: what is under
    /// test here is the thread, not the loop, so these only need to supply a
    /// few records and record what was written.
    struct Rows {
        cursor: usize,
        count: usize,
    }

    impl SourceReader for Rows {
        fn position(&self) -> SourcePosition {
            SourcePosition {
                source_id: "src".into(),
                row_key: (self.cursor + 1).to_string(),
            }
        }
        fn peek(&mut self, _: &[FieldRef]) -> Result<Advance, SourceError> {
            Ok(if self.cursor < self.count {
                Advance::Record
            } else {
                Advance::Exhausted
            })
        }
        fn read(&mut self, _: &[FieldRef]) -> Result<SourceRecord, SourceError> {
            let mut fields = BTreeMap::new();
            fields.insert("C".to_string(), format!("row{}", self.cursor + 1));
            Ok(SourceRecord {
                position: self.position(),
                fields,
            })
        }
        fn advance(&mut self) -> Result<(), SourceError> {
            self.cursor += 1;
            Ok(())
        }
        fn shape(&mut self) -> Result<SourceShape, SourceError> {
            Ok(SourceShape { columns: vec![] })
        }
    }

    struct Sink {
        written: Arc<Mutex<Vec<String>>>,
    }

    impl DestinationWriter for Sink {
        fn position(&self) -> String {
            "1".into()
        }
        fn write(&mut self, _: &str, value: &str) -> Result<(), SourceError> {
            self.written
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(value.to_string());
            Ok(())
        }
        fn advance(&mut self, _: i64) -> Result<(), SourceError> {
            Ok(())
        }
    }

    fn template() -> CompiledTemplate {
        CompiledTemplate {
            source_id: "src".into(),
            destination_id: "dst".into(),
            source_step: 1,
            destination_step: 1,
            examples: 3,
            fields: vec![FieldMapping {
                source_field: "C".into(),
                destination_field: "A".into(),
            }],
        }
    }

    /// A real encrypted database with one real playbook, and the paths needed
    /// to open a second connection to it from the run thread.
    fn fixture() -> (TempDir, PathBuf, PathBuf, String) {
        let dir = TempDir::new().expect("temp dir");
        let (db_path, key_path) = crate::db::paths_in(dir.path());
        let mut conn = crate::db::open(&db_path, &key_path).expect("open");

        let mut stream = crate::capture::CapturedStream::new(
            crate::capture::ExclusionList::from_patterns(["!never!"]),
        );
        stream.admit(crate::capture::ActionCandidate {
            kind: crate::capture::ActionKind::Click,
            identifiers: vec!["app.exe".into()],
            process_name: None,
            element_role: Some("Button".into()),
            element_name: Some("Next".into()),
            payload: None,
            detail: None,
            timestamp_ms: 0,
        });
        let pb = crate::compile::compile(
            stream.actions(),
            "Background run",
            &crate::compile::ReversibilityPolicy::placeholder(),
            &crate::labeling::RedactionPolicy::placeholder(),
        );
        crate::compile::store::store(&mut conn, &pb).expect("store");
        (dir, db_path, key_path, pb.id)
    }

    fn surfaces(count: usize, written: Arc<Mutex<Vec<String>>>) -> SurfaceFactory {
        Box::new(move || {
            Ok((
                Box::new(Rows { cursor: 0, count }) as Box<dyn SourceReader>,
                Box::new(Sink { written }) as Box<dyn DestinationWriter>,
            ))
        })
    }

    #[test]
    fn a_run_completes_on_its_own_thread_and_reports_back() {
        let (_dir, db_path, key_path, id) = fixture();
        let written = Arc::new(Mutex::new(Vec::new()));

        let run = spawn(
            db_path.clone(),
            key_path.clone(),
            id.clone(),
            template(),
            RunControl::new(),
            surfaces(3, Arc::clone(&written)),
        );

        match run.join().expect("an outcome") {
            RunOutcome::Finished(report) => {
                assert_eq!(report.stop, RunStop::Exhausted);
                assert_eq!(report.written(), 3);
            }
            RunOutcome::Failed(e) => panic!("run failed: {e}"),
        }

        assert_eq!(
            *written.lock().unwrap(),
            vec!["row1", "row2", "row3"]
        );

        // The run used its own connection; this one checks the same file.
        let conn = crate::db::open(&db_path, &key_path).expect("reopen");
        assert_eq!(processed_count(&conn, &id, "src").expect("count"), 3);
        assert_eq!(
            get_run_state(&conn, &id).expect("state"),
            Some(RunState::Idle),
            "a finished run is not running"
        );
    }

    #[test]
    fn a_paused_run_holds_its_thread_without_blocking_the_caller() {
        // §4.10's "it does not lock the window", tested as the property it
        // actually is: the run is held, the caller is not.
        let (_dir, db_path, key_path, id) = fixture();
        let written = Arc::new(Mutex::new(Vec::new()));
        let control = RunControl::new();
        control.pause();

        let run = spawn(
            db_path.clone(),
            key_path.clone(),
            id.clone(),
            template(),
            control.clone(),
            surfaces(3, Arc::clone(&written)),
        );

        // The caller got here immediately, and stays free while the run waits.
        std::thread::sleep(Duration::from_millis(150));
        assert!(!run.is_finished(), "a paused run must not run to completion");
        assert!(
            written.lock().unwrap().is_empty(),
            "a run paused before it began must not have written anything"
        );

        control.resume();
        match run.join().expect("an outcome") {
            RunOutcome::Finished(report) => assert_eq!(report.written(), 3),
            RunOutcome::Failed(e) => panic!("run failed: {e}"),
        }
        assert_eq!(*written.lock().unwrap(), vec!["row1", "row2", "row3"]);
    }

    #[test]
    fn a_stop_ends_a_paused_run_rather_than_deadlocking_on_it() {
        let (_dir, db_path, key_path, id) = fixture();
        let written = Arc::new(Mutex::new(Vec::new()));
        let control = RunControl::new();
        control.pause();

        let run = spawn(
            db_path.clone(),
            key_path.clone(),
            id.clone(),
            template(),
            control,
            surfaces(3, Arc::clone(&written)),
        );

        // `stop_and_join`, not `join`: a plain join would wait forever on a run
        // that is still paused, which is exactly the distinction these two
        // methods exist to make.
        match run.stop_and_join().expect("an outcome") {
            RunOutcome::Finished(report) => {
                assert!(matches!(report.stop, RunStop::Stopped { .. }));
                assert_eq!(report.written(), 0);
            }
            RunOutcome::Failed(e) => panic!("run failed: {e}"),
        }

        let conn = crate::db::open(&db_path, &key_path).expect("reopen");
        assert_eq!(
            get_run_state(&conn, &id).expect("state"),
            Some(RunState::Idle),
            "a stopped run is not left looking like it is still going"
        );
    }

    #[test]
    fn a_source_that_will_not_open_fails_the_run_and_leaves_it_idle() {
        // The failure has to land somewhere a user can see, and it must not
        // strand the workflow in `running` with nothing running.
        let (_dir, db_path, key_path, id) = fixture();

        let run = spawn(
            db_path.clone(),
            key_path.clone(),
            id.clone(),
            template(),
            RunControl::new(),
            Box::new(|| Err("the source document is not open".to_string())),
        );

        match run.join().expect("an outcome") {
            RunOutcome::Failed(e) => assert!(e.contains("not open"), "unhelpful: {e}"),
            RunOutcome::Finished(r) => panic!("expected a failure, got {:?}", r.stop),
        }

        let conn = crate::db::open(&db_path, &key_path).expect("reopen");
        assert_eq!(
            get_run_state(&conn, &id).expect("state"),
            Some(RunState::Idle),
            "a run that never started must not look like it is running"
        );
    }
}
