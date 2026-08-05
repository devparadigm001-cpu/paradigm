import { TriangleAlert } from "lucide-react";
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
import { cn } from "@/lib/utils";
import type {
  AutomationPreviewData,
  AutomationPreviewVariant,
  FirstRunPreviewData,
} from "./types";

type AutomationPreviewProps = {
  open: boolean;
  data: AutomationPreviewData | null;
  onConfirm: () => void;
  onDeny: () => void;
};

/**
 * Shared blocking confirmation modal used across Ghost Mode, Chat Mode, and
 * Form Memory (see .cursor/rules/core.mdc). One component, variant prop —
 * not five separate components.
 *
 * This is a security-relevant control: ESC and click-outside are disabled
 * for every variant, no exceptions. The only way out is Confirm or Deny.
 * Don't render this component directly to trigger an automation — use
 * `useAutomationPreview` instead, which couples the confirmation UI to the
 * action it gates so the action can't fire without this dialog appearing.
 */
export function AutomationPreview({
  open,
  data,
  onConfirm,
  onDeny,
}: AutomationPreviewProps) {
  if (!data) {
    return null;
  }

  const hasIrreversibleSteps = data.playbook.irreversible_count > 0;

  return (
    <AlertDialog
      open={open}
      onOpenChange={() => {
        // Intentional no-op. This dialog is controlled exclusively by the
        // Confirm/Deny handlers below. Radix's AlertDialogContent already
        // hardcodes onPointerDownOutside/onInteractOutside to preventDefault
        // internally (outside click can never close an AlertDialog — that
        // prop isn't even exposed to override), and we block onEscapeKeyDown
        // ourselves below. Ignoring onOpenChange here is a second, redundant
        // layer: even if one of those built-in guards were ever removed
        // upstream, the `open` state driving this dialog still can't flip
        // to false except through onConfirm/onDeny.
      }}
    >
      <AlertDialogContent onEscapeKeyDown={(event) => event.preventDefault()}>
        <AutomationPreviewBody data={data} />
        <AlertDialogFooter>
          <AlertDialogCancel onClick={onDeny}>Deny</AlertDialogCancel>
          <AlertDialogAction
            onClick={onConfirm}
            variant={hasIrreversibleSteps ? "destructive" : "default"}
          >
            Confirm
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function AutomationPreviewBody({ data }: { data: AutomationPreviewData }) {
  switch (data.variant) {
    case "first-run":
      return <FirstRunBody data={data} />;
    case "failure-state":
    case "drift-repair":
    case "vision-fallback":
    case "kill-switch-disabled":
      return <StubBody variant={data.variant} />;
  }
}

function FirstRunBody({ data }: { data: FirstRunPreviewData }) {
  const { playbook } = data;
  const hasIrreversible = playbook.irreversible_count > 0;

  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Run &ldquo;{playbook.name}&rdquo;?</AlertDialogTitle>
        <AlertDialogDescription>
          Placeholder copy, not final. Shown before this playbook&apos;s
          automation runs for the first time.
        </AlertDialogDescription>
      </AlertDialogHeader>

      <dl className="grid grid-cols-2 gap-x-4 gap-y-1.5 rounded-md border bg-muted/40 p-3 text-sm">
        <dt className="text-muted-foreground">Steps</dt>
        <dd className="text-right font-medium">{playbook.step_count}</dd>
        <dt className="text-muted-foreground">Irreversible actions</dt>
        <dd
          className={cn(
            "text-right font-medium",
            hasIrreversible && "text-destructive font-semibold",
          )}
        >
          {playbook.irreversible_count}
        </dd>
      </dl>

      {hasIrreversible ? (
        <div className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 p-3 text-sm text-destructive">
          <TriangleAlert className="mt-0.5 size-4 shrink-0" />
          <span>
            This playbook includes {playbook.irreversible_count} irreversible
            step{playbook.irreversible_count === 1 ? "" : "s"} (e.g. sending
            an email or submitting a form). Review carefully before
            confirming.
          </span>
        </div>
      ) : null}
    </>
  );
}

const STUB_COPY: Record<
  Exclude<AutomationPreviewVariant, "first-run">,
  string
> = {
  "failure-state": "Shown when a replay run fails partway through.",
  "drift-repair":
    "Shown when the target UI has drifted from what the playbook expects and a repair is proposed.",
  "vision-fallback":
    "Shown when the local model can't confidently label a step and the app falls back to a slower, paid cloud vision model.",
  "kill-switch-disabled":
    "Shown when a run is attempted while the kill switch has automation disabled.",
};

function StubBody({
  variant,
}: {
  variant: Exclude<AutomationPreviewVariant, "first-run">;
}) {
  return (
    <AlertDialogHeader>
      <AlertDialogTitle className="capitalize">
        {variant.replace(/-/g, " ")}
      </AlertDialogTitle>
      <AlertDialogDescription>
        Placeholder — not implemented yet (Phase 2+). {STUB_COPY[variant]}
      </AlertDialogDescription>
    </AlertDialogHeader>
  );
}
