/**
 * Mirrors the templated-workflow types in src-tauri/src/commands.rs exactly.
 *
 * Field names are snake_case: `#[derive(Serialize)]` on those structs carries
 * no `rename_all`, so only *command arguments* are camelCased by Tauri, never
 * return values. The one exception is `PreviewOutcome`, whose ENUM has
 * `rename_all = "camelCase"` — that renames the variant tags ("nothingToDo"),
 * not the fields inside them.
 */

/** One field of a proposed mapping. */
export type MappedField = {
  from: string;
  to: string;
};

/** A repeating pattern detection found, offered for confirmation (§4.2). */
export type TemplateProposal = {
  source: string;
  destination: string;
  fields: MappedField[];
  source_step: number;
  destination_step: number;
  examples: number;
};

/** One mapped field of the record about to be written (§4.3). */
export type PreviewFieldView = {
  source_field: string;
  source_label: string | null;
  destination_field: string;
  destination_label: string | null;
  /** The real value. Shown, never stored — see §3. */
  value: string;
};

export type PreviewView = {
  playbook_id: string;
  source_row: string;
  destination_row: number;
  fields: PreviewFieldView[];
  /**
   * The local model's read on the mapping. **Advisory only** — the measured
   * confidence band does not separate sensible mappings from nonsense, so this
   * informs the user rather than deciding. Never gate the UI on it.
   */
  verdict: string;
  verdict_is_reassuring: boolean;
};

/**
 * `PreviewOutcome` is an internally-tagged enum: the `kind` discriminates, and
 * the variant's fields sit alongside it.
 */
export type PreviewOutcome =
  | ({ kind: "ready" } & PreviewView)
  | { kind: "nothingToDo"; reason: string }
  | { kind: "needsAttention"; reason: string };

/** The answer to "is there anything new to do?" (§4.8). */
export type NewBatchView = {
  playbook_id: string;
  has_work: boolean;
  count: number;
  first_row: string | null;
  /** The count is a floor: the scan stopped at its ceiling. */
  capped: boolean;
  message: string;
};

/** What the running-state overlay renders (§4.6). */
export type RunStatusView = {
  playbook_id: string | null;
  /** "idle" | "running" | "paused" */
  state: string;
  /** True once the run thread has ended, whatever the reason. */
  finished: boolean;
};
