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

type AccessibilityPermissionDialogProps = {
  open: boolean;
  onAllow: () => void;
  onDecline: () => void;
};

/**
 * Just-in-time consent gate for Windows accessibility-tree access, shown
 * the first time a feature needs to read/act on screen content (e.g.
 * Record Mode's record button — real wiring lands in Step 11).
 *
 * Deliberately a separate component from <AutomationPreview>: this isn't
 * confirming an automation action, it's consent before the app can
 * observe/act on screen content at all (same precedent as Delete-My-Data
 * in the locked docs). It reuses the same blocking-modal pattern — Radix
 * AlertDialog, ESC and click-outside disabled, only explicit buttons
 * resolve it — because that pattern is proven correct, not because this is
 * a sixth AutomationPreview variant.
 */
export function AccessibilityPermissionDialog({
  open,
  onAllow,
  onDecline,
}: AccessibilityPermissionDialogProps) {
  return (
    <AlertDialog
      open={open}
      onOpenChange={() => {
        // Intentional no-op — same rationale as AutomationPreview.tsx.
        // Radix's AlertDialogContent already hardcodes outside-click to be
        // a no-op internally; onEscapeKeyDown is blocked below. Ignoring
        // onOpenChange here is a third, redundant layer: `open` can only
        // change via onAllow/onDecline.
      }}
    >
      <AlertDialogContent onEscapeKeyDown={(event) => event.preventDefault()}>
        <AlertDialogHeader>
          <AlertDialogTitle>Allow Paradigm to read your screen?</AlertDialogTitle>
          <AlertDialogDescription>
            Placeholder copy, not final. To record and replay this workflow,
            Paradigm needs to read what&apos;s currently on screen and
            interact with UI elements, using Windows accessibility APIs.
            This only happens while you&apos;re actively recording or
            replaying an automation.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel onClick={onDecline}>Decline</AlertDialogCancel>
          <AlertDialogAction onClick={onAllow}>Allow</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
