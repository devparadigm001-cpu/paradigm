//! The exclusion gate.
//!
//! Phase 1 ships a hardcoded placeholder list. The real one becomes
//! user-configurable later, but the *position* of this check in the pipeline is
//! not a later concern: it runs before an observed action is admitted to the
//! captured stream, so an excluded application's data is never held in memory
//! at all -- not held-then-filtered, not held-then-redacted.

/// Case-insensitive substring patterns matched against the source application.
///
/// Substring rather than exact match is deliberate: we would rather over-exclude
/// (lose a recording) than under-exclude (capture a password manager). A missed
/// recording is an annoyance; captured credentials are a breach.
#[derive(Debug, Clone)]
pub struct ExclusionList {
    patterns: Vec<String>,
}

impl ExclusionList {
    /// Placeholder for Phase 1. The real list is user-configurable and arrives
    /// with the settings work; these are stand-ins so the gate is exercised.
    pub fn placeholder() -> Self {
        Self::from_patterns([
            "password", "bank", "keepass", "1password", "bitwarden", "lastpass", "vault",
        ])
    }

    pub fn from_patterns<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            patterns: patterns
                .into_iter()
                .map(|p| p.as_ref().to_lowercase())
                .filter(|p| !p.is_empty())
                .collect(),
        }
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// True if any candidate identifier matches an exclusion pattern.
    ///
    /// Takes every identifier we have for the source (process name, window
    /// title, application name) rather than one: a browser tab titled
    /// "Barclays Bank" has process name "chrome.exe", which no sensible process
    /// list would ever match.
    pub fn matches<'a, I>(&self, identifiers: I) -> Option<Match>
    where
        I: IntoIterator<Item = &'a str>,
    {
        for id in identifiers {
            let haystack = id.to_lowercase();
            for pattern in &self.patterns {
                if haystack.contains(pattern) {
                    return Some(Match {
                        pattern: pattern.clone(),
                        matched_on: id.to_string(),
                    });
                }
            }
        }
        None
    }
}

/// Why something was excluded. Records the pattern and what it matched, never
/// the action payload -- an exclusion record must not itself become a leak.
#[derive(Debug, Clone)]
pub struct Match {
    pub pattern: String,
    pub matched_on: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_case_insensitive_substrings() {
        let list = ExclusionList::placeholder();
        assert!(list.matches(["KeePassXC.exe"]).is_some());
        assert!(list.matches(["My Bank - Chrome"]).is_some());
        assert!(list.matches(["notepad.exe"]).is_none());
    }

    #[test]
    fn window_title_catches_what_process_name_cannot() {
        let list = ExclusionList::placeholder();
        // The whole reason matches() takes several identifiers.
        assert!(list.matches(["chrome.exe"]).is_none());
        assert!(list
            .matches(["chrome.exe", "Barclays Bank - Google Chrome"])
            .is_some());
    }

    #[test]
    fn reports_what_triggered_the_exclusion() {
        let list = ExclusionList::from_patterns(["bank"]);
        let m = list.matches(["chrome.exe", "Bank of X"]).unwrap();
        assert_eq!(m.pattern, "bank");
        assert_eq!(m.matched_on, "Bank of X");
    }

    #[test]
    fn empty_patterns_are_dropped_not_treated_as_match_all() {
        // "".contains("") is true, so an empty pattern would exclude everything.
        let list = ExclusionList::from_patterns(["", "bank"]);
        assert_eq!(list.patterns().len(), 1);
        assert!(list.matches(["notepad.exe"]).is_none());
    }
}
