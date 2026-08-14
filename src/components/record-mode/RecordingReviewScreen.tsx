import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { describeError } from "@/lib/errors";
import { TemplateProposalSection } from "@/components/templated-workflow";
import type { CapturedActionView, CaptureSummary, StoredPlaybookInfo } from "./types";

type ReviewItem = {
  /** Position of this action in the ORIGINAL CaptureSummary.actions array —
   * i.e. exactly what compile_and_store_playbook's step_indices expects.
   * Tracked per-item (not derived from array position) so it survives
   * reordering and deletion untouched. */
  originalIndex: number;
  action: CapturedActionView;
};

type RecordingReviewScreenProps = {
  summary: CaptureSummary;
  /** Called once the user is done with this screen — after acknowledging a
   * successful save, or after explicitly discarding. Either way there is
   * no more capture to review, so the caller clears it. */
  onDone: () => void;
};

function describeAction(action: CapturedActionView): string {
  const parts = [action.element_role, action.element_name].filter(
    (part): part is string => Boolean(part && part.trim()),
  );
  return parts.length > 0 ? parts.join(" — ") : "(no element details)";
}

export function RecordingReviewScreen({
  summary,
  onDone,
}: RecordingReviewScreenProps) {
  const [items, setItems] = useState<ReviewItem[]>(() =>
    summary.actions.map((action, i) => ({ originalIndex: i, action })),
  );
  const [nameHint, setNameHint] = useState("");
  const [isSaving, setIsSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savedInfo, setSavedInfo] = useState<StoredPlaybookInfo | null>(null);
  // §4.2's answer. Starts false: a template makes a recording something that
  // writes repeatedly, and §4.10 is explicit that not confirming leaves "an
  // ordinary one-shot playbook, unaffected" — which is what doing nothing here
  // produces.
  const [confirmTemplate, setConfirmTemplate] = useState(false);
  const queryClient = useQueryClient();

  function moveUp(index: number) {
    if (index === 0) return;
    setItems((prev) => {
      const next = [...prev];
      [next[index - 1], next[index]] = [next[index], next[index - 1]];
      return next;
    });
  }

  function moveDown(index: number) {
    setItems((prev) => {
      if (index >= prev.length - 1) return prev;
      const next = [...prev];
      [next[index], next[index + 1]] = [next[index + 1], next[index]];
      return next;
    });
  }

  function removeAt(index: number) {
    setItems((prev) => prev.filter((_, i) => i !== index));
  }

  async function handleSave() {
    if (items.length === 0) {
      setSaveError("Nothing left to save — every step was deleted.");
      return;
    }
    setIsSaving(true);
    setSaveError(null);
    try {
      const stepIndices = items.map((item) => item.originalIndex);
      const trimmedHint = nameHint.trim();
      const info = await invoke<StoredPlaybookInfo>(
        "compile_and_store_playbook",
        {
          nameHint: trimmedHint.length > 0 ? trimmedHint : null,
          stepIndices,
          // Only ever true when a pattern was actually proposed. Sending true
          // for a recording with no template would make the backend re-run
          // detection and refuse, which is a confusing way to say "there was
          // nothing to confirm".
          confirmTemplate: summary.template ? confirmTemplate : false,
        },
      );
      setSavedInfo(info);
      await queryClient.invalidateQueries({ queryKey: ["playbooks"] });
    } catch (e) {
      setSaveError(describeError(e));
    } finally {
      setIsSaving(false);
    }
  }

  if (savedInfo) {
    return (
      <main className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
        <h1 className="text-xl font-semibold tracking-tight">Playbook saved</h1>
        <div className="flex w-full max-w-md flex-col gap-2 rounded-md border p-4">
          <Row label="Label" value={savedInfo.label} />
          <Row label="Step count" value={String(savedInfo.step_count)} />
          <Row
            label="Irreversible steps"
            value={String(savedInfo.irreversible_count)}
            emphasize={savedInfo.irreversible_count > 0}
          />
          <Row
            label="Redacted steps"
            value={String(savedInfo.redacted_count)}
          />
          <Row
            label="Label source"
            value={savedInfo.label_generated ? "auto-generated" : "your name hint"}
          />
          <Row label="Playbook ID" value={savedInfo.playbook_id} mono />
        </div>
        <Button onClick={onDone}>Done</Button>
      </main>
    );
  }

  return (
    <main className="flex min-h-svh flex-col items-center gap-4 p-8">
      <div className="flex w-full max-w-2xl flex-col items-center gap-1">
        <h1 className="text-xl font-semibold tracking-tight">
          Review recording
        </h1>
        <p className="text-muted-foreground text-center text-sm">
          Session <span className="font-mono">{summary.session_name}</span> —{" "}
          {summary.action_count} captured, {summary.excluded_count} excluded,{" "}
          {summary.unmapped_events} unmapped raw events.
        </p>
      </div>

      {/* §4.12: the detected mapping is a new section at the TOP of the review
          screen, alongside the captured steps — not a separate flow. */}
      <TemplateProposalSection
        proposal={summary.template}
        noTemplateReason={summary.no_template_reason}
        confirmed={confirmTemplate}
        onConfirmedChange={setConfirmTemplate}
        disabled={isSaving}
      />

      <div className="w-full max-w-2xl flex-1 overflow-y-auto rounded-md border">
        {items.length === 0 ? (
          <p className="text-muted-foreground p-6 text-center text-sm">
            No steps left — every captured action was deleted.
          </p>
        ) : (
          <ul className="divide-y">
            {items.map((item, index) => (
              <li
                key={`${item.originalIndex}-${index}`}
                className="flex items-center gap-3 p-3"
              >
                <span className="text-muted-foreground w-6 shrink-0 text-right text-xs tabular-nums">
                  {index + 1}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="rounded bg-secondary px-1.5 py-0.5 text-xs font-medium">
                      {item.action.action_type}
                    </span>
                    <span className="truncate text-sm">
                      {describeAction(item.action)}
                    </span>
                  </div>
                  <div className="text-muted-foreground mt-0.5 flex items-center gap-2 text-xs">
                    <span className="truncate">{item.action.source_app}</span>
                    {item.action.would_redact ? (
                      <span className="text-destructive font-medium">
                        [payload redacted]
                      </span>
                    ) : item.action.payload_preview ? (
                      <span className="truncate font-mono">
                        {item.action.payload_preview}
                      </span>
                    ) : null}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  <Button
                    variant="outline"
                    size="icon-sm"
                    disabled={index === 0}
                    onClick={() => moveUp(index)}
                    aria-label="Move up"
                  >
                    ↑
                  </Button>
                  <Button
                    variant="outline"
                    size="icon-sm"
                    disabled={index === items.length - 1}
                    onClick={() => moveDown(index)}
                    aria-label="Move down"
                  >
                    ↓
                  </Button>
                  <Button
                    variant="destructive"
                    size="icon-sm"
                    onClick={() => removeAt(index)}
                    aria-label="Delete step"
                  >
                    ×
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="flex w-full max-w-2xl flex-col gap-3">
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-muted-foreground">
            Playbook name (optional — leave blank to auto-label from the
            local model)
          </span>
          <input
            type="text"
            value={nameHint}
            onChange={(e) => setNameHint(e.target.value)}
            placeholder="e.g. Log into portal and submit form"
            className="border-input h-9 rounded-md border bg-transparent px-3 text-sm outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
          />
        </label>

        {saveError ? (
          <p className="text-destructive text-sm">{saveError}</p>
        ) : null}

        <div className="flex justify-end gap-2">
          <Button variant="outline" onClick={onDone} disabled={isSaving}>
            Discard (don&apos;t save)
          </Button>
          <Button onClick={() => void handleSave()} disabled={isSaving}>
            {isSaving ? "Saving..." : "Save playbook"}
          </Button>
        </div>
      </div>
    </main>
  );
}

function Row({
  label,
  value,
  emphasize,
  mono,
}: {
  label: string;
  value: string;
  emphasize?: boolean;
  mono?: boolean;
}) {
  return (
    <div className="flex items-center justify-between gap-3 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span
        className={cn(
          "text-right",
          mono && "font-mono text-xs",
          emphasize && "text-destructive font-semibold",
        )}
      >
        {value}
      </span>
    </div>
  );
}
