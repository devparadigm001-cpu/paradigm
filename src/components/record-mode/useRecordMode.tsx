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
    } catch (e) {
      // Nothing was started, so there is nothing to unwind.
      setError(describeError(e));
      setPhase("idle");
      return;
    }

    // Past this point THE BACKEND IS RECORDING, and anything that fails from
    // here has to put that back. Otherwise the session is orphaned: the UI
    // returns to idle while `state.session` stays occupied, and the next Start
    // is refused with "a recording session is already active" while nothing
    // appears to be recording.
    //
    // That is a real reported symptom, and it came from this function treating
    // a badge-window failure as though the start had never happened. The
    // window is a separate webview and its creation can genuinely fail --
    // `openRecordingBadgeWindow` rejects on `tauri://error`.
    //
    // `stop` already reasons this way in the other direction: it goes idle on
    // error precisely BECAUSE the backend has released the session. Now the
    // two agree about who is holding it.
    try {
      await openRecordingBadgeWindow();
      setPhase("recording");
    } catch (e) {
      setError(describeError(e));
      try {
        await invoke("stop_record_session");
      } catch {
        // Deliberately swallowed, and the badge failure is what gets reported.
        // That is the one the user can act on, and `stop_record_session` frees
        // the slot whether or not it errors afterwards -- so replacing the
        // message would trade a useful error for a less useful one.
      }
      // A half-created badge would otherwise sit there owning the label and
      // making the NEXT start fail the same way.
      await closeRecordingBadgeWindow();
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

  /**
   * Ask the backend what is actually true, once, on mount.
   *
   * This hook's phase starts at "idle" because that is the only sensible
   * initial guess — but it IS a guess, and a webview reload makes it wrong.
   * React state resets; `state.session` in the Rust process does not. The user
   * then sees an idle UI over a live recording, and Start is refused with "a
   * recording session is already active" while nothing appears to be
   * recording.
   *
   * Adopted rather than cancelled. The session is still capturing, and the
   * recording belongs to the user — silently throwing it away to make the UI
   * tidy would destroy work they never asked to lose. Reopening the badge is a
   * no-op if it survived the reload.
   */
  useEffect(() => {
    if (!isTauriRuntime()) {
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const active = await invoke<boolean>("record_session_active");
        if (cancelled || !active || phaseRef.current !== "idle") {
          return;
        }
        await openRecordingBadgeWindow();
        if (!cancelled) {
          setPhase("recording");
        }
      } catch {
        // A failed reconciliation must not block Record Mode. The worst case
        // is the state this already had before asking.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

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
