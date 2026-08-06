import {
  getCurrentWebviewWindow,
  WebviewWindow,
} from "@tauri-apps/api/webviewWindow";

export const PROOF_WINDOW_PREFIX = "proof-";

/**
 * Fixed (not timestamped) label: Phase 1 allows at most one recording at a
 * time, so there is at most one badge window, and callers need a stable way
 * to find/close "the" badge rather than tracking a generated label.
 */
export const RECORDING_BADGE_WINDOW_LABEL = "recording-badge";

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function getWindowLabel(): string | null {
  if (!isTauriRuntime()) {
    return null;
  }

  return getCurrentWebviewWindow().label;
}

export function isProofWindow(label: string | null): boolean {
  return label?.startsWith(PROOF_WINDOW_PREFIX) ?? false;
}

export function isRecordingBadgeWindow(label: string | null): boolean {
  return label === RECORDING_BADGE_WINDOW_LABEL;
}

export async function openProofWindow(): Promise<void> {
  const label = `${PROOF_WINDOW_PREFIX}${Date.now()}`;

  const existing = await WebviewWindow.getByLabel(label);
  if (existing) {
    await existing.setFocus();
    return;
  }

  const proofWindow = new WebviewWindow(label, {
    url: "/",
    title: "Proof Window",
    width: 420,
    height: 320,
    center: true,
    resizable: true,
  });

  await new Promise<void>((resolve, reject) => {
    proofWindow.once("tauri://created", () => resolve());
    proofWindow.once("tauri://error", (event) => {
      const message =
        typeof event.payload === "string"
          ? event.payload
          : "Failed to create proof window.";
      reject(new Error(message));
    });
  });
}

/**
 * Open the floating "Recording... [Stop]" badge: alwaysOnTop, undecorated,
 * transparent, and non-focus-stealing so the user can keep working in
 * whatever app they're recording. It talks to the main window purely
 * through Tauri events (see src/components/record-mode) — separate
 * webviews, no shared React state.
 */
export async function openRecordingBadgeWindow(): Promise<void> {
  const existing = await WebviewWindow.getByLabel(RECORDING_BADGE_WINDOW_LABEL);
  if (existing) {
    await existing.setFocus();
    return;
  }

  const badgeWindow = new WebviewWindow(RECORDING_BADGE_WINDOW_LABEL, {
    url: "/",
    title: "Recording",
    width: 190,
    height: 56,
    x: 40,
    y: 40,
    resizable: false,
    maximizable: false,
    minimizable: false,
    alwaysOnTop: true,
    decorations: false,
    transparent: true,
    shadow: false,
    skipTaskbar: true,
    focus: false,
  });

  await new Promise<void>((resolve, reject) => {
    badgeWindow.once("tauri://created", () => resolve());
    badgeWindow.once("tauri://error", (event) => {
      const message =
        typeof event.payload === "string"
          ? event.payload
          : "Failed to create recording badge window.";
      reject(new Error(message));
    });
  });
}

/** Close the badge window if it exists. A no-op if it's already gone. */
export async function closeRecordingBadgeWindow(): Promise<void> {
  const existing = await WebviewWindow.getByLabel(RECORDING_BADGE_WINDOW_LABEL);
  if (existing) {
    await existing.close();
  }
}
