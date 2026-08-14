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
  | { kind: "nothingToDo"; reason: string; corrections: CorrectionRequestView[] }
  | { kind: "needsAttention"; reason: string; corrections: CorrectionRequestView[] };

/** One blocking drift, structured enough to drive the correction panel. */
export type CorrectionRequestView = {
  side: CorrectionSide;
  old_locator: string;
  old_label: string | null;
  best_guess: string | null;
  detail: string;
};

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

/** One record worth looking at (§4.9). */
export type FlaggedRecordView = {
  source_row: string;
  destination_row: string;
  missing_fields: string[];
};

/** §4.9's end-of-run summary: quiet by default, detailed only when it matters. */
export type RunSummaryView = {
  playbook_id: string;
  /** "completed" | "stopped" | "needs_correction" | "failed" | "incomplete" */
  status: string;
  /** The one plain line a clean run gets. */
  headline: string;
  /** "45–57", or null when nothing was written. */
  processed_range: string | null;
  written: number;
  skipped: number;
  /**
   * §4.9: "shown only when greater than zero". Render nothing at 0 — a
   * "0 flagged for review" line is exactly the noise quiet-by-default avoids.
   */
  flagged: number;
  /** Whether the report should expand at all. */
  needs_attention: boolean;
  /** Empty on a clean run. */
  flagged_records: FlaggedRecordView[];
  /** Why the run ended, when it was not ordinary exhaustion. */
  stop_reason: string | null;
};

/** Which side of a mapping a correction applies to. */
export type CorrectionSide = "source" | "destination";

/** What the user has selected in the live spreadsheet (§4.5). */
export type SelectionView = {
  column: string;
  /** The header at that column. Null when the column has none. */
  label: string | null;
};

/**
 * What the correction panel is being asked to fix.
 *
 * Built from a blocking `DriftFinding`: the side and the locator that moved,
 * plus the best guess when the backend has one (`Drift::Moved` carries it).
 */
export type CorrectionRequest = {
  playbookId: string;
  side: CorrectionSide;
  /** The locator the template names, and which is now wrong. */
  oldLocator: string;
  /** What that column was called when the workflow was confirmed. */
  oldLabel: string | null;
  /** §4.5's "Looks like column D now?" — null when there is no plausible one. */
  bestGuess: string | null;
  /** Human-readable description of what drifted. */
  detail: string;
  /**
   * The source row a one-off would apply to. Null when no run is in progress,
   * which is what makes the one-off option unavailable — a one-off correction
   * with no record to attach to is not a thing §4.5 describes.
   */
  sourceRow: string | null;
};

/** What the running-state overlay renders (§4.6). */
export type RunStatusView = {
  playbook_id: string | null;
  /**
   * §4.5: the record a supervised run has stopped on. Null on an ordinary
   * run -- which is what keeps the panel's one-off branch unavailable unless
   * there is genuinely a record to attach it to.
   */
  awaiting_row: string | null;
  /** The mapped columns that were empty on that record. */
  awaiting_missing_fields: string[];
  /** "idle" | "running" | "paused" */
  state: string;
  /** True once the run thread has ended, whatever the reason. */
  finished: boolean;
};
