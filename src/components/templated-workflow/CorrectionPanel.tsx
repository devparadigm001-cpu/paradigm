import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { describeError } from "@/lib/errors";
import type { CorrectionRequest, SelectionView } from "./types";

/**
 * §4.5's click-to-point correction panel, and the scope question that follows.
 *
 * ## Why this is not a dialog
 *
 * §4.5 is explicit: "a small, non-blocking floating panel — not a full blocking
 * dialog, since the user needs to be able to click in the live spreadsheet
 * underneath it". The entire interaction is *point at the right column*, which
 * is impossible if the app has taken a modal lock on input. So this is a fixed
 * card with no overlay, no focus trap, and nothing that stops the user clicking
 * away into Sheets — which is the one thing they have to be able to do.
 *
 * ## The three steps, and why each exists
 *
 * 1. **Offer the best guess first.** `Drift::Moved` already knows where the
 *    column went, so the common case is one click. Asking someone to go and
 *    point at a column the system has already located would be busywork.
 * 2. **Confirm what was understood.** §4.5 wants this specifically as a guard
 *    against a mis-click, and quotes the form: "You selected column D — 'Client
 *    Phone.' Use this for Customer Name?" A wrong column silently accepted is
 *    the failure this step exists to prevent, and **Try again** re-opens the
 *    same prompt with nothing lost.
 * 3. **Ask the scope, always.** Permanent and one-off go to different backends
 *    and mean different things. There is no default and no remembered answer:
 *    §4.5's whole point is that "this one order was weird" and "the format
 *    actually changed" are different claims, and guessing which one the user
 *    meant would collapse the distinction the question exists to draw.
 */
type Step =
  | { name: "choose" }
  | { name: "pointing" }
  | { name: "confirm"; selection: SelectionView }
  | { name: "scope"; column: string; label: string | null }
  | { name: "done"; message: string };

export function CorrectionPanel({
  request,
  onResolved,
  onDismiss,
}: {
  request: CorrectionRequest;
  /** Called once a correction has been applied, with what happened. */
  onResolved: (summary: string) => void;
  onDismiss: () => void;
}) {
  const [step, setStep] = useState<Step>({ name: "choose" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const columnName = request.oldLabel
    ? `${request.oldLabel} (column ${request.oldLocator})`
    : `column ${request.oldLocator}`;

  async function readSelection() {
    setBusy(true);
    setError(null);
    try {
      const selection = await invoke<SelectionView>("read_selected_column", {
        playbookId: request.playbookId,
        side: request.side,
      });
      setStep({ name: "confirm", selection });
    } catch (e) {
      setError(describeError(e));
      setStep({ name: "choose" });
    } finally {
      setBusy(false);
    }
  }

  async function apply(permanent: boolean, column: string, label: string | null) {
    setBusy(true);
    setError(null);
    try {
      if (permanent) {
        await invoke("apply_permanent_correction", {
          playbookId: request.playbookId,
          side: request.side,
          oldLocator: request.oldLocator,
          newLocator: column,
          // The shape is a locator-plus-label pair, so a correction that moved
          // the column without saying what it is now would leave a recorded
          // shape that no longer describes the sheet.
          newLabel: label ?? column,
        });
        onResolved(
          `${columnName} now reads from column ${column}, from now on.`,
        );
      } else {
        if (!request.sourceRow) {
          throw new Error("no record is in progress for a one-off correction");
        }
        await invoke("apply_one_off_correction", {
          sourceRow: request.sourceRow,
          side: request.side,
          oldLocator: request.oldLocator,
          newLocator: column,
        });
        onResolved(
          `Row ${request.sourceRow} will use column ${column}. Everything else is unchanged.`,
        );
      }
      setStep({ name: "done", message: "Applied." });
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    // Fixed, not modal: no backdrop, no focus trap. The user must be able to
    // click straight through to the spreadsheet.
    <div
      role="dialog"
      aria-label="Correct a column"
      className="bg-background fixed right-4 bottom-4 z-50 w-[26rem] max-w-[calc(100vw-2rem)] rounded-md border p-4 shadow-lg"
    >
      <h2 className="text-sm font-semibold">Which column should this use?</h2>
      <p className="text-muted-foreground mt-1 text-xs">{request.detail}</p>

      {error ? <p className="text-destructive mt-2 text-sm">{error}</p> : null}

      {step.name === "choose" ? (
        <div className="mt-3 flex flex-col gap-2">
          {/* §4.5: best guess first, when there is a plausible one. */}
          {request.bestGuess ? (
            <Button
              size="sm"
              disabled={busy}
              onClick={() =>
                setStep({
                  name: "confirm",
                  selection: { column: request.bestGuess!, label: null },
                })
              }
            >
              Looks like column {request.bestGuess} now?
            </Button>
          ) : null}
          <p className="text-muted-foreground text-xs">
            Or click the correct column in your spreadsheet, then:
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => void readSelection()}
          >
            {busy ? "Checking..." : "Use the column I selected"}
          </Button>
          <Button variant="ghost" size="sm" disabled={busy} onClick={onDismiss}>
            Cancel
          </Button>
        </div>
      ) : null}

      {/* §4.5's confirm-before-locking-in, worded as the design words it. */}
      {step.name === "confirm" ? (
        <div className="mt-3 flex flex-col gap-2">
          <p className="text-sm">
            You selected <span className="font-mono">column {step.selection.column}</span>
            {step.selection.label ? ` — "${step.selection.label}."` : "."} Use
            this for {columnName}?
          </p>
          <div className="flex justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => setStep({ name: "choose" })}
            >
              Try again
            </Button>
            <Button
              size="sm"
              disabled={busy}
              onClick={() =>
                setStep({
                  name: "scope",
                  column: step.selection.column,
                  label: step.selection.label,
                })
              }
            >
              Use this
            </Button>
          </div>
        </div>
      ) : null}

      {/* §4.5's correction scope, asked explicitly and never defaulted. */}
      {step.name === "scope" ? (
        <div className="mt-3 flex flex-col gap-2">
          <p className="text-sm">
            Should this be permanent, or was it just this one record?
          </p>
          <Button
            size="sm"
            disabled={busy}
            onClick={() => void apply(true, step.column, step.label)}
          >
            The format changed — use column {step.column} from now on
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={busy || !request.sourceRow}
            onClick={() => void apply(false, step.column, step.label)}
          >
            {request.sourceRow
              ? `Just row ${request.sourceRow} was odd — this time only`
              : "One-off (needs a run in progress)"}
          </Button>
          {!request.sourceRow ? (
            <p className="text-muted-foreground text-xs">
              A one-off correction applies to a record in a run, and no run is
              in progress.
            </p>
          ) : null}
        </div>
      ) : null}

      {step.name === "done" ? (
        <div className="mt-3 flex justify-end">
          <Button size="sm" onClick={onDismiss}>
            Close
          </Button>
        </div>
      ) : null}
    </div>
  );
}
