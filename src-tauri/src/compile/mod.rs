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

/// A playbook ready to be validated and stored.
#[derive(Debug, Clone)]
pub struct CompiledPlaybook {
    pub id: String,
    pub name: String,
    pub source: String,
    pub steps: Vec<CompiledStep>,
}

impl CompiledPlaybook {
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
