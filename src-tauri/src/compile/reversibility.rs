//! Reversibility classification.
//!
//! Third policy struct in the same family as `capture::exclusion` and
//! `labeling::redact`: a keyword list, a Phase 1 placeholder, and a bias
//! towards the safe answer when the signal is weak.
//!
//! ## The bias is towards IRREVERSIBLE
//!
//! The failure-handling design tiers behaviour on reversibility, so an
//! irreversible step is the one that gets confirmation and careful handling.
//! Mislabelling a reversible step as irreversible costs a needless prompt.
//! Mislabelling an irreversible step as reversible means silently clicking
//! "Delete" or "Pay". Those are not symmetric, so ambiguity resolves to
//! irreversible.
//!
//! ## Why `action_type` alone is useless here
//!
//! A click on "Send" and a click on "Cancel" are both `click`. The signal lives
//! in the target's name, not in the kind of interaction.

use crate::capture::{ActionKind, CapturedAction};
use crate::labeling::RedactionPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reversibility {
    Reversible,
    Irreversible,
}

impl Reversibility {
    pub fn is_reversible(self) -> bool {
        self == Reversibility::Reversible
    }
}

/// Why a step was classified as it was. Kept for the audit trail and so the
/// probe can show its working rather than asserting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReversibilityReason {
    /// `read` cannot have side effects.
    ReadIsAlwaysSafe,
    /// Target name or role matched an irreversible keyword.
    KeywordMatch { keyword: String, matched_on: String },
    /// Typing into a field the redaction policy considers sensitive.
    SensitiveField,
    /// No usable identifying information, so we cannot assess it.
    UnidentifiableTarget,
    /// Switching or opening an application has no persistent side effect.
    NavigationIsSafe,
    /// Nothing matched and the target was identifiable.
    NoIrreversibleSignal,
}

