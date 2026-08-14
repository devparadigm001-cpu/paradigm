import { Button } from "@/components/ui/button";
import type {
  NewBatchView,
  PreviewView,
  RunStatusView,
  RunSummaryView,
} from "./types";
import type { WorkflowRunStage } from "./useWorkflowRun";

/**
 * Section 6 items 2, 3 and 4, rendered from one stage value.
 *
 * They are separate items in the build order and one flow in use, so they live
 * in one file: the alternative is three components that each have to know which
 * of the others may be on screen.
 */
export function WorkflowRunFlow({
  stage,
  onConfirmBatch,
  onConfirmPreview,
  onCancelPreview,
  onPause,
  onResume,
  onStop,
  onDismiss,
}: {
  stage: WorkflowRunStage;
  onConfirmBatch: () => void;
  onConfirmPreview: () => void;
  onCancelPreview: () => void;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
  onDismiss: () => void;
}) {
  switch (stage.name) {
    case "idle":
      return null;
    case "checking":
      return <Card>Looking for new records…</Card>;
    case "previewing":
      return <Card>Reading the next record…</Card>;
    case "batch":
      return (
        <BatchPrompt
          batch={stage.batch}
          onConfirm={onConfirmBatch}
          onDismiss={onDismiss}
        />
      );
    case "preview":
      return (
        <FirstRecordPreview
          preview={stage.preview}
          onConfirm={onConfirmPreview}
          onCancel={onCancelPreview}
        />
      );
    case "blocked":
      return (
        <Card>
          <p
            className={
              stage.needsAttention ? "text-destructive text-sm" : "text-sm"
            }
          >
            {stage.reason}
          </p>
          <div className="mt-3 flex justify-end">
            <Button variant="outline" size="sm" onClick={onDismiss}>
              Close
            </Button>
          </div>
        </Card>
      );
    case "running":
      return (
        <RunControlOverlay
          status={stage.status}
          onPause={onPause}
          onResume={onResume}
          onStop={onStop}
          onDismiss={onDismiss}
        />
      );
    case "summary":
      return <RunSummary summary={stage.summary} onDismiss={onDismiss} />;
    case "error":
      return (
        <Card>
          <p className="text-destructive text-sm">{stage.message}</p>
          <div className="mt-3 flex justify-end">
            <Button variant="outline" size="sm" onClick={onDismiss}>
              Close
            </Button>
          </div>
        </Card>
      );
  }
}

/**
 * §4.9: "quiet by default, detailed only when it matters."
 *
 * A clean run renders one line and a Close button — no counts table, no
 * per-record list, and specifically no "0 flagged for review", which is the
 * noise the rule exists to prevent. Everything below the headline appears only
 * when the backend says `needs_attention`.
 */
