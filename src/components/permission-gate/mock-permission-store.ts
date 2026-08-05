/**
 * Mock local state standing in for a future backend permission command
 * (e.g. something like `get_accessibility_permission()` /
 * `record_accessibility_consent()`). No such command exists in src-tauri
 * yet — this is backend follow-up work needed before Step 11, the same
 * situation as Step 9's `get_playbook_detail` gap.
 *
 * This is a tiny external store rather than plain component state so that
 * every component/window checking permission within the same app session
 * sees the same answer — mirroring how the real backend-backed state would
 * behave once it exists. Resets on full app restart (in-memory only).
 */
type Listener = () => void;

let granted = false;
const listeners = new Set<Listener>();

export function getAccessibilityPermissionGranted(): boolean {
  return granted;
}

export function setAccessibilityPermissionGranted(value: boolean): void {
  if (granted === value) {
    return;
  }
  granted = value;
  listeners.forEach((listener) => listener());
}

export function subscribeAccessibilityPermission(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
