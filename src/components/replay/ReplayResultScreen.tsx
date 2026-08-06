import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { ReplayReportView } from "@/components/record-mode/types";

type ReplayResultScreenProps = {
  report: ReplayReportView;
  onDone: () => void;
};

const STATUS_LABEL: Record<string, string> = {
  completed: "Completed",
  failed: "Failed",
  aborted: "Aborted",
};

function statusClass(status: string): string {
  switch (status) {
    case "completed":
      return "text-green-700 dark:text-green-400";
    case "failed":
      return "text-destructive font-semibold";
    case "aborted":
      return "text-amber-700 dark:text-amber-400 font-semibold";
    default:
      return "font-semibold";
  }
}

export function ReplayResultScreen({ report, onDone }: ReplayResultScreenProps) {
  const statusLabel = STATUS_LABEL[report.status] ?? report.status;
  const failureCount = report.outcomes.filter((o) => o.is_failure).length;

  return (
    <main className="flex min-h-svh flex-col items-center gap-4 p-8">
      <div className="flex w-full max-w-2xl flex-col items-center gap-1">
        <h1 className="text-xl font-semibold tracking-tight">Replay finished</h1>
        <p className="text-muted-foreground text-center text-sm">
          &ldquo;{report.playbook_name}&rdquo;
        </p>
      </div>

      <div className="flex w-full max-w-2xl flex-col gap-2 rounded-md border p-4">
        <Row label="Status">
          <span className={statusClass(report.status)}>{statusLabel}</span>
        </Row>
        <Row label="Steps attempted">
          {report.steps_attempted} of {report.steps_total}
        </Row>
        {failureCount > 0 ? (
          <Row label="Failed steps">
            <span className="text-destructive font-semibold">{failureCount}</span>
          </Row>
        ) : null}
        <Row label="Run ID" mono>
          {report.run_id}
        </Row>
      </div>

      {report.outcomes.length > 0 ? (
        <div className="w-full max-w-2xl flex-1 overflow-y-auto rounded-md border">
          <ul className="divide-y">
            {report.outcomes.map((outcome) => (
              <li
                key={outcome.step_order}
                className={cn(
                  "flex flex-col gap-1 p-3 text-sm",
                  outcome.is_failure && "bg-destructive/5",
                )}
              >
                <div className="flex items-center gap-2">
                  <span className="text-muted-foreground w-6 shrink-0 text-right text-xs tabular-nums">
                    {outcome.step_order}
                  </span>
                  <span className="rounded bg-secondary px-1.5 py-0.5 text-xs font-medium">
                    {outcome.action_type}
                  </span>
                  <span
                    className={cn(
                      "text-xs font-medium",
                      outcome.is_failure
                        ? "text-destructive"
                        : "text-muted-foreground",
                    )}
                  >
                    {outcome.result}
                    {outcome.is_failure ? " (failure)" : ""}
                  </span>
                </div>
                <p
                  className={cn(
                    "pl-8 text-xs",
                    outcome.is_failure
                      ? "text-destructive"
                      : "text-muted-foreground",
                  )}
                >
                  {outcome.detail}
                </p>
                {outcome.selector ? (
                  <p className="text-muted-foreground pl-8 truncate font-mono text-xs">
                    {outcome.selector}
                  </p>
                ) : null}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="text-muted-foreground text-sm">No step outcomes recorded.</p>
      )}

      <Button onClick={onDone}>Done</Button>
    </main>
  );
}

function Row({
  label,
  children,
  mono,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
}) {
  return (
    <div className="flex items-center justify-between gap-3 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span className={cn("text-right", mono && "font-mono text-xs")}>
        {children}
      </span>
    </div>
  );
}
