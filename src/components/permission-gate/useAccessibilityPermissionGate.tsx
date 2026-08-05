import { useCallback, useState, useSyncExternalStore } from "react";
import { AccessibilityPermissionDialog } from "./AccessibilityPermissionDialog";
import {
  getAccessibilityPermissionGranted,
  setAccessibilityPermissionGranted,
  subscribeAccessibilityPermission,
} from "./mock-permission-store";

type PendingAction = {
  action: () => void | Promise<void>;
};

/**
 * Owns the entire permission-gate flow: checks current (mocked) permission
 * state, shows the consent dialog if not yet granted, and only invokes the
 * wrapped action after explicit Allow.
 *
 * Same non-bypassable shape as Step 9's useAutomationPreview. `requestAction`
 * is the only entry point calling code gets — it never receives a
 * standalone "just run the gated action" reference. The hook itself decides
 * whether to run `action` immediately (already granted) or hold it in state
 * until the user clicks Allow in the dialog rendered by `gateElement`.
 */
export function useAccessibilityPermissionGate() {
  const isGranted = useSyncExternalStore(
    subscribeAccessibilityPermission,
    getAccessibilityPermissionGranted,
  );
  const [pending, setPending] = useState<PendingAction | null>(null);

  const requestAction = useCallback((action: () => void | Promise<void>) => {
    if (getAccessibilityPermissionGranted()) {
      void action();
      return;
    }
    setPending({ action });
  }, []);

  const handleAllow = useCallback(() => {
    const action = pending?.action;
    setAccessibilityPermissionGranted(true);
    setPending(null);
    if (action) {
      void action();
    }
  }, [pending]);

  const handleDecline = useCallback(() => {
    // Phase 1 scope: declining just re-prompts next time requestAction is
    // called (permission stays ungranted, nothing is persisted). Real
    // permanent-denial UX — e.g. "don't ask again" plus a way to re-enable
    // from settings — is an open item, intentionally not built here.
    setPending(null);
  }, []);

  const gateElement = (
    <AccessibilityPermissionDialog
      open={pending !== null}
      onAllow={handleAllow}
      onDecline={handleDecline}
    />
  );

  return { requestAction, gateElement, isGranted } as const;
}
