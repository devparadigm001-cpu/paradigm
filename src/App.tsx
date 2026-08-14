import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useDeletePlaybookConfirmation } from "@/components/delete-playbook";
import { Button } from "@/components/ui/button";
import {
  MOCK_PLAYBOOK_IRREVERSIBLE,
  MOCK_PLAYBOOK_SAFE,
  useAutomationPreview,
  type PlaybookPreviewSummary,
} from "@/components/automation-preview";
import { useAccessibilityPermissionGate } from "@/components/permission-gate";
import {
  RecordingBadgeView,
  RecordingReviewScreen,
  useRecordMode,
  type PlaybookSummaryView,
  type ReplayReportView,
} from "@/components/record-mode";
import { ReplayProgressScreen, ReplayResultScreen } from "@/components/replay";
import { useWorkflowRun, WorkflowRunFlow } from "@/components/templated-workflow";
import {
  isProofWindow,
  isRecordingBadgeWindow,
  isTauriRuntime,
  openProofWindow,
} from "@/lib/windows";
import { describeError } from "@/lib/errors";
import { useWindowLabel } from "@/hooks/useWindowLabel";

function ProofWindowView() {
  return (
    <main className="flex min-h-svh flex-col items-center justify-center gap-2 p-8">
      <h1 className="text-lg font-semibold tracking-tight">Proof Window</h1>
      <p className="text-muted-foreground text-sm">
        Native secondary window — multi-window capability confirmed.
      </p>
    </main>
  );
}

const RECORD_PHASE_LABEL: Record<string, string> = {
  idle: "Start recording",
  starting: "Starting...",
  recording: "Stop recording",
  stopping: "Stopping...",
};

function playbookToPreviewSummary(
  playbook: PlaybookSummaryView,
): PlaybookPreviewSummary {
  return {
    playbook_id: playbook.id,
    name: playbook.name,
    step_count: playbook.step_count,
    irreversible_count: playbook.irreversible_count,
  };
}

type StoredPlaybooksSectionProps = {
  onReplay: (playbook: PlaybookSummaryView) => void;
  replayBusyPlaybookId: string | null;
  /** §4.8: ask whether this workflow's source has anything unprocessed. */
  onCheckForNewRecords: (playbook: PlaybookSummaryView) => void;
  workflowBusy: boolean;
};

