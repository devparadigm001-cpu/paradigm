//! Compile a captured session into a playbook, validate it, and store it.
//!
//! This is Record Mode's actual save path: raw actions in, rows in the
//! encrypted local database out. Nothing is written until validation passes.
//!
//! Redaction discipline carries through from Step 4b: a payload the redaction
//! policy withholds from the model prompt is also withheld from the stored
//! step. A compiled playbook must never be a way to read back a secret that was
//! kept out of the prompt.

pub mod reversibility;
pub mod roles;
pub mod store;
pub mod validate;

use serde_json::json;
use uuid::Uuid;

use crate::capture::{ActionKind, CapturedAction};
use crate::labeling::{RedactionPolicy, REDACTED};

pub use reversibility::{Reversibility, ReversibilityPolicy, ReversibilityReason};
pub use roles::{map_role, ControlRole};
pub use validate::{validate, ValidationError};

/// Source value for playbooks produced by Record Mode.
pub const SOURCE_RECORD_MODE: &str = "record_mode";

/// One compiled `playbook_steps` row, plus the provenance needed to explain it.
#[derive(Debug, Clone)]
pub struct CompiledStep {
    pub id: String,
    pub step_order: i64,
    pub action_type: String,
    pub control_role: ControlRole,
    pub reversible: bool,
    pub action_payload_json: String,

    // -- provenance, not stored in playbook_steps --
    pub raw_role: Option<String>,
    pub target_name: Option<String>,
    pub reversibility_reason: ReversibilityReason,
    pub payload_redacted: bool,
}

/// The learned mapping and advancement rule, for a templated workflow.
///
/// **Structure only.** §3 permits the durable mapping to record "source column
/// C -> destination column E, advance one row each run" and nothing about
/// specific past rows or their content. There is no value here and no row
/// index, and the schema has no column that could hold one.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTemplate {
    pub source_id: String,
    pub destination_id: String,
    pub source_step: i64,
    pub destination_step: i64,
    pub examples: usize,
    pub fields: Vec<crate::detect::FieldMapping>,
}

impl CompiledTemplate {
    /// Build one from what detection established, plus the surfaces it ran
    /// against -- which detection does not carry, because a [`Pattern`] is
    /// deliberately structure with no identity attached.
    ///
    /// [`Pattern`]: crate::detect::Pattern
    pub fn from_pattern(
        pattern: &crate::detect::Pattern,
        source_id: impl Into<String>,
        destination_id: impl Into<String>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            destination_id: destination_id.into(),
            source_step: pattern.source_step,
            destination_step: pattern.destination_step,
            examples: pattern.examples,
            fields: pattern.fields.clone(),
        }
    }
}

/// A playbook ready to be validated and stored.
#[derive(Debug, Clone)]
pub struct CompiledPlaybook {
    pub id: String,
    pub name: String,
    pub source: String,
    pub steps: Vec<CompiledStep>,
    /// The template, when this recording was confirmed as a repeating pattern.
    ///
    /// `None` for every ordinary recording, which is every recording Phase 1
    /// produces. [`compile`] always sets it to `None`; attaching one is a
    /// separate, deliberate act via [`CompiledPlaybook::with_template`], so the
    /// existing compile path is additive-only and cannot acquire a template by
    /// accident.
    pub template: Option<CompiledTemplate>,
    /// A repeating pattern was detected and offered, and the user said no.
    ///
    /// Distinct from `template: None`, which it accompanies rather than
    /// replaces. Both mean "not a repeating workflow", and every code path that
    /// asks that question keeps getting the same answer -- §4.10 wants a
    /// declined proposal to leave "an ordinary one-shot playbook, unaffected".
    /// What this adds is the ability to answer a DIFFERENT question later: was
    /// one ever offered? Without it, "you were asked and said no" and "nothing
    /// was ever found" are the same row.
    pub declined_template: bool,
}

