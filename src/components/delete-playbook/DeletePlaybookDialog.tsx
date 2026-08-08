import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import type { PlaybookSummaryView } from "@/components/record-mode";

type DeletePlaybookDialogProps = {
  open: boolean;
  playbook: PlaybookSummaryView | null;
  onConfirm: () => void;
  onCancel: () => void;
};

/**
 * Irreversible-delete confirmation for stored playbooks. Same blocking-modal
 * pattern as AccessibilityPermissionDialog and AutomationPreview — Radix
 * AlertDialog, ESC and click-outside disabled, only explicit buttons resolve it.
 */
export function DeletePlaybookDialog({
  open,
  playbook,
  onConfirm,
  onCancel,
}: DeletePlaybookDialogProps) {
  if (!playbook) {
    return null;
  }

  return (
    <AlertDialog
      open={open}
      onOpenChange={() => {
        // Controlled exclusively by Cancel/Delete below — see AutomationPreview.
      }}
    >
      <AlertDialogContent onEscapeKeyDown={(event) => event.preventDefault()}>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete this playbook?</AlertDialogTitle>
          <AlertDialogDescription>
            <span className="font-medium text-foreground">{playbook.name}</span>{" "}
            ({playbook.step_count} step
            {playbook.step_count === 1 ? "" : "s"}) will be permanently removed.
            This cannot be undone. Past run history is kept, but the playbook and
            its steps are gone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel onClick={onCancel}>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm} variant="destructive">
            Delete
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