function StoredPlaybooksSection({
  onReplay,
  replayBusyPlaybookId,
  onCheckForNewRecords,
  workflowBusy,
}: StoredPlaybooksSectionProps) {
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [deletingPlaybookId, setDeletingPlaybookId] = useState<string | null>(
    null,
  );
  const { data, isLoading, isError, error, refetch, isFetching } = useQuery({
    queryKey: ["playbooks"],
    queryFn: () => invoke<PlaybookSummaryView[]>("list_playbooks"),
  });
  const { requestDeleteConfirmation, deleteDialogElement } =
    useDeletePlaybookConfirmation({
      onConfirmError: (deleteErr, playbook) => {
        setDeleteError(
          `Delete "${playbook.name}" failed: ${describeError(deleteErr)}`,
        );
        setDeletingPlaybookId(null);
      },
    });

  const actionsBusy =
    replayBusyPlaybookId !== null || deletingPlaybookId !== null;

  function handleDelete(playbook: PlaybookSummaryView) {
    setDeleteError(null);
    requestDeleteConfirmation(playbook, async () => {
      setDeletingPlaybookId(playbook.id);
      await invoke("delete_playbook", { playbookId: playbook.id });
      await refetch();
      setDeletingPlaybookId(null);
    });
  }

  return (
    <div className="mt-4 flex w-full max-w-md flex-col gap-2 rounded-md border p-4">
      <div className="flex items-center justify-between">
        <p className="text-sm font-medium">Stored playbooks</p>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => void refetch()}
          disabled={isFetching}
        >
          {isFetching ? "Refreshing..." : "Refresh"}
        </Button>
      </div>
      {deleteError ? (
        <p className="text-destructive text-sm">{deleteError}</p>
      ) : null}
      {isLoading ? (
        <p className="text-muted-foreground text-sm">Loading...</p>
      ) : isError ? (
        <p className="text-destructive text-sm">{describeError(error)}</p>
      ) : !data || data.length === 0 ? (
        <p className="text-muted-foreground text-sm">
          No playbooks saved yet — record and save one below.
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {data.map((playbook, index) => (
            <li
              key={playbook.id}
              className="flex items-center justify-between gap-2 text-sm"
            >
              <div className="min-w-0 flex-1">
                <p className="flex items-center gap-1.5 truncate">
                  {playbook.name}
                  {/* Section 6: templated workflows live in this same list,
                      marked rather than moved to a screen of their own. */}
                  {playbook.is_templated ? (
                    <span
                      className="rounded bg-secondary px-1.5 py-0.5 text-[10px] font-medium"
                      title="Repeating workflow — can run over new source records"
                    >
                      ↻ repeating
                    </span>
                  ) : null}
                </p>
                <p className="text-muted-foreground text-xs">
                  {playbook.step_count} step{playbook.step_count === 1 ? "" : "s"}{" "}
                  · {playbook.source}
                  {playbook.irreversible_count > 0
                    ? ` · ${playbook.irreversible_count} irreversible`
                    : ""}
                </p>
              </div>
              <div className="flex shrink-0 gap-2">
                {playbook.is_templated ? (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={actionsBusy || workflowBusy}
                    // Same reasoning as Delete below: every row's button would
                    // otherwise share one accessible name.
                    aria-label={`Check for new records for ${playbook.name}, ${index + 1} of ${data.length}`}
                    onClick={() => onCheckForNewRecords(playbook)}
                  >
                    Check for new
                  </Button>
                ) : null}
                <Button
                  variant="outline"
                  size="sm"
                  disabled={actionsBusy}
                  onClick={() => onReplay(playbook)}
                >
                  Replay
                </Button>
                <Button
                  variant="destructive"
                  size="sm"
                  // Every row's Delete otherwise has the accessible name
                  // "Delete", so nothing can tell them apart -- not a screen
                  // reader, and not this product's own automation, which drives
                  // the UI through the accessibility tree and refuses to act
                  // when a selector matches more than one element
                  // (StepResult::FailedAmbiguous).
                  //
                  // The position is what makes it unique. The NAME alone does
                  // not: playbook names are not unique -- nothing enforces it at
                  // any layer -- so two recordings called the same thing would
                  // still produce two identical labels. Measured with two
                  // same-named playbooks before settling on this.
                  aria-label={`Delete ${playbook.name}, ${index + 1} of ${data.length}`}
                  disabled={actionsBusy}
                  onClick={() => handleDelete(playbook)}
                >
                  {deletingPlaybookId === playbook.id ? "Deleting..." : "Delete"}
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
      {deleteDialogElement}
    </div>
  );
}

function MainWindowView() {
  const { isSuccess, isError } = useQuery({
    queryKey: ["foundation-health"],
    queryFn: async () => true,
  });
  const [windowError, setWindowError] = useState<string | null>(null);
  const [isOpeningWindow, setIsOpeningWindow] = useState(false);
  const [demoLog, setDemoLog] = useState<string | null>(null);
  const [replayBusyPlaybookId, setReplayBusyPlaybookId] = useState<string | null>(
    null,
  );
  const [replayProgressName, setReplayProgressName] = useState<string | null>(
    null,
  );
  const [replayReport, setReplayReport] = useState<ReplayReportView | null>(null);
  const [replayError, setReplayError] = useState<string | null>(null);

  const { requestConfirmation, previewElement } = useAutomationPreview({
    onDeny: (data) => {
      setDemoLog(`Denied: "${data.playbook.name}" was not run.`);
    },
    onConfirmError: (error, data) => {
      setReplayError(
        `Replay for "${data.playbook.name}" failed: ${describeError(error)}`,
      );
      setReplayBusyPlaybookId(null);
      setReplayProgressName(null);
    },
  });

  function clearReplayState() {
    setReplayBusyPlaybookId(null);
    setReplayProgressName(null);
    setReplayReport(null);
    setReplayError(null);
  }

  function runStoredPlaybookReplay(playbook: PlaybookSummaryView) {
    setReplayError(null);
    setReplayReport(null);
    requestConfirmation(
      { variant: "first-run", playbook: playbookToPreviewSummary(playbook) },
      async () => {
        setReplayBusyPlaybookId(playbook.id);
        setReplayProgressName(playbook.name);
        const report = await invoke<ReplayReportView>("replay_playbook", {
          playbookId: playbook.id,
        });
        setReplayReport(report);
        setReplayBusyPlaybookId(null);
        setReplayProgressName(null);
      },
    );
  }

  function runFirstRunDemo(playbook: PlaybookPreviewSummary) {
    setDemoLog(null);
    requestConfirmation({ variant: "first-run", playbook }, () => {
      setDemoLog(
        `Confirmed: "${playbook.name}" would now call replay_playbook() ` +
          "(demo only — no real invoke() call is made here).",
      );
    });
  }

  const [permissionDemoLog, setPermissionDemoLog] = useState<string | null>(
    null,
  );
  const {
    requestAction: requestPermissionGatedAction,
    gateElement: permissionGateElement,
    isGranted: hasAccessibilityPermission,
  } = useAccessibilityPermissionGate();

  function runStartRecordingDemo() {
    requestPermissionGatedAction(() => {
      setPermissionDemoLog(
        "Action ran: would now call start_record_session() " +
          "(demo only — no real invoke() call is made here).",
      );
    });
  }

  async function handleOpenProofWindow() {
    if (!isTauriRuntime()) {
      setWindowError("Multi-window proof requires the Tauri desktop runtime.");
      return;
    }

    setWindowError(null);
    setIsOpeningWindow(true);

    try {
      await openProofWindow();
    } catch (error) {
      setWindowError(
        error instanceof Error ? error.message : "Failed to open proof window.",
      );
    } finally {
      setIsOpeningWindow(false);
    }
  }

  const workflowRun = useWorkflowRun();
  const recordMode = useRecordMode();

  if (recordMode.captureSummary) {
    return (
      <RecordingReviewScreen
        summary={recordMode.captureSummary}
        onDone={recordMode.dismissCaptureSummary}
      />
    );
  }

  if (replayProgressName) {
    return <ReplayProgressScreen playbookName={replayProgressName} />;
  }

  if (replayReport) {
    return (
      <ReplayResultScreen report={replayReport} onDone={clearReplayState} />
    );
  }

  if (replayError) {
    return (
      <main className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
        <h1 className="text-xl font-semibold tracking-tight">Replay failed</h1>
        <p className="text-destructive max-w-md text-center text-sm">{replayError}</p>
        <Button onClick={clearReplayState}>Done</Button>
      </main>
    );
  }

  const recordButtonDisabled =
    recordMode.phase === "starting" || recordMode.phase === "stopping";

  return (
    <main className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
      <h1 className="text-2xl font-semibold tracking-tight">Paradigm</h1>
      <p className="text-muted-foreground max-w-md text-center text-sm">
        Foundation stack: shadcn/ui, Tailwind CSS, TanStack Query, and
        multi-window Tauri.
      </p>
      <div className="flex flex-col items-center gap-2">
        <Button disabled={!isSuccess} variant={isError ? "destructive" : "default"}>
          {isSuccess
            ? "Query client ready"
            : isError
              ? "Connection failed"
              : "Initializing..."}
        </Button>
        <Button
          variant="outline"
          disabled={isOpeningWindow}
          onClick={() => void handleOpenProofWindow()}
        >
          {isOpeningWindow ? "Opening..." : "Open proof window"}
        </Button>
      </div>
      {windowError ? (
        <p className="text-destructive max-w-md text-center text-sm">
          {windowError}
        </p>
      ) : null}

      <div className="mt-4 flex w-full max-w-md flex-col items-center gap-2 rounded-md border p-4">
        <p className="text-sm font-medium">Record Mode</p>
        <Button
          variant={recordMode.phase === "recording" ? "destructive" : "default"}
          disabled={recordButtonDisabled}
          onClick={recordMode.toggle}
        >
          {RECORD_PHASE_LABEL[recordMode.phase]}
        </Button>
        <p className="text-muted-foreground text-center text-xs">
          Global shortcut: Ctrl+Shift+R toggles start/stop from anywhere.
          Accessibility permission:{" "}
          <span className="font-medium">
            {recordMode.hasAccessibilityPermission ? "granted" : "not granted"}
          </span>
        </p>
        {recordMode.error ? (
          <p className="text-destructive max-w-full text-center text-xs break-words">
            {recordMode.error}
          </p>
        ) : null}
      </div>

      <StoredPlaybooksSection
        onReplay={runStoredPlaybookReplay}
        replayBusyPlaybookId={replayBusyPlaybookId}
        onCheckForNewRecords={(playbook) =>
          void workflowRun.checkForNewRecords(playbook.id)
        }
        workflowBusy={workflowRun.stage.name !== "idle"}
      />

      {/* Section 6 items 2-4. Rendered here rather than inside the list so a
          run keeps its controls on screen while the user works elsewhere in
          the window -- §4.10's "does not lock the window". */}
      <WorkflowRunFlow
        stage={workflowRun.stage}
        onConfirmBatch={() => {
          if (workflowRun.playbookId) {
            void workflowRun.openPreview(workflowRun.playbookId);
          }
        }}
        onConfirmPreview={() => void workflowRun.confirmPreview()}
        onCancelPreview={() => void workflowRun.cancelPreview()}
        onPause={() => void workflowRun.pause()}
        onResume={() => void workflowRun.resume()}
        onStop={() => void workflowRun.stop()}
        onDismiss={workflowRun.reset}
      />

      <div className="mt-4 flex w-full max-w-md flex-col items-center gap-2 rounded-md border border-dashed p-4">
        <p className="text-muted-foreground text-center text-xs font-medium tracking-wide uppercase">
          Dev-only demo — Automation Preview (Step 9), not real product UI
        </p>
        <div className="flex flex-wrap justify-center gap-2">
          <Button
            variant="secondary"
            size="sm"
            onClick={() => runFirstRunDemo(MOCK_PLAYBOOK_SAFE)}
          >
            Demo: safe playbook
          </Button>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => runFirstRunDemo(MOCK_PLAYBOOK_IRREVERSIBLE)}
          >
            Demo: irreversible playbook
          </Button>
        </div>
        {demoLog ? (
          <p className="text-muted-foreground text-center text-xs">
            {demoLog}
          </p>
        ) : null}
      </div>

      <div className="mt-4 flex w-full max-w-md flex-col items-center gap-2 rounded-md border border-dashed p-4">
        <p className="text-muted-foreground text-center text-xs font-medium tracking-wide uppercase">
          Dev-only demo — Accessibility Permission Gate (Step 10), not real
          product UI
        </p>
        <p className="text-muted-foreground text-xs">
          Mock permission state:{" "}
          <span className="font-medium">
            {hasAccessibilityPermission ? "granted" : "not granted"}
          </span>
        </p>
        <Button variant="secondary" size="sm" onClick={runStartRecordingDemo}>
          Demo: start recording
        </Button>
        {permissionDemoLog ? (
          <p className="text-muted-foreground text-center text-xs">
            {permissionDemoLog}
          </p>
        ) : null}
      </div>

      {previewElement}
      {permissionGateElement}
      {recordMode.permissionGateElement}
    </main>
  );
}

function App() {
  const windowLabel = useWindowLabel();

  if (isRecordingBadgeWindow(windowLabel)) {
    return <RecordingBadgeView />;
  }

  if (isProofWindow(windowLabel)) {
    return <ProofWindowView />;
  }

  return <MainWindowView />;
}

export default App;
