pub mod config;
pub mod domain;
pub mod error;
pub mod schema;
pub mod stemmer;

pub use config::Config;
pub use domain::{DetailedRecord, Domain, NormalizedDomain};
pub use error::Error;
pub use schema::DomainSchema;
pub use stemmer::{stem_token, stem_tokens};

/// Generate trigrams from a string (for building search queries).
///
/// Returns empty vec for strings shorter than 3 characters.
/// Example: "pay" -> ["pay"], "executive" -> ["exe", "xec", "ecu", "cut", "uti", "tiv", "ive"]
pub fn generate_trigrams(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    if chars.len() < 3 {
        return vec![];
    }
    (0..=chars.len() - 3)
        .map(|i| chars[i..i + 3].iter().collect())
        .collect()
}

/// Register the ngram3 tokenizer with a Tantivy index.
/// Must be called after creating or opening an index.
pub fn register_ngram_tokenizer(index: &tantivy::Index) {
    use tantivy::tokenizer::NgramTokenizer;
    index
        .tokenizers()
        .register("ngram3", NgramTokenizer::new(3, 3, false).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_trigrams_short() {
        assert_eq!(generate_trigrams("ab"), Vec::<String>::new());
        assert_eq!(generate_trigrams("a"), Vec::<String>::new());
        assert_eq!(generate_trigrams(""), Vec::<String>::new());
    }

    #[test]
    fn test_generate_trigrams_exact_three() {
        assert_eq!(generate_trigrams("pay"), vec!["pay"]);
        assert_eq!(generate_trigrams("PAY"), vec!["pay"]); // lowercased
    }

    #[test]
    fn test_generate_trigrams_longer() {
        let result = generate_trigrams("executive");
        assert_eq!(result, vec!["exe", "xec", "ecu", "cut", "uti", "tiv", "ive"]);
    }

    #[test]
    fn test_generate_trigrams_with_digits() {
        let result = generate_trigrams("abc123");
        assert_eq!(result, vec!["abc", "bc1", "c12", "123"]);
    }
}