function RunSummary({
  summary,
  onDismiss,
}: {
  summary: RunSummaryView;
  onDismiss: () => void;
}) {
  const clean = !summary.needs_attention;
  return (
    <Card wide={!clean}>
      <div className="flex items-center gap-2">
        <span
          aria-hidden
          className={
            clean
              ? "size-2 rounded-full bg-emerald-500"
              : "size-2 rounded-full bg-amber-500"
          }
        />
        <p className="text-sm font-medium">
          {clean ? "Run finished" : "Run finished — needs a look"}
        </p>
      </div>

      {/* §4.9's plain line. Always shown; on a clean run it is the whole
          report. */}
      <p className="mt-1 text-sm">{summary.headline}</p>

      {summary.needs_attention ? (
        <div className="mt-3 flex flex-col gap-2 border-t pt-3">
          {summary.stop_reason ? (
            <p className="text-sm">{summary.stop_reason}</p>
          ) : null}

          {/* Shown only when greater than zero — §4.9 is explicit. */}
          {summary.flagged > 0 ? (
            <>
              <p className="text-sm font-medium">
                {summary.flagged} flagged for review
              </p>
              <ul className="flex flex-col gap-1">
                {summary.flagged_records.map((record) => (
                  <li key={record.source_row} className="text-xs">
                    <span className="font-mono">row {record.source_row}</span>{" "}
                    → destination row{" "}
                    <span className="font-mono">{record.destination_row}</span>
                    {record.missing_fields.length > 0 ? (
                      <span className="text-muted-foreground">
                        {" "}
                        — nothing in{" "}
                        {record.missing_fields.join(", ")}, written blank
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            </>
          ) : null}

          {summary.skipped > 0 ? (
            <p className="text-muted-foreground text-xs">
              {summary.skipped} already processed, so left alone.
            </p>
          ) : null}
        </div>
      ) : null}

      <div className="mt-3 flex justify-end">
        <Button variant="outline" size="sm" onClick={onDismiss}>
          Close
        </Button>
      </div>
    </Card>
  );
}

/**
 * §4.8's confirmation.
 *
 * Answering yes does NOT start a run — it opens the first-record preview,
 * which is the only thing that can authorize one. The button says "Preview"
 * rather than "Run" for that reason: a button labelled Run that leads to
 * another confirmation trains the user to click through both.
 */
function BatchPrompt({
  batch,
  onConfirm,
  onDismiss,
}: {
  batch: NewBatchView;
  onConfirm: () => void;
  onDismiss: () => void;
}) {
  return (
    <Card>
      <p className="text-sm">{batch.message}</p>
      {batch.capped && batch.has_work ? (
        <p className="text-muted-foreground mt-1 text-xs">
          The scan stopped early, so there may be more than {batch.count}.
        </p>
      ) : null}
      <div className="mt-3 flex justify-end gap-2">
        <Button variant="outline" size="sm" onClick={onDismiss}>
          {batch.has_work ? "Not now" : "Close"}
        </Button>
        {batch.has_work ? (
          <Button size="sm" onClick={onConfirm}>
            Preview first record
          </Button>
        ) : null}
      </div>
    </Card>
  );
}

/**
 * §4.3: "shows the very next record it's about to write — real values, in the
 * real destination — with a simple confirm/cancel."
 *
 * The model's verdict is shown but never gates the buttons. Its measured
 * confidence band does not separate sensible mappings from nonsense (see
 * `run::preview`), so disabling Confirm on it would block real work on a
 * signal known not to discriminate. The human confirming IS the gate.
 */
function FirstRecordPreview({
  preview,
  onConfirm,
  onCancel,
}: {
  preview: PreviewView;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Card wide>
      <h2 className="text-sm font-semibold">
        About to write source row {preview.source_row} → destination row{" "}
        {preview.destination_row}
      </h2>
      <p className="text-muted-foreground mt-1 text-xs">
        Checking one record now catches a wrong mapping before the rest are
        written.
      </p>

      <table className="mt-3 w-full text-sm">
        <thead>
          <tr className="text-muted-foreground text-left text-xs">
            <th scope="col" className="font-medium">
              From
            </th>
            <th scope="col" className="font-medium">
              To
            </th>
            <th scope="col" className="font-medium">
              Value
            </th>
          </tr>
        </thead>
        <tbody>
          {preview.fields.map((field) => (
            <tr key={`${field.source_field}-${field.destination_field}`}>
              <td className="py-0.5">
                <span className="font-mono">{field.source_field}</span>
                {field.source_label ? (
                  <span className="text-muted-foreground"> {field.source_label}</span>
                ) : null}
              </td>
              <td className="py-0.5">
                <span className="font-mono">
                  {field.destination_field}
                  {preview.destination_row}
                </span>
                {field.destination_label ? (
                  <span className="text-muted-foreground">
                    {" "}
                    {field.destination_label}
                  </span>
                ) : null}
              </td>
              <td className="py-0.5 font-medium">{field.value}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <p
        className={
          preview.verdict_is_reassuring
            ? "text-muted-foreground mt-3 text-xs"
            : "mt-3 text-xs text-amber-600 dark:text-amber-500"
        }
      >
        {preview.verdict}
      </p>

      <div className="mt-3 flex justify-end gap-2">
        <Button variant="outline" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        <Button size="sm" onClick={onConfirm}>
          Looks right — run the rest
        </Button>
      </div>
    </Card>
  );
}

/**
 * §4.6's Stop and Pause, over §4.10's non-blocking execution.
 *
 * A fixed card rather than a modal, deliberately: §4.10 says a run "does not
 * lock the window", and a modal overlay would lock it in the only sense the
 * user cares about even though the run thread carried on underneath.
 *
 * Stop is styled destructive and Pause is not, because §4.6 makes them
 * genuinely different — Stop is permanent, Pause is not — and two identical
 * buttons would hide that.
 */
function RunControlOverlay({
  status,
  onPause,
  onResume,
  onStop,
  onDismiss,
}: {
  status: RunStatusView;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
  onDismiss: () => void;
}) {
  const paused = status.state === "paused";
  const finished = status.finished;

  return (
    <Card>
      <div className="flex items-center gap-2">
        <span
          aria-hidden
          className={
            finished
              ? "size-2 rounded-full bg-muted-foreground"
              : paused
                ? "size-2 rounded-full bg-amber-500"
                : "size-2 animate-pulse rounded-full bg-emerald-500"
          }
        />
        <p className="text-sm font-medium">
          {finished ? "Run finished" : paused ? "Paused" : "Running"}
        </p>
      </div>

      {paused && !finished ? (
        <p className="text-muted-foreground mt-1 text-xs">
          The record it was part-way through will be redone from the start when
          you resume.
        </p>
      ) : null}

      <div className="mt-3 flex justify-end gap-2">
        {finished ? (
          <Button variant="outline" size="sm" onClick={onDismiss}>
            Close
          </Button>
        ) : (
          <>
            {paused ? (
              <Button variant="outline" size="sm" onClick={onResume}>
                Resume
              </Button>
            ) : (
              <Button variant="outline" size="sm" onClick={onPause}>
                Pause
              </Button>
            )}
            <Button variant="destructive" size="sm" onClick={onStop}>
              Stop
            </Button>
          </>
        )}
      </div>
    </Card>
  );
}

/** Fixed, not modal — see `RunControlOverlay`. */
function Card({
  children,
  wide,
}: {
  children: React.ReactNode;
  wide?: boolean;
}) {
  return (
    <div
      role="status"
      aria-live="polite"
      className={`bg-background fixed right-4 bottom-4 z-50 rounded-md border p-4 shadow-lg ${
        wide ? "w-[28rem] max-w-[calc(100vw-2rem)]" : "w-80"
      }`}
    >
      {children}
    </div>
  );
}
