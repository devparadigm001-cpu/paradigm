import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `PlaybookSummaryView` in src-tauri/src/commands.rs. Tauri serialises
 * the Rust struct's fields verbatim, so these names are the contract.
 */
type PlaybookSummary = {
  id: string;
  name: string;
  source: string;
  created_at: string;
  updated_at: string;
  step_count: number;
  irreversible_count: number;
};

/**
 * Stored playbooks, with a delete control.
 *
 * The delete exists because there was previously no way to remove a recording
 * at any layer, and capture is system-wide -- a user who realises they recorded
 * something they did not mean to needs an answer to "delete that". See
 * docs/known-issues/no-way-to-delete-playbooks.md.
 *
 * Deletion is irreversible and there is no undo, so it is confirmed, and the
 * confirmation NAMES the playbook rather than asking a generic "are you sure?".
 * A generic prompt is the one most likely to be clicked through by reflex, and
 * the mistake it would wave past -- deleting the wrong recording -- cannot be
 * taken back.
 */
export function PlaybookList() {
  const [playbooks, setPlaybooks] = useState<PlaybookSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  /** Which playbook is awaiting confirmation, if any. */
  const [pendingDelete, setPendingDelete] = useState<PlaybookSummary | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setPlaybooks(await invoke<PlaybookSummary[]>("list_playbooks"));
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function confirmDelete(playbook: PlaybookSummary) {
    setBusyId(playbook.id);
    try {
      await invoke("delete_playbook", { playbookId: playbook.id });
      setPendingDelete(null);
      setError(null);
      // Re-read rather than removing it locally: the list should reflect the
      // store, not this component's guess about what the store now contains.
      await refresh();
    } catch (e) {
      // The command errors on an unknown id rather than reporting a silent
      // success, so this is worth showing rather than swallowing.
      setError(`Could not delete "${playbook.name}": ${e}`);
      setPendingDelete(null);
    } finally {
      setBusyId(null);
    }
  }

  return (
    <section className="playbooks">
      <h2>Saved playbooks</h2>

      {error && (
        <p role="alert" className="playbooks-error">
          {error}
        </p>
      )}

      {loading && <p>Loading…</p>}

      {!loading && playbooks.length === 0 && (
        <p>No playbooks saved yet.</p>
      )}

      <ul className="playbook-list">
        {playbooks.map((p) => (
          <li key={p.id} className="playbook-row">
            <span className="playbook-name">{p.name}</span>
            <span className="playbook-meta">
              {p.step_count} step{p.step_count === 1 ? "" : "s"}
              {p.irreversible_count > 0 && ` · ${p.irreversible_count} irreversible`}
            </span>
            <button
              type="button"
              aria-label={`Delete ${p.name}`}
              disabled={busyId === p.id}
              onClick={() => setPendingDelete(p)}
            >
              Delete
            </button>
          </li>
        ))}
      </ul>

      {pendingDelete && (
        <div role="dialog" aria-modal="true" className="confirm-delete">
          <p>
            Delete <strong>{pendingDelete.name}</strong> and its{" "}
            {pendingDelete.step_count} step
            {pendingDelete.step_count === 1 ? "" : "s"}?
          </p>
          <p className="confirm-detail">
            This cannot be undone. Past run history is kept.
          </p>
          <button
            type="button"
            onClick={() => void confirmDelete(pendingDelete)}
            disabled={busyId !== null}
          >
            Delete permanently
          </button>
          <button type="button" onClick={() => setPendingDelete(null)}>
            Cancel
          </button>
        </div>
      )}
    </section>
  );
}
