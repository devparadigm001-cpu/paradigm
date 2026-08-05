import type { PlaybookPreviewSummary } from "./types";

/**
 * Dev-only mock data standing in for a future `get_playbook_detail`
 * backend command. No such command exists yet — see .cursor/rules/core.mdc
 * for the commands that do. Field names/shapes intentionally match
 * `StoredPlaybookInfo` where they overlap.
 */
export const MOCK_PLAYBOOK_SAFE: PlaybookPreviewSummary = {
  playbook_id: "pb_9f21a3",
  name: "Send weekly status email",
  step_count: 6,
  irreversible_count: 0,
};

export const MOCK_PLAYBOOK_IRREVERSIBLE: PlaybookPreviewSummary = {
  playbook_id: "pb_4c88e0",
  name: "Submit expense report",
  step_count: 11,
  irreversible_count: 3,
};
