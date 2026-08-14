import { useState } from "react";
import { Button } from "@/components/ui/button";
import type { TemplateProposal } from "./types";

/**
 * §4.2's post-stop prompt, as a section of the review screen rather than a
 * screen of its own — Section 6: "Extends the existing review screen (4.12),
 * not a new screen."
 *
 * ## Why the mapping is behind "See more"
 *
 * §4.2 asks for the detail to be available, not in the way: the question is
 * "want me to do the rest?", and a user who trusts it should be able to answer
 * without reading a field-by-field table. The expansion is what makes the
 * detail *checkable* by someone who wants to check it, which is §4.12's review.
 *
 * ## Why declining is the default
 *
 * Nothing is pre-selected as "yes". A template turns a one-shot recording into
 * something that will write to a spreadsheet repeatedly, and the design is
 * consistent that this takes a deliberate act: §4.10 says rejecting leaves "an
 * ordinary one-shot playbook, unaffected", which is exactly what happens if the
 * user ignores this section entirely.
 */
export function TemplateProposalSection({
  proposal,
  noTemplateReason,
  confirmed,
  onConfirmedChange,
  disabled,
}: {
  proposal: TemplateProposal | null;
  noTemplateReason: string | null;
  confirmed: boolean;
  onConfirmedChange: (next: boolean) => void;
  disabled?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);

  // An ordinary recording: nothing was copied between grids, so there was
  // never a pattern to look for. Rendering a "no pattern found" notice here
  // would turn the normal case into a report of failure.
  if (!proposal && !noTemplateReason) return null;

  if (!proposal) {
    return (
      <div className="w-full max-w-2xl rounded-md border border-dashed p-3">
        <p className="text-muted-foreground text-sm">
          <span className="font-medium">Not saved as a repeating workflow.</span>{" "}
          {noTemplateReason}
        </p>
      </div>
    );
  }

  const stepLabel = (step: number) =>
    step === 1 ? "one row" : `${step} rows`;

  return (
    <section
      className="w-full max-w-2xl rounded-md border p-4"
      aria-labelledby="template-proposal-heading"
    >
      <h2 id="template-proposal-heading" className="text-sm font-semibold">
        This looks like a repeating pattern — want me to do the rest?
      </h2>
      <p className="text-muted-foreground mt-1 text-sm">
        You copied {proposal.examples} records from{" "}
        <span className="font-mono">{proposal.source}</span> into{" "}
        <span className="font-mono">{proposal.destination}</span>. I can keep
        going through the rest of the source.
      </p>

      <label className="mt-3 flex items-start gap-2 text-sm">
        <input
          type="checkbox"
          checked={confirmed}
          disabled={disabled}
          onChange={(e) => onConfirmedChange(e.target.checked)}
          className="mt-0.5"
        />
        <span>
          Save this as a repeating workflow
          <span className="text-muted-foreground block text-xs">
            You&apos;ll still see the first record before anything is written.
          </span>
        </span>
      </label>

      <Button
        variant="ghost"
        size="sm"
        className="mt-2 px-0"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        {expanded ? "Hide details" : "See more"}
      </Button>

      {expanded ? (
        <div className="mt-2 flex flex-col gap-2 border-t pt-3">
          <table className="w-full text-sm">
            <caption className="text-muted-foreground sr-only">
              The detected field mapping
            </caption>
            <thead>
              <tr className="text-muted-foreground text-left text-xs">
                <th scope="col" className="font-medium">
                  From (source)
                </th>
                <th scope="col" className="font-medium">
                  To (destination)
                </th>
              </tr>
            </thead>
            <tbody>
              {proposal.fields.map((field) => (
                <tr key={`${field.from}-${field.to}`}>
                  <td className="font-mono">{field.from}</td>
                  <td className="font-mono">{field.to}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="text-muted-foreground text-xs">
            Each run moves down {stepLabel(proposal.source_step)} in the source
            and {stepLabel(proposal.destination_step)} in the destination.
          </p>
        </div>
      ) : null}
    </section>
  );
}