impl ReversibilityReason {
    pub fn describe(&self) -> String {
        match self {
            ReversibilityReason::ReadIsAlwaysSafe => {
                "read actions have no side effects".to_string()
            }
            ReversibilityReason::KeywordMatch {
                keyword,
                matched_on,
            } => format!("{matched_on:?} matched irreversible keyword {keyword:?}"),
            ReversibilityReason::SensitiveField => {
                "types into a field classified sensitive by the redaction policy".to_string()
            }
            ReversibilityReason::UnidentifiableTarget => {
                "target could not be identified, so assumed unsafe".to_string()
            }
            ReversibilityReason::NavigationIsSafe => {
                "switching or opening an app has no persistent effect".to_string()
            }
            ReversibilityReason::NoIrreversibleSignal => {
                "no irreversible signal in the target name or role".to_string()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReversibilityPolicy {
    keywords: Vec<String>,
    /// Wired in for Phase 1: typing into a sensitive field is treated as an
    /// irreversibility signal, on the grounds that credential entry is usually
    /// a step towards an irreversible submit. Reuses the redaction policy so
    /// "sensitive field" has exactly one definition in the codebase.
    redaction: RedactionPolicy,
}

impl Default for ReversibilityPolicy {
    fn default() -> Self {
        Self::placeholder()
    }
}

impl ReversibilityPolicy {
    /// Phase 1 placeholder. Deliberately over-broad, for the same reason the
    /// exclusion list is: the cost of a false positive is a prompt, the cost of
    /// a false negative is an irreversible action taken silently.
    ///
    /// Note "cancel" is absent. It aborts rather than commits, so treating it
    /// as irreversible would add friction without protecting anything.
    pub fn placeholder() -> Self {
        Self {
            keywords: [
                "send", "submit", "delete", "remove", "purchase", "buy", "confirm", "pay",
                "checkout", "order", "transfer", "publish", "post", "discard", "destroy",
                "erase", "overwrite", "uninstall", "deactivate", "approve", "sign", "share",
                "upload", "reply", "forward", "empty trash", "permanently",
            ]
            .iter()
            .map(|k| k.to_lowercase())
            .collect(),
            redaction: RedactionPolicy::placeholder(),
        }
    }

    pub fn keywords(&self) -> &[String] {
        &self.keywords
    }

    /// Classify one captured action.
    pub fn classify(&self, action: &CapturedAction) -> (Reversibility, ReversibilityReason) {
        // 1. Reading cannot change anything, regardless of target.
        if action.kind == ActionKind::Read {
            return (
                Reversibility::Reversible,
                ReversibilityReason::ReadIsAlwaysSafe,
            );
        }

        // 2. Keyword signal, checked on both name and role, for every kind
        //    including navigate -- rare, but not foreclosed.
        let name = action.element_name.as_deref().unwrap_or("");
        let role = action.element_role.as_deref().unwrap_or("");
        for field in [name, role] {
            if field.trim().is_empty() {
                continue;
            }
            let lower = field.to_lowercase();
            if let Some(kw) = self.keywords.iter().find(|k| lower.contains(*k)) {
                return (
                    Reversibility::Irreversible,
                    ReversibilityReason::KeywordMatch {
                        keyword: kw.clone(),
                        matched_on: field.to_string(),
                    },
                );
            }
        }

        // 3. No identifying information at all -> cannot assess -> assume
        //    unsafe. Same fail-closed shape as the capture exclusion gate.
        if name.trim().is_empty() && role.trim().is_empty() {
            return (
                Reversibility::Irreversible,
                ReversibilityReason::UnidentifiableTarget,
            );
        }

        // 4. Navigation has no persistent side effect.
        if action.kind == ActionKind::Navigate {
            return (
                Reversibility::Reversible,
                ReversibilityReason::NavigationIsSafe,
            );
        }

        // 5. Typing into a sensitive field.
        if action.kind == ActionKind::Type && self.redaction.evaluate(action).is_some() {
            return (
                Reversibility::Irreversible,
                ReversibilityReason::SensitiveField,
            );
        }

        (
            Reversibility::Reversible,
            ReversibilityReason::NoIrreversibleSignal,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{ActionCandidate, CapturedStream, ExclusionList};

    fn action(
        kind: ActionKind,
        role: Option<&str>,
        name: Option<&str>,
        payload: Option<&str>,
    ) -> CapturedAction {
        let mut s = CapturedStream::new(ExclusionList::from_patterns(["!never!"]));
        s.admit(ActionCandidate {
            element_bounds: None,
            window: None,
            kind,
            identifiers: vec!["app.exe".into()],
            process_name: None,
            element_role: role.map(str::to_string),
            element_name: name.map(str::to_string),
            payload: payload.map(str::to_string),
            detail: None,
            timestamp_ms: 0,
        });
        s.actions()[0].clone()
    }

    #[test]
    fn send_button_is_irreversible() {
        let p = ReversibilityPolicy::placeholder();
        let (r, why) = p.classify(&action(ActionKind::Click, Some("Button"), Some("Send"), None));
        assert_eq!(r, Reversibility::Irreversible);
        assert!(matches!(why, ReversibilityReason::KeywordMatch { .. }));
    }

    #[test]
    fn cancel_button_is_reversible() {
        // Same action_type as Send. Only the name distinguishes them, which is
        // the whole reason this policy exists.
        let p = ReversibilityPolicy::placeholder();
        let (r, _) = p.classify(&action(ActionKind::Click, Some("Button"), Some("Cancel"), None));
        assert_eq!(r, Reversibility::Reversible);
    }

    #[test]
    fn read_is_always_reversible_even_on_a_delete_target() {
        let p = ReversibilityPolicy::placeholder();
        let (r, why) = p.classify(&action(ActionKind::Read, Some("Button"), Some("Delete"), None));
        assert_eq!(r, Reversibility::Reversible);
        assert_eq!(why, ReversibilityReason::ReadIsAlwaysSafe);
    }

    #[test]
    fn navigation_is_reversible_by_default() {
        let p = ReversibilityPolicy::placeholder();
        let (r, why) = p.classify(&action(
            ActionKind::Navigate,
            Some("Window"),
            Some("Notepad"),
            None,
        ));
        assert_eq!(r, Reversibility::Reversible);
        assert_eq!(why, ReversibilityReason::NavigationIsSafe);
    }

    #[test]
    fn navigation_to_an_irreversible_target_is_not_foreclosed() {
        let p = ReversibilityPolicy::placeholder();
        let (r, _) = p.classify(&action(
            ActionKind::Navigate,
            Some("Window"),
            Some("Confirm Purchase"),
            None,
        ));
        assert_eq!(r, Reversibility::Irreversible);
    }

    #[test]
    fn unidentifiable_target_assumes_unsafe() {
        let p = ReversibilityPolicy::placeholder();
        let (r, why) = p.classify(&action(ActionKind::Click, None, None, None));
        assert_eq!(r, Reversibility::Irreversible);
        assert_eq!(why, ReversibilityReason::UnidentifiableTarget);
    }

    #[test]
    fn typing_into_a_password_field_is_irreversible() {
        let p = ReversibilityPolicy::placeholder();
        let (r, why) = p.classify(&action(
            ActionKind::Type,
            Some("Edit"),
            Some("Password"),
            Some("secret"),
        ));
        assert_eq!(r, Reversibility::Irreversible);
        assert_eq!(why, ReversibilityReason::SensitiveField);
    }

    #[test]
    fn ordinary_typing_is_reversible() {
        let p = ReversibilityPolicy::placeholder();
        let (r, why) = p.classify(&action(
            ActionKind::Type,
            Some("Document"),
            Some("Text editor"),
            Some("hello"),
        ));
        assert_eq!(r, Reversibility::Reversible);
        assert_eq!(why, ReversibilityReason::NoIrreversibleSignal);
    }
}