impl CompiledPlaybook {
    /// Attach a template. The literal example steps are untouched -- §5 item 5:
    /// the mapping is stored "alongside the literal example steps captured
    /// during recording -- additive to the existing schema, not a replacement."
    pub fn with_template(mut self, template: CompiledTemplate) -> Self {
        self.template = Some(template);
        self
    }

    /// Record that a pattern was offered and refused.
    ///
    /// Deliberately not the inverse of [`with_template`] and not exclusive with
    /// it at the type level -- the database CHECK is what makes
    /// confirmed-and-declined unrepresentable, in one place, rather than a rule
    /// spread across every builder call.
    pub fn with_declined_template(mut self) -> Self {
        self.declined_template = true;
        self
    }

    pub fn irreversible_count(&self) -> usize {
        self.steps.iter().filter(|s| !s.reversible).count()
    }

    pub fn redacted_count(&self) -> usize {
        self.steps.iter().filter(|s| s.payload_redacted).count()
    }
}

/// Compile captured actions into a playbook named by the Step 4b label.
///
/// `step_order` is 1-based and dense, which `validate` then re-checks rather
/// than trusting -- the schema has a UNIQUE(playbook_id, step_order) that will
/// reject duplicates anyway, but a gap would silently produce a playbook that
/// replays in the wrong shape.
pub fn compile(
    actions: &[CapturedAction],
    label: &str,
    reversibility: &ReversibilityPolicy,
    redaction: &RedactionPolicy,
) -> CompiledPlaybook {
    let steps = actions
        .iter()
        .enumerate()
        .map(|(i, action)| compile_step(i, action, reversibility, redaction))
        .collect();

    CompiledPlaybook {
        id: Uuid::new_v4().to_string(),
        name: label.trim().to_string(),
        source: SOURCE_RECORD_MODE.to_string(),
        steps,
        // Always. Detection runs separately and a template is attached
        // afterwards, so this function behaves exactly as it did before
        // templated workflows existed.
        template: None,
        // Both defaults say the same thing: `compile` produces an ordinary
        // playbook and nothing else. Whether a pattern was offered is a fact
        // about the review that follows, so it is attached afterwards.
        declined_template: false,
    }
}

fn compile_step(
    index: usize,
    action: &CapturedAction,
    reversibility: &ReversibilityPolicy,
    redaction: &RedactionPolicy,
) -> CompiledStep {
    let control_role = action
        .element_role
        .as_deref()
        .map(map_role)
        .unwrap_or(ControlRole::Other);

    let (rev, reason) = reversibility.classify(action);

    // Redaction decided BEFORE the payload is written into the JSON, so a
    // withheld payload is never serialised at any point.
    let must_redact = redaction.evaluate(action).is_some();
    let payload_value = match (&action.payload, must_redact) {
        (Some(_), true) => Some(REDACTED.to_string()),
        (Some(p), false) => Some(p.clone()),
        (None, _) => None,
    };

    let mut payload = json!({
        "target": {
            "name": action.element_name,
            "raw_role": action.element_role,
            // The selector a replay would actually use, in Terminator syntax.
            "selector": selector_for(action),
        },
        "app": action.source_app,
        // The owning executable, kept separate from `app` because `app` is a
        // display string -- for a navigate step it is the window TITLE, which
        // changes as the user works. Replay needs the stable half to build a
        // `process:`-scoped selector, which is what `Locator::all()` demands
        // before it will count candidates.
        //
        // Null for playbooks recorded before this existed. Replay must treat
        // that as "no scoping available" and behave exactly as it did before,
        // never as an error.
        "process": action.process_name,
    });

    if action.kind == ActionKind::Type {
        payload["text"] = json!(payload_value);
        payload["redacted"] = json!(must_redact);
    }
    if let Some(detail) = &action.detail {
        payload["detail"] = json!(detail);
    }
    // Positional identity, stored under `target` because that is what it
    // identifies. A rectangle is a position and never content, so it is clean
    // under §3 for the same reason a cell reference is.
    //
    // Stored rather than merely captured, deliberately. A recording carries two
    // elements that say the same thing more often than it looks -- two records
    // sharing a value -- and `name` cannot tell them apart. Neither can the
    // platform's element id, which is a hash of role + name; see
    // `docs/known-issues/element-id-is-a-hash-of-the-text.md`.
    //
    // Absent for playbooks recorded before this existed, and absent whenever the
    // element would not report bounds. Consumers must treat missing as "no
    // positional identity", never as an error.
    // `[x, y, width, height]` -- width and height, NOT a right and bottom edge.
    // See `ActionCandidate::element_bounds`. A consumer that reads the third
    // value as a right edge gets a number smaller than the left one, which is
    // exactly how the mislabelling was found.
    if let Some((x, y, width, height)) = action.element_bounds {
        payload["target"]["bounds"] = json!([x, y, width, height]);
    }

    CompiledStep {
        id: Uuid::new_v4().to_string(),
        step_order: (index + 1) as i64,
        action_type: action.kind.as_str().to_string(),
        control_role,
        reversible: rev.is_reversible(),
        action_payload_json: payload.to_string(),
        raw_role: action.element_role.clone(),
        target_name: action.element_name.clone(),
        reversibility_reason: reason,
        payload_redacted: must_redact,
    }
}

