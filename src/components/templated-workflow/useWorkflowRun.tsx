import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { describeError } from "@/lib/errors";
import type { NewBatchView, PreviewOutcome, PreviewView, RunStatusView } from "./types";

/**
 * The one path from "is there anything to do?" to a running workflow.
 *
 * ## Why this is a single state machine and not three components' local state
 *
 * §4.8's batch prompt, §4.3's preview and §4.6's controls are three screens of
 * one flow, and the backend enforces the order: `start_workflow_run` takes no
 * playbook id at all, because the run is defined by the preview that was
 * confirmed. Splitting the state would let the UI offer a "Run" button in a
 * place the backend will refuse, which is a worse experience than not offering
 * it — the user would learn the button sometimes lies.
 *
 * So the only way out of this hook into a run is `confirmPreview`, and the only
 * way to reach that is through a preview that actually returned a record.
 */
export type WorkflowRunStage =
  | { name: "idle" }
  | { name: "checking" }
  /** §4.8 found work, or found none and is saying so plainly. */
  | { name: "batch"; batch: NewBatchView }
  | { name: "previewing" }
  /** §4.3's confirm/cancel. */
  | { name: "preview"; preview: PreviewView }
  /** Nothing to preview: exhausted, or drift needs attention first. */
  | { name: "blocked"; reason: string; needsAttention: boolean }
  | { name: "running"; status: RunStatusView }
  | { name: "error"; message: string };

/** How often the overlay asks the backend where the run has got to. */
const POLL_MS = 1000;

export function useWorkflowRun() {
  const [stage, setStage] = useState<WorkflowRunStage>({ name: "idle" });
  const [playbookId, setPlaybookId] = useState<string | null>(null);
  const pollRef = useRef<number | null>(null);

  const stopPolling = useCallback(() => {
    if (pollRef.current !== null) {
      window.clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }, []);

  // A poll that outlives its screen would keep invoking after the user has
  // moved on, and would set state on an unmounted tree.
  useEffect(() => stopPolling, [stopPolling]);

  const reset = useCallback(() => {
    stopPolling();
    setStage({ name: "idle" });
    setPlaybookId(null);
  }, [stopPolling]);

  /** §4.8. Ask whether the source has anything unprocessed. */
  const checkForNewRecords = useCallback(async (id: string) => {
    setPlaybookId(id);
    setStage({ name: "checking" });
    try {
      const batch = await invoke<NewBatchView>("check_for_new_records", {
        playbookId: id,
      });
      setStage({ name: "batch", batch });
    } catch (e) {
      setStage({ name: "error", message: describeError(e) });
    }
  }, []);

  /**
   * §4.3. Read the next record without writing it.
   *
   * This is also the only thing that can produce the backend's
   * `RunAuthorization`, so it is deliberately the only route to `startRun`.
   */
  const openPreview = useCallback(
    async (id: string) => {
      setPlaybookId(id);
      setStage({ name: "previewing" });
      try {
        const outcome = await invoke<PreviewOutcome>("preview_workflow_run", {
          playbookId: id,
        });
        switch (outcome.kind) {
          case "ready":
            setStage({ name: "preview", preview: outcome });
            break;
          case "nothingToDo":
            setStage({
              name: "blocked",
              reason: outcome.reason,
              needsAttention: false,
            });
            break;
          case "needsAttention":
            setStage({
              name: "blocked",
              reason: outcome.reason,
              needsAttention: true,
            });
            break;
        }
      } catch (e) {
        setStage({ name: "error", message: describeError(e) });
      }
    },
    [],
  );

  const pollStatus = useCallback(() => {
    stopPolling();
    pollRef.current = window.setInterval(() => {
      void (async () => {
        try {
          const status = await invoke<RunStatusView>("get_workflow_run_status");
          setStage({ name: "running", status });
          if (status.finished) stopPolling();
        } catch (e) {
          stopPolling();
          setStage({ name: "error", message: describeError(e) });
        }
      })();
    }, POLL_MS);
  }, [stopPolling]);

  /** §4.3's "confirm". The only way into a run. */
  const confirmPreview = useCallback(async () => {
    try {
      const status = await invoke<RunStatusView>("start_workflow_run");
      setStage({ name: "running", status });
      pollStatus();
    } catch (e) {
      setStage({ name: "error", message: describeError(e) });
    }
  }, [pollStatus]);

  /** §4.10: "cancels cleanly. Nothing activates." */
  const cancelPreview = useCallback(async () => {
    try {
      await invoke("cancel_workflow_preview");
    } catch {
      // A cancel that fails still means the user said no, and the backend
      // refuses to start without a held preview either way. Surfacing an error
      // here would imply something was left running.
    }
    reset();
  }, [reset]);

  const pause = useCallback(async () => {
    try {
      const status = await invoke<RunStatusView>("pause_workflow_run");
      setStage({ name: "running", status });
    } catch (e) {
      setStage({ name: "error", message: describeError(e) });
    }
  }, []);

  const resume = useCallback(async () => {
    try {
      const status = await invoke<RunStatusView>("resume_workflow_run");
      setStage({ name: "running", status });
      pollStatus();
    } catch (e) {
      setStage({ name: "error", message: describeError(e) });
    }
  }, [pollStatus]);

  const stop = useCallback(async () => {
    try {
      const status = await invoke<RunStatusView>("stop_workflow_run");
      stopPolling();
      setStage({ name: "running", status: { ...status, finished: true } });
    } catch (e) {
      setStage({ name: "error", message: describeError(e) });
    }
  }, [stopPolling]);

  return {
    stage,
    playbookId,
    checkForNewRecords,
    openPreview,
    confirmPreview,
    cancelPreview,
    pause,
    resume,
    stop,
    reset,
  };
}
