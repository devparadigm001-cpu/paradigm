/**
 * Mirrors the real backend types from src-tauri/src/commands.rs exactly —
 * these are live invoke() calls now (Step 11a), not mock data. Field names
 * are snake_case because #[derive(Serialize)] on those structs has no
 * rename_all attribute; only *command arguments* are camelCased by Tauri,
 * return values are not.
 */
export type CapturedActionView = {
  step_order: number;
  action_type: string;
  source_app: string;
  element_role: string | null;
  element_name: string | null;
  /** null both when nothing was captured AND when it was redacted — check would_redact to tell them apart. */
  payload_preview: string | null;
  would_redact: boolean;
  timestamp_ms: number;
};

export type CaptureSummary = {
  session_name: string;
  action_count: number;
  excluded_count: number;
  unmapped_events: number;
  actions: CapturedActionView[];
};

export type StoredPlaybookInfo = {
  playbook_id: string;
  label: string;
  step_count: number;
  irreversible_count: number;
  redacted_count: number;
  label_generated: boolean;
};

export type PlaybookSummaryView = {
  id: string;
  name: string;
  source: string;
  created_at: string;
  updated_at: string;
  step_count: number;
  irreversible_count: number;
};

export type StepOutcomeView = {
  step_order: number;
  action_type: string;
  selector: string | null;
  result: string;
  detail: string;
  is_failure: boolean;
};

export type ReplayReportView = {
  run_id: string;
  playbook_id: string;
  playbook_name: string;
  /** "completed" | "failed" | "aborted" */
  status: string;
  steps_total: number;
  steps_attempted: number;
  outcomes: StepOutcomeView[];
};
