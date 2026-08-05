import { useCallback, useState } from "react";
import { AutomationPreview } from "./AutomationPreview";
import type { AutomationPreviewData } from "./types";

type PendingConfirmation = {
  data: AutomationPreviewData;
  action: () => void | Promise<void>;
};

type PreviewState =
  | { open: false; pending: null }
  | { open: true; pending: PendingConfirmation };

const CLOSED_STATE: PreviewState = { open: false, pending: null };

export type UseAutomationPreviewOptions = {
  onDeny?: (data: AutomationPreviewData) => void;
  onConfirmError?: (error: unknown, data: AutomationPreviewData) => void;
};

/**
 * Owns both the Automation Preview dialog AND the trigger for the action it
 * gates, so the two can't be pulled apart by mistake.
 *
 * This hook deliberately does NOT return a "just run it" function. The only
 * way to get `action` to execute is:
 *   1. call `requestConfirmation(data, action)`,
 *   2. render the returned `previewElement`,
 *   3. have the user click Confirm in that dialog.
 *
 * `action` is held in internal state, not handed back to the caller, and is
 * only ever invoked from this hook's own onConfirm handler — which is wired
 * solely to <AutomationPreview>'s Confirm button. There's no code path here
 * that fires the automation without the modal appearing first. (A caller
 * can of course still call their own `action` function directly instead of
 * going through `requestConfirmation` — nothing in JS can prevent that —
 * but doing so means visibly *not* using this hook, rather than an easy
 * accidental skip like forgetting to render a dialog.)
 */
export function useAutomationPreview(
  options: UseAutomationPreviewOptions = {},
) {
  const [state, setState] = useState<PreviewState>(CLOSED_STATE);

  const requestConfirmation = useCallback(
    (data: AutomationPreviewData, action: () => void | Promise<void>) => {
      setState({ open: true, pending: { data, action } });
    },
    [],
  );

  const handleConfirm = useCallback(() => {
    const pending = state.pending;
    setState(CLOSED_STATE);
    if (!pending) {
      return;
    }
    Promise.resolve()
      .then(() => pending.action())
      .catch((error: unknown) => {
        options.onConfirmError?.(error, pending.data);
      });
  }, [state.pending, options]);

  const handleDeny = useCallback(() => {
    const pending = state.pending;
    setState(CLOSED_STATE);
    if (pending) {
      options.onDeny?.(pending.data);
    }
  }, [state.pending, options]);

  const previewElement = (
    <AutomationPreview
      open={state.open}
      data={state.pending?.data ?? null}
      onConfirm={handleConfirm}
      onDeny={handleDeny}
    />
  );

  return { requestConfirmation, previewElement } as const;
}
