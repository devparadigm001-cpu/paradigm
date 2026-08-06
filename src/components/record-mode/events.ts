/**
 * Tauri event names shared between the main window and the recording badge
 * window. They're separate webviews with no shared React state, so this —
 * Tauri's event system — is the only channel between them, per the locked
 * window strategy.
 */

/** Rust emits this (see src-tauri/src/lib.rs) when the global shortcut fires. */
export const RECORD_MODE_TOGGLE_SHORTCUT_EVENT = "record-mode:toggle-shortcut";

/** The badge window emits this when its own Stop button is clicked. */
export const RECORD_MODE_STOP_REQUESTED_EVENT = "record-mode:stop-requested";
