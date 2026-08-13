//! §4.1's sensibility check: ask the local model whether a detected mapping
//! actually makes sense.
//!
//! Backend item 4. Detection ([`super::detect`]) establishes that a mapping is
//! *consistent* -- the same fields, advancing together, three times over. That
//! is a structural fact and says nothing about whether the mapping is
//! meaningful. A recording could consistently copy a column of phone numbers
//! into a column headed "Order Total", and the Rule of 3 would confirm it
//! happily.
//!
//! So the model is given a second job, per §4.1: "confirming a source column
//! that looks like customer names is landing in a destination column that makes
//! sense for that data -- the same model already used for labeling".
//!
//! ## Confidence comes from decoding, not from the model's opinion of itself
//!
//! §4.1 makes the fallback conditional on confidence: low confidence means ask
//! the user to re-record with cleaner examples, rather than proceeding on a
//! shaky mapping. Asking a 0.5B model to state its own confidence is not a
//! reliable way to get that number.
//!
//! `mean_token_probability` already exists on the inference path and is the
//! honest version: the average probability the model assigned to the tokens it
//! actually chose. A confident yes and a hedged yes are distinguishable in it
//! even when the text is identical.
//!
//! ## The threshold is deliberately not settled here
//!
//! §4.1 is explicit that the level separating "proceed" from "ask to re-record"
//! "needs to be tunable and tested against real behavior... the same way other
//! thresholds in this product are treated as real values to calibrate from use,
//! not numbers to guess at now." [`CONFIDENCE_FLOOR`] is therefore a starting
//! point with its measurement recorded beside it, not a settled constant.

use super::{FieldMapping, Pattern};
use crate::labeling::{Completion, LabelingEngine, LabelingError};

/// Below this mean token probability, a verdict is treated as unsure.
///
/// ## Measured, and it does not currently discriminate
///
/// §4.1 expects confidence to separate "proceed" from "ask to re-record". Across
/// six real mappings on qwen2.5-0.5b-instruct-q4_k_m -- three sensible, three
/// nonsense, run twice -- the observed range was **0.861 to 0.898**. The model
/// is uniformly confident whether it is right or wrong, including on the one
/// case it got wrong (0.87 for a false negative).
///
/// So this floor never fires in practice, and the design's confidence-based
/// fallback is inert as things stand. What actually protects the user is the
/// VERDICT: the model said "no" to all three nonsensical mappings, and
/// [`Verdict::should_rerecord`] routes both `NotSensible` and `Unsure` to the
/// same §4.1 remedy. The re-record path is reached -- just not by the route the
/// design anticipated.
///
/// Kept low and kept named rather than deleted, for two reasons. It still
/// catches a degenerate collapse -- a model returning near-random tokens would
/// fall under it -- and §4.1 requires the threshold to remain tunable, which a
/// literal buried in a comparison would not be. Raising it to sit inside the
/// observed band would start refusing correct answers arbitrarily, since right
/// and wrong answers are not separated along this axis.
pub const CONFIDENCE_FLOOR: f64 = 0.60;

/// What the model made of a mapping.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Confidently sensible. Proceed to the first-record preview (§4.3).
    Sensible { confidence: f64 },
    /// Confidently NOT sensible -- the mapping is wrong, not merely uncertain.
    NotSensible { confidence: f64 },
    /// Either the model hedged, or its answer was unreadable. §4.1's fallback:
    /// ask the user to re-record with cleaner examples.
    Unsure { confidence: f64, reason: String },
}

impl Verdict {
    /// Whether §4.1's re-record fallback applies.
    ///
    /// True for `NotSensible` as well as `Unsure`: a mapping the model is
    /// confident is wrong is not something to proceed with either, and the
    /// design offers exactly one remedy for "do not proceed".
    pub fn should_rerecord(&self) -> bool {
        !matches!(self, Verdict::Sensible { .. })
    }

    pub fn confidence(&self) -> f64 {
        match self {
            Verdict::Sensible { confidence }
            | Verdict::NotSensible { confidence }
            | Verdict::Unsure { confidence, .. } => *confidence,
        }
    }
}

