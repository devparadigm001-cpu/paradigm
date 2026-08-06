/**
 * Tauri's invoke() rejects a failing `Result<T, String>` command with the
 * raw String from the Rust `Err(...)` — not an Error instance — so
 * `error.message` alone would miss the real backend message. Used
 * everywhere Step 11a calls a real command and needs to show that message
 * verbatim rather than swallowing it.
 */
export function describeError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}
