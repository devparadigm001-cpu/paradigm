import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
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
} from "@/components/record-mode";
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

function StoredPlaybooksSection() {
  const { data, isLoading, isError, error, refetch, isFetching } = useQuery({
    queryKey: ["playbooks"],
    queryFn: () => invoke<PlaybookSummaryView[]>("list_playbooks"),
  });

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
      {isLoading ? (
        <p className="text-muted-foreground text-sm">Loading...</p>
      ) : isError ? (
        <p className="text-destructive text-sm">{describeError(error)}</p>
      ) : !data || data.length === 0 ? (
        <p className="text-muted-foreground text-sm">
          No playbooks saved yet — record and save one below.
        </p>
      ) : (
        <ul className="flex flex-col gap-1">
          {data.map((playbook) => (
            <li
              key={playbook.id}
              className="flex items-center justify-between gap-2 text-sm"
            >
              <span className="truncate">{playbook.name}</span>
              <span className="text-muted-foreground shrink-0 text-xs">
                {playbook.step_count} step{playbook.step_count === 1 ? "" : "s"}{" "}
                · {playbook.source}
              </span>
            </li>
          ))}
        </ul>
      )}
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

  const { requestConfirmation, previewElement } = useAutomationPreview({
    onDeny: (data) => {
      setDemoLog(`Denied: "${data.playbook.name}" was not run.`);
    },
    onConfirmError: (error, data) => {
      setDemoLog(
        `Confirm handler for "${data.playbook.name}" threw: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    },
  });

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

  const recordMode = useRecordMode();

  if (recordMode.captureSummary) {
    return (
      <RecordingReviewScreen
        summary={recordMode.captureSummary}
        onDone={recordMode.dismissCaptureSummary}
      />
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

      <StoredPlaybooksSection />

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