/// Render a mapping the way the prompt describes one.
///
/// Labels, not locators: "Customer Name -> Client" is a question the model can
/// answer, "C -> B" is not. A mapping whose fields have no labels cannot be
/// checked for sensibility at all, which is why [`verify`] requires them.
pub fn describe(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(from, to)| format!("{from} -> {to}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The prompt shape, mirroring `labeling::build_prompt`: worked examples, then
/// the question.
///
/// Four examples, two of them negative. With only positive examples a small
/// model learns to answer "yes" regardless -- the failure this check exists to
/// catch would then be invisible, which is worse than not checking.
///
/// ## A fifth example was tried and removed
///
/// The model judges identical column names -- `"Invoice Number -> Invoice
/// Number, Due Date -> Due Date"` -- as NOT sensible, at 0.87 confidence.
/// Copying a column into one of the same name is about the most ordinary
/// mapping there is, so this is a real false negative, not a curiosity.
///
/// A worked positive example of exactly that shape was added to teach it, and
/// re-measured: **the answer did not change** (0.884, still "no"). A 0.5B model
/// did not generalise from one demonstration. The example was removed rather
/// than left in, because a prompt line that looks like it handles a case and
/// measurably does not is worse than an acknowledged gap.
///
/// The cost of the false negative is an unnecessary re-record, never a wrong
/// mapping accepted -- so it degrades the feature's convenience, not its
/// safety. Fixing it likely needs a larger model or a differently-shaped
/// question, and is worth its own attempt rather than another prompt guess.
pub fn build_prompt(mapping: &str) -> String {
    format!(
        "Decide if a spreadsheet column mapping is sensible.\n\
         Mapping: \"Customer Name -> Client, Order Total -> Amount\"\n\
         Output: {{\"sensible\": \"yes\"}}\n\
         Mapping: \"Phone Number -> Order Total, Email -> Ship Date\"\n\
         Output: {{\"sensible\": \"no\"}}\n\
         Mapping: \"Product SKU -> Item Code, Quantity -> Qty\"\n\
         Output: {{\"sensible\": \"yes\"}}\n\
         Mapping: \"Delivery Address -> Invoice Number, Customer Name -> Tax Rate\"\n\
         Output: {{\"sensible\": \"no\"}}\n\
         Mapping: \"{mapping}\"\n\
         Output:"
    )
}

/// Turn a raw completion into a verdict.
///
/// Pure, so the whole decision table is testable without loading a model --
/// including the cases that matter most, which are the ones where the model
/// says something unexpected.
pub fn interpret(output: &str, confidence: f64) -> Verdict {
    let said = output.to_ascii_lowercase();
    // Look for the answer rather than requiring well-formed JSON. The model is
    // small and its punctuation is not dependable; what it committed to is.
    let yes = said.contains("\"yes\"") || said.contains(": yes") || said.contains("sensible\": y");
    let no = said.contains("\"no\"") || said.contains(": no") || said.contains("sensible\": n");

    if yes == no {
        // Both or neither: the answer is not readable, whatever the confidence.
        return Verdict::Unsure {
            confidence,
            reason: format!("could not read a yes or no from {output:?}"),
        };
    }
    if confidence < CONFIDENCE_FLOOR {
        return Verdict::Unsure {
            confidence,
            reason: format!(
                "model said {} but only at {confidence:.2} mean token probability, below the \
                 {CONFIDENCE_FLOOR:.2} floor",
                if yes { "yes" } else { "no" }
            ),
        };
    }
    if yes {
        Verdict::Sensible { confidence }
    } else {
        Verdict::NotSensible { confidence }
    }
}

/// Ask the model whether a detected mapping is sensible.
///
/// `labels` supplies a human-readable name for each field locator -- typically
/// from [`crate::source::SourceShape`], read from the source's header row. A
/// locator with no label is left out, and a mapping with nothing left is
/// [`Verdict::Unsure`] rather than a guess: "C -> B" carries no meaning to
/// judge.
pub fn verify(
    engine: &LabelingEngine,
    pattern: &Pattern,
    labels: &dyn Fn(&str) -> Option<String>,
) -> Result<(Verdict, Completion), LabelingError> {
    let described: Vec<(String, String)> = pattern
        .fields
        .iter()
        .filter_map(|FieldMapping { source_field, destination_field }| {
            Some((labels(source_field)?, labels(destination_field)?))
        })
        .collect();

    if described.is_empty() {
        return Ok((
            Verdict::Unsure {
                confidence: 0.0,
                reason: "no field in the mapping has a label to judge -- bare column letters \
                         carry no meaning"
                    .to_string(),
            },
            Completion {
                output: String::new(),
                tokens_generated: 0,
                stopped_by: None,
                inference_time: std::time::Duration::ZERO,
                mean_token_probability: 0.0,
            },
        ));
    }

    let completion = engine.complete(&build_prompt(&describe(&described)))?;
    let verdict = interpret(&completion.output, completion.mean_token_probability);
    Ok((verdict, completion))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_confident_yes_proceeds() {
        let v = interpret("{\"sensible\": \"yes\"}", 0.91);
        assert_eq!(v, Verdict::Sensible { confidence: 0.91 });
        assert!(!v.should_rerecord());
    }

    #[test]
    fn a_confident_no_does_not_proceed() {
        let v = interpret("{\"sensible\": \"no\"}", 0.88);
        assert_eq!(v, Verdict::NotSensible { confidence: 0.88 });
        assert!(
            v.should_rerecord(),
            "a mapping the model is sure is wrong must not proceed either"
        );
    }

    #[test]
    fn a_hedged_answer_falls_back_to_rerecording() {
        // §4.1's actual rule: the text says yes, but the model was not
        // confident, so proceeding would be building on a shaky mapping.
        let v = interpret("{\"sensible\": \"yes\"}", CONFIDENCE_FLOOR - 0.01);
        assert!(matches!(v, Verdict::Unsure { .. }), "got {v:?}");
        assert!(v.should_rerecord());
    }

    #[test]
    fn the_floor_is_inclusive_so_a_borderline_answer_proceeds() {
        // Pinning the boundary itself, since the threshold is meant to be
        // tuned: moving it should change behaviour predictably.
        let v = interpret("{\"sensible\": \"yes\"}", CONFIDENCE_FLOOR);
        assert_eq!(v, Verdict::Sensible { confidence: CONFIDENCE_FLOOR });
    }

    #[test]
    fn an_unreadable_answer_is_unsure_however_confident() {
        // A small model saying something else entirely, at high confidence, is
        // exactly when a naive parser would invent a verdict.
        for output in ["{\"label\": \"Copy rows?\"}", "maybe", "", "{}"] {
            let v = interpret(output, 0.99);
            assert!(
                matches!(v, Verdict::Unsure { .. }),
                "{output:?} should be unsure, got {v:?}"
            );
        }
    }

    #[test]
    fn saying_both_yes_and_no_is_unsure_not_a_coin_flip() {
        // A realistic ambiguity: the model volunteers a second field carrying
        // the opposite answer. Picking whichever appears first would be a coin
        // flip dressed as a decision.
        //
        // The first draft of this test used a trailing bare `no` after the
        // closing brace, which the parser correctly ignored -- and which
        // inference cannot produce anyway, since `}` is a stop sequence. The
        // test was wrong, not the rule.
        let v = interpret("{\"sensible\": \"yes\", \"confident\": \"no\"}", 0.95);
        assert!(matches!(v, Verdict::Unsure { .. }), "got {v:?}");
    }

    #[test]
    fn the_prompt_teaches_both_answers() {
        // With only positive examples a small model answers "yes" to anything,
        // which would make this check useless while looking like it worked.
        let p = build_prompt("A -> B");
        assert_eq!(p.matches("\"yes\"").count(), 2);
        assert_eq!(p.matches("\"no\"").count(), 2);
        assert!(p.ends_with("Mapping: \"A -> B\"\nOutput:"));
    }

    #[test]
    fn a_mapping_is_described_by_labels_not_letters() {
        assert_eq!(
            describe(&[
                ("Customer Name".into(), "Client".into()),
                ("Order Total".into(), "Amount".into()),
            ]),
            "Customer Name -> Client, Order Total -> Amount"
        );
    }
}
