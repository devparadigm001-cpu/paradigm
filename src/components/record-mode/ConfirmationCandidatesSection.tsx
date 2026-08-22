import type { FieldCandidateView } from "./types";

/**
 * What the filter decided is worth asking about, from `detect::candidates`.
 *
 * ## Read-only, deliberately, for now
 *
 * The design in `docs/planning/Filtered-Post-Hoc-Confirmation.md` has the user
 * tick these to confirm which repeated fields were meaningful. That flow has no
 * backend yet — there is no command to send the answers to — so this section
 * shows what WOULD be asked and does not pretend to collect an answer.
 *
 * A checkbox that goes nowhere would be worse than no checkbox: it would look
 * like the user had decided something. The section says plainly that acting on
 * these is not wired up yet.
 *
 * ## Why an empty list renders nothing
 *
 * Most recordings contain no field touched across three records, and that is
 * the ordinary case rather than a failure. Rendering "no candidates found"
 * would turn the normal outcome into a report of something having gone wrong —
 * the same reason `TemplateProposalSection` stays silent without a proposal.
 */
export function ConfirmationCandidatesSection({
  candidates,
}: {
  candidates: FieldCandidateView[];
}) {
  if (candidates.length === 0) return null;

  return (
    <section className="w-full max-w-2xl rounded-md border p-4">
      <h2 className="text-sm font-semibold">
        Repeated fields found in this recording
      </h2>
      <p className="text-muted-foreground mt-1 text-xs">
        Each of these was touched in at least three records. Repetition is why
        they are listed — it is not a claim that any of them matter.
      </p>

      <ul className="mt-3 divide-y">
        {candidates.map((c) => (
          <li key={c.id} className="flex items-baseline gap-3 py-2">
            <span className="text-muted-foreground shrink-0 font-mono text-xs">
              {c.id}
            </span>
            <div className="min-w-0 flex-1">
              <p className="text-sm">{c.detail}</p>
              <p className="text-muted-foreground mt-0.5 text-xs">
                {c.distinct_records} record
                {c.distinct_records === 1 ? "" : "s"}, {c.occurrences} action
                {c.occurrences === 1 ? "" : "s"} — step
                {c.step_orders.length === 1 ? " " : "s "}
                {c.step_orders.join(", ")}
              </p>
            </div>
          </li>
        ))}
      </ul>

      <p className="text-muted-foreground mt-3 text-xs italic">
        Confirming these is not wired up yet — this list is what the filter
        found, not a question being asked.
      </p>
    </section>
  );
}
