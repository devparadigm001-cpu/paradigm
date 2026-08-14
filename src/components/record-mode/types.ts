/**
 * Mirrors the real backend types from src-tauri/src/commands.rs exactly —
 * these are live invoke() calls now (Step 11a), not mock data. Field names
 * are snake_case because #[derive(Serialize)] on those structs has no
 * rename_all attribute; only *command arguments* are camelCased by Tauri,
 * return values are not.
 */
import type { TemplateProposal } from "@/components/templated-workflow/types";

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
  /**
   * Pastes seen. Non-zero means the recording may be missing data movement
   * that no action records.
   */
  pastes_observed: number;
  actions: CapturedActionView[];
  /**
   * The repeating pattern detection found on stop, if any (§4.1/§4.12).
   * Null for an ordinary recording, which is most of them.
   */
  template: TemplateProposal | null;
  /**
   * Why no pattern was offered, when none was — worded as the next thing to
   * do. Null both when a pattern WAS found and when the recording never
   * copied anything between grids, which is not a failure and must not read
   * as one.
   */
  no_template_reason: string | null;
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
  /**
   * A confirmed repeating workflow (§4.11). Section 6 reuses this one list
   * rather than adding a parallel screen, so this is what the templated
   * behaviour attaches to.
   */
  is_templated: boolean;
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
