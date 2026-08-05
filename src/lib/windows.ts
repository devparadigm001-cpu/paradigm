import {
  getCurrentWebviewWindow,
  WebviewWindow,
} from "@tauri-apps/api/webviewWindow";

export const PROOF_WINDOW_PREFIX = "proof-";

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
