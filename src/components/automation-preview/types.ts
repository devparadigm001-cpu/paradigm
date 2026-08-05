/**
 * Variants of the shared Automation Preview confirmation modal.
 *
 * Phase 1 only implements "first-run" for real. The other four exist here
 * so the component's shape (and the discriminated union below) is already
 * right for Phase 2-4 to extend without a redesign — their bodies in
 * AutomationPreview.tsx are placeholder content until then.
 */
export type AutomationPreviewVariant =
  | "first-run"
  | "failure-state"
  | "drift-repair"
  | "vision-fallback"
  | "kill-switch-disabled";

/**
 * Mirrors the fields a future `get_playbook_detail(playbookId)` backend
 * command would plausibly return, reusing names already documented in
 * .cursor/rules/core.mdc (see `StoredPlaybookInfo`) so wiring the real
 * command later is a small change, not a rewrite.
 */
export type PlaybookPreviewSummary = {
  playbook_id: string;
  name: string;
  step_count: number;
  irreversible_count: number;
};

export type FirstRunPreviewData = {
  variant: "first-run";
  playbook: PlaybookPreviewSummary;
};

// --- Stub payloads for Phase 2-4 variants -----------------------------
// Shape-only for now; each reuses PlaybookPreviewSummary as a reasonable
// placeholder until the real per-variant data is designed.

export type FailureStatePreviewData = {
  variant: "failure-state";
  playbook: PlaybookPreviewSummary;
};

export type DriftRepairPreviewData = {
  variant: "drift-repair";
  playbook: PlaybookPreviewSummary;
};

export type VisionFallbackPreviewData = {
  variant: "vision-fallback";
  playbook: PlaybookPreviewSummary;
};

export type KillSwitchDisabledPreviewData = {
  variant: "kill-switch-disabled";
  playbook: PlaybookPreviewSummary;
};

export type AutomationPreviewData =
  | FirstRunPreviewData
  | FailureStatePreviewData
  | DriftRepairPreviewData
  | VisionFallbackPreviewData
  | KillSwitchDisabledPreviewData;
