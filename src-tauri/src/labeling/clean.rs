//! Clean: turn a captured action sequence into the compact pattern description
//! the labeling prompt expects.
//!
//! Redaction happens HERE, while the description is being built, so a redacted
//! payload never exists in the prompt string at any point. `clean()` is the only
//! way to produce a `CleanedPattern`, and it always applies the policy -- there
//! is no path that builds a description from raw payloads.

use crate::capture::{ActionKind, CapturedAction};

use super::redact::{RedactionPolicy, RedactionRecord, REDACTED};

/// Cap on described steps. A long session would otherwise produce a prompt
/// longer than the few-shot examples, which pushes the model off-pattern.
const MAX_DESCRIBED_STEPS: usize = 8;

/// A description that is guaranteed to have passed the redaction policy.
#[derive(Debug, Clone)]
pub struct CleanedPattern {
    /// The string that goes into the prompt.
    pub description: String,
    pub steps_described: usize,
    pub steps_total: usize,
    pub truncated: bool,
    pub redactions: Vec<RedactionRecord>,
}

impl CleanedPattern {
    pub fn redacted_count(&self) -> usize {
        self.redactions.len()
    }
}

/// Build the pattern description, redacting as it goes.
///
/// A single Record Mode session is one observation, so it describes as
/// "repeated 1 time" -- the degenerate case the Step 4a probe already used.
/// Repetition counting across sessions is Ghost Mode's job, not Step 4b's.
pub fn clean(actions: &[CapturedAction], policy: &RedactionPolicy) -> CleanedPattern {
    let mut phrases = Vec::new();
    let mut redactions = Vec::new();

    for (i, action) in actions.iter().take(MAX_DESCRIBED_STEPS).enumerate() {
        let phrase = match action.kind {
            ActionKind::Click => {
                format!("clicks {}", target_of(action))
            }

            ActionKind::Type => {
                // Redaction decided BEFORE the payload is formatted into the
                // phrase. The raw payload is never written to `phrases`.
                let text = match policy.evaluate(action) {
                    Some(reason) => {
                        redactions.push(RedactionRecord {
                            step_index: i,
                            element_role: action.element_role.clone(),
                            element_name: action.element_name.clone(),
                            reason,
                            withheld_len: action.payload.as_ref().map_or(0, |p| p.len()),
                        });
                        REDACTED.to_string()
                    }
                    None => match &action.payload {
                        Some(p) => summarise_payload(p),
                        None => REDACTED.to_string(),
                    },
                };
                format!("types {} into {}", text, target_of(action))
            }

            ActionKind::Navigate => {
                format!("switches to {}", target_of(action))
            }

            ActionKind::Read => {
                format!("reads {}", target_of(action))
            }
        };
        phrases.push(phrase);
    }

    let description = if phrases.is_empty() {
        "User performs no recorded actions, repeated 1 time.".to_string()
    } else {
        format!("User {}, repeated 1 time.", phrases.join(" then "))
    };

    CleanedPattern {
        description,
        steps_described: phrases.len(),
        steps_total: actions.len(),
        truncated: actions.len() > MAX_DESCRIBED_STEPS,
        redactions,
    }
}

/// A short, prompt-safe name for what an action acted on.
fn target_of(action: &CapturedAction) -> String {
    let name = action
        .element_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match name {
        Some(n) => trim_to(n, 40),
        None => action
            .element_role
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|r| format!("a {}", r.to_lowercase()))
            .unwrap_or_else(|| "an element".to_string()),
    }
}

/// Non-secret payloads still get shortened: the description is a summary, and a
/// wall of typed text would swamp the few-shot pattern.
fn summarise_payload(payload: &str) -> String {
    let flat = payload.replace(['\r', '\n'], " ");
    let flat = flat.trim();
    if flat.is_empty() {
        return "nothing".to_string();
    }
    format!("\"{}\"", trim_to(flat, 40))
}

fn trim_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{ActionCandidate, CapturedStream, ExclusionList};

    fn stream_with(items: Vec<(ActionKind, &str, &str, Option<&str>)>) -> Vec<CapturedAction> {
        let mut stream = CapturedStream::new(ExclusionList::from_patterns(["!never-matches!"]));
        for (kind, role, name, payload) in items {
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
        }
        stream.actions().to_vec()
    }

    #[test]
    fn builds_the_expected_shape() {
        let actions = stream_with(vec![
            (ActionKind::Click, "Button", "Add New Tab", None),
            (ActionKind::Type, "Document", "Text editor", Some("hello")),
        ]);
        let cleaned = clean(&actions, &RedactionPolicy::placeholder());

        assert_eq!(
            cleaned.description,
            "User clicks Add New Tab then types \"hello\" into Text editor, repeated 1 time."
        );
        assert_eq!(cleaned.redacted_count(), 0);
    }

    #[test]
    fn secret_payload_never_reaches_the_description() {
        let actions = stream_with(vec![
            (ActionKind::Click, "Button", "Sign in", None),
            (ActionKind::Type, "Edit", "Password", Some("hunter2-secret")),
        ]);
        let cleaned = clean(&actions, &RedactionPolicy::placeholder());

        assert!(
            !cleaned.description.contains("hunter2-secret"),
            "payload leaked into the description: {}",
            cleaned.description
        );
        assert!(cleaned.description.contains(REDACTED));
        assert_eq!(cleaned.redacted_count(), 1);
    }

    #[test]
    fn redaction_record_does_not_carry_the_payload() {
        let actions = stream_with(vec![(
            ActionKind::Type,
            "Edit",
            "Password",
            Some("hunter2-secret"),
        )]);
        let cleaned = clean(&actions, &RedactionPolicy::placeholder());

        let dumped = format!("{:?}", cleaned.redactions);
        assert!(!dumped.contains("hunter2-secret"), "{dumped}");
        assert_eq!(cleaned.redactions[0].withheld_len, "hunter2-secret".len());
    }

    #[test]
    fn long_sessions_are_capped() {
        let items: Vec<_> = (0..20)
            .map(|_| (ActionKind::Click, "Button", "Thing", None))
            .collect();
        let cleaned = clean(&stream_with(items), &RedactionPolicy::placeholder());

        assert_eq!(cleaned.steps_described, MAX_DESCRIBED_STEPS);
        assert_eq!(cleaned.steps_total, 20);
        assert!(cleaned.truncated);
    }

    #[test]
    fn empty_session_still_produces_a_valid_description() {
        let cleaned = clean(&[], &RedactionPolicy::placeholder());
        assert!(cleaned.description.ends_with("repeated 1 time."));
    }
}