/// Terminator-syntax selector for the step's target, or `null` when there is
/// nothing to select on.
fn selector_for(action: &CapturedAction) -> Option<String> {
    let role = action.element_role.as_deref().unwrap_or("").trim();
    let name = action.element_name.as_deref().unwrap_or("").trim();

    match (role.is_empty(), name.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("role:{role}")),
        (true, false) => Some(format!("name:{name}")),
        (false, false) => Some(format!("role:{role}|name:{name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{ActionCandidate, CapturedStream, ExclusionList};

    fn actions(items: Vec<(ActionKind, &str, &str, Option<&str>)>) -> Vec<CapturedAction> {
        let mut s = CapturedStream::new(ExclusionList::from_patterns(["!never!"]));
        for (kind, role, name, payload) in items {
            s.admit(ActionCandidate {
            element_bounds: None,
                kind,
                identifiers: vec!["app.exe".into()],
                process_name: None,
                element_role: Some(role.to_string()),
                element_name: Some(name.to_string()),
                payload: payload.map(str::to_string),
                detail: None,
                timestamp_ms: 0,
            });
        }
        s.actions().to_vec()
    }

    fn compile_default(a: &[CapturedAction]) -> CompiledPlaybook {
        compile(
            a,
            "Test Playbook",
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
    }

    #[test]
    fn step_order_is_dense_and_one_based() {
        let pb = compile_default(&actions(vec![
            (ActionKind::Click, "Button", "One", None),
            (ActionKind::Click, "Button", "Two", None),
            (ActionKind::Click, "Button", "Three", None),
        ]));
        let orders: Vec<i64> = pb.steps.iter().map(|s| s.step_order).collect();
        assert_eq!(orders, vec![1, 2, 3]);
    }

    #[test]
    fn roles_are_mapped_to_the_locked_enum() {
        let pb = compile_default(&actions(vec![
            (ActionKind::Click, "button", "Go", None),
            (ActionKind::Type, "Edit", "Notes", Some("hi")),
            (ActionKind::Click, "Slider", "Volume", None),
        ]));
        assert_eq!(pb.steps[0].control_role, ControlRole::Button);
        assert_eq!(pb.steps[1].control_role, ControlRole::Textbox);
        assert_eq!(pb.steps[2].control_role, ControlRole::Other);
    }

    #[test]
    fn secret_payload_is_not_serialised_into_the_step() {
        let pb = compile_default(&actions(vec![(
            ActionKind::Type,
            "Edit",
            "Password",
            Some("hunter2-secret"),
        )]));

        let json = &pb.steps[0].action_payload_json;
        assert!(
            !json.contains("hunter2-secret"),
            "secret leaked into stored step: {json}"
        );
        assert!(json.contains(REDACTED));
        assert!(pb.steps[0].payload_redacted);
    }

    #[test]
    fn ordinary_payload_is_kept() {
        let pb = compile_default(&actions(vec![(
            ActionKind::Type,
            "Document",
            "Text editor",
            Some("hello world"),
        )]));
        assert!(pb.steps[0].action_payload_json.contains("hello world"));
        assert!(!pb.steps[0].payload_redacted);
    }

    /// Positional identity has to survive compilation, or it is captured and
    /// then thrown away -- which is what happened to `detail`, and cost a whole
    /// investigation to work around. See
    /// `docs/known-issues/sheets-cell-edits-are-captured-by-both-watchers.md`.
    #[test]
    fn element_bounds_reach_the_stored_payload() {
        let mut acts = actions(vec![(ActionKind::Click, "Text", "Ceramic Mug Set", None)]);
        acts[0].element_bounds = Some((10.0, 20.0, 110.0, 44.0));
        let pb = compile_default(&acts);

        let payload: serde_json::Value =
            serde_json::from_str(&pb.steps[0].action_payload_json).expect("payload is json");
        assert_eq!(
            payload["target"]["bounds"],
            json!([10.0, 20.0, 110.0, 44.0]),
            "the rectangle identifies WHICH element, where the name cannot"
        );
    }

    /// Two records sharing a value are the case the whole positional identity
    /// exists for: identical names, identical roles, and on Windows an
    /// identical platform id, because the id is a hash of role + name.
    #[test]
    fn two_elements_with_the_same_name_are_still_distinguishable() {
        let mut acts = actions(vec![
            (ActionKind::Click, "Text", "Ceramic Mug Set", None),
            (ActionKind::Click, "Text", "Ceramic Mug Set", None),
        ]);
        acts[0].element_bounds = Some((10.0, 20.0, 110.0, 44.0));
        acts[1].element_bounds = Some((10.0, 320.0, 110.0, 344.0));
        let pb = compile_default(&acts);

        let first: serde_json::Value =
            serde_json::from_str(&pb.steps[0].action_payload_json).expect("json");
        let second: serde_json::Value =
            serde_json::from_str(&pb.steps[1].action_payload_json).expect("json");

        assert_eq!(
            first["target"]["name"], second["target"]["name"],
            "the premise: the names really are identical"
        );
        assert_ne!(
            first["target"]["bounds"], second["target"]["bounds"],
            "and the positions are not"
        );
    }

    /// An element that will not report bounds is ordinary, not an error, and
    /// must not put a null into the payload for a consumer to trip over.
    #[test]
    fn a_step_without_bounds_carries_no_bounds_key() {
        let pb = compile_default(&actions(vec![(
            ActionKind::Click,
            "Button",
            "Save",
            None,
        )]));
        let payload: serde_json::Value =
            serde_json::from_str(&pb.steps[0].action_payload_json).expect("json");
        assert!(
            payload["target"].get("bounds").is_none(),
            "absent, not null"
        );
    }

    #[test]
    fn irreversible_steps_are_flagged() {
        let pb = compile_default(&actions(vec![
            (ActionKind::Click, "Button", "Cancel", None),
            (ActionKind::Click, "Button", "Send", None),
        ]));
        assert!(pb.steps[0].reversible);
        assert!(!pb.steps[1].reversible);
        assert_eq!(pb.irreversible_count(), 1);
    }

    #[test]
    fn payload_json_is_valid_json() {
        let pb = compile_default(&actions(vec![(
            ActionKind::Click,
            "Button",
            "Go",
            None,
        )]));
        let v: serde_json::Value =
            serde_json::from_str(&pb.steps[0].action_payload_json).expect("valid json");
        assert_eq!(v["target"]["selector"], "role:Button|name:Go");
    }
}
