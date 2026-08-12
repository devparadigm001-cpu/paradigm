//! Payload redaction, applied BEFORE the prompt is built.
//!
//! Same structural principle as the Step 3 exclusion gate: the check runs
//! before the data can reach the next stage, not as a filter afterwards. A
//! redacted payload is never written into the prompt string, so it cannot
//! reach the model even if a later stage is careless.
//!
//! The heuristic here is a Phase 1 placeholder. Real sensitivity
//! classification is a later concern; the position of the check is not.

use crate::capture::{ActionKind, CapturedAction};

/// What replaces a redacted payload in the pattern description.
pub const REDACTED: &str = "[REDACTED]";

/// Case-insensitive substring patterns matched against a typed field's role
/// and name.
#[derive(Debug, Clone)]
pub struct RedactionPolicy {
    field_patterns: Vec<String>,
    /// Roles that are inherently secret regardless of their name.
    secret_roles: Vec<String>,
}

impl RedactionPolicy {
    /// Phase 1 placeholder. Deliberately over-broad: redacting a harmless
    /// field costs a slightly vaguer label, while missing a real one puts a
    /// credential in a prompt.
    pub fn placeholder() -> Self {
        Self {
            field_patterns: [
                "password", "passwd", "passphrase", "pin", "secret", "token", "api key",
                "apikey", "ssn", "social security", "card number", "cvv", "security code",
                "account number", "routing",
            ]
            .iter()
            .map(|s| s.to_lowercase())
            .collect(),
            secret_roles: ["passwordbox", "password"]
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
        }
    }

    pub fn field_patterns(&self) -> &[String] {
        &self.field_patterns
    }

    /// Decide whether an action's payload must be redacted.
    ///
    /// Only `Type` actions carry a payload, so only they can be redacted.
    /// Returns the reason when redaction applies, for the audit record.
    pub fn evaluate(&self, action: &CapturedAction) -> Option<RedactionReason> {
        if action.kind != ActionKind::Type {
            return None;
        }
        if action.payload.is_none() {
            return None;
        }

        if let Some(role) = &action.element_role {
            let lower = role.to_lowercase();
            if self.secret_roles.iter().any(|r| lower.contains(r)) {
                return Some(RedactionReason::SecretRole { role: role.clone() });
            }
        }

        for field in [action.element_name.as_deref(), action.element_role.as_deref()]
            .into_iter()
            .flatten()
        {
            let lower = field.to_lowercase();
            if let Some(pattern) = self.field_patterns.iter().find(|p| lower.contains(*p)) {
                return Some(RedactionReason::FieldPattern {
                    pattern: pattern.clone(),
                    matched_on: field.to_string(),
                });
            }
        }

        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedactionReason {
    SecretRole { role: String },
    FieldPattern { pattern: String, matched_on: String },
}

impl RedactionReason {
    pub fn describe(&self) -> String {
        match self {
            RedactionReason::SecretRole { role } => {
                format!("element role {role:?} is inherently secret")
            }
            RedactionReason::FieldPattern {
                pattern,
                matched_on,
            } => format!("field {matched_on:?} matched pattern {pattern:?}"),
        }
    }
}

/// Audit record of one redaction. Records WHY, never WHAT -- a redaction
/// record must not become the leak it exists to prevent.
#[derive(Debug, Clone)]
pub struct RedactionRecord {
    pub step_index: usize,
    pub element_role: Option<String>,
    pub element_name: Option<String>,
    pub reason: RedactionReason,
    /// Length of the payload that was withheld. Useful for diagnostics and
    /// safe to keep; the content itself is not.
    pub withheld_len: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{ActionCandidate, CapturedStream, ExclusionList};

    /// CapturedAction cannot be constructed outside capture::stream (it holds a
    /// private gate token), so build one the only legitimate way: through the
    /// exclusion gate.
    fn action(kind: ActionKind, role: &str, name: &str, payload: Option<&str>) -> CapturedAction {
        let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
        stream.admit(ActionCandidate {
            kind,
            identifiers: vec!["notepad.exe".into()],
            process_name: None,
            element_role: Some(role.to_string()),
            element_name: Some(name.to_string()),
            payload: payload.map(str::to_string),
            detail: None,
            timestamp_ms: 0,
        });
        stream.actions()[0].clone()
    }

    #[test]
    fn redacts_password_named_field() {
        let policy = RedactionPolicy::placeholder();
        let a = action(ActionKind::Type, "Edit", "Password", Some("hunter2"));
        assert!(policy.evaluate(&a).is_some());
    }

    #[test]
    fn redacts_password_role_regardless_of_name() {
        let policy = RedactionPolicy::placeholder();
        let a = action(ActionKind::Type, "PasswordBox", "field 3", Some("hunter2"));
        assert!(matches!(
            policy.evaluate(&a),
            Some(RedactionReason::SecretRole { .. })
        ));
    }

    #[test]
    fn leaves_ordinary_typing_alone() {
        let policy = RedactionPolicy::placeholder();
        let a = action(ActionKind::Type, "Document", "Text editor", Some("hello"));
        assert!(policy.evaluate(&a).is_none());
    }

    #[test]
    fn clicks_are_never_redacted_they_carry_no_payload() {
        let policy = RedactionPolicy::placeholder();
        let a = action(ActionKind::Click, "Button", "Password", None);
        assert!(policy.evaluate(&a).is_none());
    }
}
