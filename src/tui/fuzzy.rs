/// Simple fuzzy-ish matcher: returns true if every whitespace-separated term
/// in `query` appears as a case-insensitive substring in `haystack`.
///
/// Unicode-aware (uses char-based lowercase). This is not true fuzzy matching,
/// but it handles typical search cases well and has zero dependencies.
pub fn matches(query: &str, haystack: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let hay_lower: String = haystack.chars().flat_map(|c| c.to_lowercase()).collect();
    for term in query.split_whitespace() {
        let term_lower: String = term.chars().flat_map(|c| c.to_lowercase()).collect();
        if !hay_lower.contains(&term_lower) {
            return false;
        }
    }
    true
}

/// Same as [`matches`] but checks multiple haystacks — returns true if all
/// terms match across the combined haystacks.
pub fn matches_any(query: &str, haystacks: &[&str]) -> bool {
    if query.is_empty() {
        return true;
    }
    let combined: String = haystacks.join(" ");
    matches(query, &combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches() {
        assert!(matches("", "anything"));
    }

    #[test]
    fn case_insensitive() {
        assert!(matches("HELLO", "hello world"));
        assert!(matches("hello", "HELLO WORLD"));
    }

    #[test]
    fn all_terms_required() {
        assert!(matches("foo bar", "a foo b bar c"));
        assert!(!matches("foo baz", "a foo b bar c"));
    }

    #[test]
    fn terms_in_any_order() {
        assert!(matches("bar foo", "foo bar"));
    }

    #[test]
    fn unicode() {
        assert!(matches("東方", "東方紅魔郷"));
        assert!(matches("björk", "BJÖRK"));
    }

    #[test]
    fn multi_haystack() {
        assert!(matches_any("foo bar", &["foo", "bar"]));
        assert!(matches_any("portishead dummy", &["Portishead", "Dummy", "1994"]));
    }
}
