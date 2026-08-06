import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAccessibilityPermissionGate } from "@/components/permission-gate";
import {
  closeRecordingBadgeWindow,
  isTauriRuntime,
  openRecordingBadgeWindow,
} from "@/lib/windows";
import { describeError } from "@/lib/errors";
import {
  RECORD_MODE_STOP_REQUESTED_EVENT,
  RECORD_MODE_TOGGLE_SHORTCUT_EVENT,
} from "./events";
import type { CaptureSummary } from "./types";

export type RecordPhase = "idle" | "starting" | "recording" | "stopping";

/**
 * Owns the entire Record Mode start/stop lifecycle: the permission gate,
 * the real start_record_session()/stop_record_session() calls, opening and
 * closing the badge window, and reacting to the two external triggers that
 * must run the exact same flow as the main window's own button — the
 * global shortcut and the badge window's own Stop click. Both arrive here
 * as Tauri events, never as direct function calls across windows.
 *
 * `toggle` is the only entry point: which underlying action it runs
 * (start vs. stop) is decided from the current phase, not by the caller,
 * so the button, the shortcut, and the badge's Stop button can't drift
 * into calling different things.
 */
export function useRecordMode() {
  const [phase, setPhase] = useState<RecordPhase>("idle");
  const [captureSummary, setCaptureSummary] = useState<CaptureSummary | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);

  const {
    requestAction: requestPermissionGatedAction,
    gateElement: permissionGateElement,
    isGranted: hasAccessibilityPermission,
  } = useAccessibilityPermissionGate();

  const phaseRef = useRef(phase);
  phaseRef.current = phase;

  const start = useCallback(async () => {
    setError(null);
    setPhase("starting");
    try {
      await invoke<string>("start_record_session");
      await openRecordingBadgeWindow();
      setPhase("recording");
    } catch (e) {
      setError(describeError(e));
      setPhase("idle");
    }
  }, []);

  const stop = useCallback(async () => {
    setPhase("stopping");
    try {
      const summary = await invoke<CaptureSummary>("stop_record_session");
      await closeRecordingBadgeWindow();
      setCaptureSummary(summary);
      setPhase("idle");
    } catch (e) {
      setError(describeError(e));
      // The backend already took the session out of play (stop_record_session
      // takes it whether or not it errors afterwards), so there is nothing
      // left to be "recording" — idle is the honest state to show.
      setPhase("idle");
    }
  }, []);

  const toggle = useCallback(() => {
    const current = phaseRef.current;
    if (current === "idle") {
      requestPermissionGatedAction(start);
    } else if (current === "recording") {
      void stop();
    }
    // "starting"/"stopping": a request is already in flight, ignore.
  }, [requestPermissionGatedAction, start, stop]);

  useEffect(() => {
    if (!isTauriRuntime()) {
      return;
    }
    let unlistenShortcut: (() => void) | undefined;
    let unlistenBadgeStop: (() => void) | undefined;
    let cancelled = false;

    void (async () => {
      const offShortcut = await listen(RECORD_MODE_TOGGLE_SHORTCUT_EVENT, () => {
        toggle();
      });
      const offBadgeStop = await listen(RECORD_MODE_STOP_REQUESTED_EVENT, () => {
        toggle();
      });
      if (cancelled) {
        offShortcut();
        offBadgeStop();
        return;
      }
      unlistenShortcut = offShortcut;
      unlistenBadgeStop = offBadgeStop;
    })();

    return () => {
      cancelled = true;
      unlistenShortcut?.();
      unlistenBadgeStop?.();
    };
  }, [toggle]);

  const dismissCaptureSummary = useCallback(() => {
    setCaptureSummary(null);
  }, []);

  return {
    phase,
    captureSummary,
    error,
    hasAccessibilityPermission,
    toggle,
    dismissCaptureSummary,
    permissionGateElement,
  } as const;
}
