import { useCallback, useState } from "react";
import type { PlaybookSummaryView } from "@/components/record-mode";
import { DeletePlaybookDialog } from "./DeletePlaybookDialog";

type PendingDelete = {
  playbook: PlaybookSummaryView;
  action: () => void | Promise<void>;
};

type DeleteState =
  | { open: false; pending: null }
  | { open: true; pending: PendingDelete };

const CLOSED_STATE: DeleteState = { open: false, pending: null };

export type UseDeletePlaybookConfirmationOptions = {
  onCancel?: (playbook: PlaybookSummaryView) => void;
  onConfirmError?: (error: unknown, playbook: PlaybookSummaryView) => void;
};

/**
 * Gates `delete_playbook` behind an explicit confirmation dialog — same
 * non-bypassable shape as useAutomationPreview / useAccessibilityPermissionGate.
 */
export function useDeletePlaybookConfirmation(
  options: UseDeletePlaybookConfirmationOptions = {},
) {
  const [state, setState] = useState<DeleteState>(CLOSED_STATE);

  const requestDeleteConfirmation = useCallback(
    (playbook: PlaybookSummaryView, action: () => void | Promise<void>) => {
      setState({ open: true, pending: { playbook, action } });
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
        options.onConfirmError?.(error, pending.playbook);
      });
  }, [state.pending, options]);

  const handleCancel = useCallback(() => {
    const pending = state.pending;
    setState(CLOSED_STATE);
    if (pending) {
      options.onCancel?.(pending.playbook);
    }
  }, [state.pending, options]);

  const deleteDialogElement = (
    <DeletePlaybookDialog
      open={state.open}
      playbook={state.pending?.playbook ?? null}
      onConfirm={handleConfirm}
      onCancel={handleCancel}
    />
  );

  return { requestDeleteConfirmation, deleteDialogElement } as const;
}
