//! Validate a compiled playbook before anything is written.
//!
//! The schema already enforces most of this with CHECK constraints and a
//! UNIQUE(playbook_id, step_order). Validating first is not redundant: a
//! constraint violation arrives as a generic SQLite error partway through a
//! transaction, while this returns every problem at once, in terms of the
//! playbook rather than of a row.

use super::{CompiledPlaybook, ControlRole};

/// Values the schema's `action_type` CHECK accepts.
const VALID_ACTION_TYPES: &[&str] = &["click", "type", "navigate", "read"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyPlaybook,
    EmptyName,
    StepOrderNotDense {
        expected: i64,
        found: i64,
        step_index: usize,
    },
    InvalidActionType {
        step_order: i64,
        found: String,
    },
    InvalidPayloadJson {
        step_order: i64,
        detail: String,
    },
    DuplicateStepId {
        id: String,
    },
    RedactedStepLeaksPayload {
        step_order: i64,
    },
}

impl ValidationError {
    pub fn describe(&self) -> String {
        match self {
            ValidationError::EmptyPlaybook => "playbook has no steps".to_string(),
            ValidationError::EmptyName => "playbook name is empty".to_string(),
            ValidationError::StepOrderNotDense {
                expected,
                found,
                step_index,
            } => format!(
                "step_order must be dense and start at 1: step {step_index} has {found}, expected {expected}"
            ),
            ValidationError::InvalidActionType { step_order, found } => format!(
                "step {step_order} has action_type {found:?}, not one of {VALID_ACTION_TYPES:?}"
            ),
            ValidationError::InvalidPayloadJson { step_order, detail } => {
                format!("step {step_order} has invalid action_payload_json: {detail}")
            }
            ValidationError::DuplicateStepId { id } => {
                format!("two steps share id {id:?}")
            }
            ValidationError::RedactedStepLeaksPayload { step_order } => format!(
                "step {step_order} is marked redacted but its payload JSON still carries text"
            ),
        }
    }
}

/// Every problem found, or an empty vec when the playbook is storable.
pub fn validate(playbook: &CompiledPlaybook) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if playbook.steps.is_empty() {
        errors.push(ValidationError::EmptyPlaybook);
    }
    if playbook.name.trim().is_empty() {
        errors.push(ValidationError::EmptyName);
    }

    let mut seen_ids = std::collections::HashSet::new();

    for (i, step) in playbook.steps.iter().enumerate() {
        let expected = (i + 1) as i64;
        if step.step_order != expected {
            errors.push(ValidationError::StepOrderNotDense {
                expected,
                found: step.step_order,
                step_index: i,
            });
        }

        if !VALID_ACTION_TYPES.contains(&step.action_type.as_str()) {
            errors.push(ValidationError::InvalidActionType {
                step_order: step.step_order,
                found: step.action_type.clone(),
            });
        }

        if !seen_ids.insert(step.id.clone()) {
            errors.push(ValidationError::DuplicateStepId {
                id: step.id.clone(),
            });
        }

        match serde_json::from_str::<serde_json::Value>(&step.action_payload_json) {
            Ok(value) => {
                // A step whose payload was redacted must not also carry the
                // text. Cheap to check, and the one failure mode that would
                // silently undo Step 4b's redaction discipline.
                if step.payload_redacted {
                    let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    if text != crate::labeling::REDACTED {
                        errors.push(ValidationError::RedactedStepLeaksPayload {
                            step_order: step.step_order,
                        });
                    }
                }
            }
            Err(e) => errors.push(ValidationError::InvalidPayloadJson {
                step_order: step.step_order,
                detail: e.to_string(),
            }),
        }

        // control_role is an enum in Rust, so it cannot be invalid; this
        // assertion documents that the mapping is total rather than checking it.
        let _: &'static str = match step.control_role {
            ControlRole::Button
            | ControlRole::Textbox
            | ControlRole::Dropdown
            | ControlRole::Checkbox
            | ControlRole::Radio
            | ControlRole::Link
            | ControlRole::Other => step.control_role.as_str(),
        };
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{ActionCandidate, ActionKind, CapturedAction, CapturedStream, ExclusionList};
    use crate::compile::{compile, ReversibilityPolicy};
    use crate::labeling::RedactionPolicy;

    fn one_action() -> Vec<CapturedAction> {
        let mut s = CapturedStream::new(ExclusionList::from_patterns(["!never!"]));
        s.admit(ActionCandidate {
            kind: ActionKind::Click,
            identifiers: vec!["app.exe".into()],
            element_role: Some("Button".into()),
            element_name: Some("Go".into()),
            payload: None,
            detail: None,
            timestamp_ms: 0,
        });
        s.actions().to_vec()
    }

    fn compiled(name: &str) -> CompiledPlaybook {
        compile(
            &one_action(),
            name,
            &ReversibilityPolicy::placeholder(),
            &RedactionPolicy::placeholder(),
        )
    }

    #[test]
    fn a_normal_playbook_validates() {
        assert!(validate(&compiled("Good Playbook")).is_empty());
    }

    #[test]
    fn empty_playbook_is_rejected() {
        let mut pb = compiled("Name");
        pb.steps.clear();
        assert!(validate(&pb).contains(&ValidationError::EmptyPlaybook));
    }

    #[test]
    fn empty_name_is_rejected() {
        assert!(validate(&compiled("   ")).contains(&ValidationError::EmptyName));
    }

    #[test]
    fn sparse_step_order_is_rejected() {
        let mut pb = compiled("Name");
        pb.steps[0].step_order = 7;
        assert!(validate(&pb).iter().any(|e| matches!(
            e,
            ValidationError::StepOrderNotDense { found: 7, .. }
        )));
    }

    #[test]
    fn malformed_payload_json_is_rejected() {
        let mut pb = compiled("Name");
        pb.steps[0].action_payload_json = "{not json".to_string();
        assert!(validate(&pb)
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidPayloadJson { .. })));
    }

    #[test]
    fn a_redacted_step_that_still_carries_text_is_rejected() {
        let mut pb = compiled("Name");
        pb.steps[0].payload_redacted = true;
        pb.steps[0].action_payload_json =
            serde_json::json!({"text": "hunter2-secret"}).to_string();
        assert!(validate(&pb)
            .iter()
            .any(|e| matches!(e, ValidationError::RedactedStepLeaksPayload { .. })));
    }
}
