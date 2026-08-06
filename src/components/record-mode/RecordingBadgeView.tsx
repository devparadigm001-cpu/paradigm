import { useEffect } from "react";
import { emit } from "@tauri-apps/api/event";
import { RECORD_MODE_STOP_REQUESTED_EVENT } from "./events";

/**
 * Content of the floating recording badge window. This is a SEPARATE
 * webview from the main window (see openRecordingBadgeWindow in
 * src/lib/windows.ts) — it has no access to the main window's React state,
 * and does not call stop_record_session() itself. Clicking Stop only emits
 * a Tauri event; the main window's useRecordMode hook is what actually
 * calls the backend, closes this window, and navigates to the review
 * screen, so there is exactly one place that owns that flow.
 */
export function RecordingBadgeView() {
  useEffect(() => {
    // The shared index.css gives every window an opaque bg-background body,
    // which would defeat this window's `transparent: true` config. Overridden
    // here rather than globally so the main window's background is untouched.
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
  }, []);

  function handleStop() {
    void emit(RECORD_MODE_STOP_REQUESTED_EVENT);
  }

  return (
    <div className="flex h-svh w-full items-center justify-center bg-transparent">
      <div
        data-tauri-drag-region
        className="flex items-center gap-2 rounded-full border border-white/10 bg-neutral-900/95 px-3 py-1.5 text-white shadow-lg select-none"
      >
        <span className="relative flex size-2.5 shrink-0">
          <span className="absolute inline-flex size-full animate-ping rounded-full bg-red-500 opacity-75" />
          <span className="relative inline-flex size-2.5 rounded-full bg-red-500" />
        </span>
        <span className="text-xs font-medium whitespace-nowrap">
          Recording...
        </span>
        <button
          type="button"
          onClick={handleStop}
          className="cursor-pointer rounded-full bg-white/15 px-2.5 py-1 text-xs font-semibold whitespace-nowrap transition-colors hover:bg-white/25"
        >
          Stop
        </button>
      </div>
    </div>
  );
}
