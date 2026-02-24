use rust_stemmers::{Algorithm, Stemmer};

/// Stem a single token using the Snowball English stemmer.
pub fn stem_token(token: &str) -> String {
    let stemmer = Stemmer::create(Algorithm::English);
    stemmer.stem(token).into_owned()
}

/// Stem a batch of tokens, returning deduplicated results preserving order.
pub fn stem_tokens(tokens: &[String]) -> Vec<String> {
    let stemmer = Stemmer::create(Algorithm::English);
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(tokens.len());

    for token in tokens {
        let stemmed = stemmer.stem(token).into_owned();
        if seen.insert(stemmed.clone()) {
            result.push(stemmed);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stem_token_basic() {
        assert_eq!(stem_token("farms"), "farm");
        assert_eq!(stem_token("running"), "run");
        assert_eq!(stem_token("indoor"), "indoor");
    }

    #[test]
    fn test_stem_token_already_stemmed() {
        assert_eq!(stem_token("farm"), "farm");
        assert_eq!(stem_token("run"), "run");
    }

    #[test]
    fn test_stem_tokens_dedup() {
        let tokens = vec![
            "indoor".to_string(),
            "farms".to_string(),
            "farm".to_string(),
        ];
        let result = stem_tokens(&tokens);
        assert_eq!(result, vec!["indoor", "farm"]);
    }

    #[test]
    fn test_stem_tokens_preserves_order() {
        let tokens = vec![
            "marketing".to_string(),
            "solutions".to_string(),
        ];
        let result = stem_tokens(&tokens);
        assert_eq!(result[0], stem_token("marketing"));
        assert_eq!(result[1], stem_token("solutions"));
    }

    #[test]
    fn test_stem_tokens_empty() {
        let tokens: Vec<String> = vec![];
        let result = stem_tokens(&tokens);
        assert!(result.is_empty());
    }
}
