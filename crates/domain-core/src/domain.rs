use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Raw domain input before normalization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    pub raw: String,
}

/// Normalized domain with extracted components
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedDomain {
    /// Full normalized domain (e.g., "example.com")
    pub domain_exact: String,

    /// Label without TLD (e.g., "example")
    pub label: String,

    /// TLD (e.g., "com")
    pub tld: String,

    /// Length of the label
    pub len: u16,

    /// Whether the label contains a hyphen
    pub has_hyphen: bool,

    /// Segmented tokens from word splitter (filled later)
    pub tokens: Vec<String>,
}

impl Domain {
    pub fn new(raw: impl Into<String>) -> Self {
        Self { raw: raw.into() }
    }

    /// Normalize the domain
    ///
    /// - Lowercase
    /// - IDNA/punycode normalization
    /// - Strip trailing dot
    /// - Extract label and TLD
    pub fn normalize(&self) -> Result<NormalizedDomain> {
        let raw = self.raw.trim();

        // Strip trailing dot
        let domain = raw.strip_suffix('.').unwrap_or(raw);

        // Lowercase
        let domain_lower = domain.to_lowercase();

        // IDNA normalization (handles punycode and unicode domains)
        let domain_normalized = match idna::domain_to_ascii(&domain_lower) {
            Ok(d) => d,
            Err(_) => {
                // If IDNA fails, use lowercase version
                domain_lower.clone()
            }
        };

        // Split into label and TLD
        let parts: Vec<&str> = domain_normalized.rsplitn(2, '.').collect();

        if parts.len() < 2 {
            return Err(Error::InvalidDomain(format!(
                "Domain must have at least one dot: {}",
                self.raw
            )));
        }

        let tld = parts[0].to_string();
        let label = parts[1].to_string();

        // Validate label length (DNS limit)
        if label.len() > 63 {
            return Err(Error::InvalidDomain(format!(
                "Label exceeds 63 characters: {}",
                self.raw
            )));
        }

        // Validate label is not empty
        if label.is_empty() {
            return Err(Error::InvalidDomain(format!(
                "Empty label: {}",
                self.raw
            )));
        }

        let has_hyphen = label.contains('-');
        let len = label.len() as u16;

        Ok(NormalizedDomain {
            domain_exact: domain_normalized,
            label,
            tld,
            len,
            has_hyphen,
            tokens: Vec::new(),
        })
    }
}

impl NormalizedDomain {
    /// Generate a deterministic ID from the domain
    /// Uses MD5 hash truncated to 48 bits (6 bytes)
    pub fn generate_id(&self) -> u64 {
        let digest = md5::compute(self.domain_exact.as_bytes());
        let bytes = digest.as_ref();

        // Take first 6 bytes for a 48-bit ID
        let mut id_bytes = [0u8; 8];
        id_bytes[2..8].copy_from_slice(&bytes[0..6]);

        u64::from_be_bytes(id_bytes)
    }

    /// Set tokens from word segmentation
    pub fn with_tokens(mut self, tokens: Vec<String>) -> Self {
        self.tokens = tokens;
        self
    }
}

/// Check if a domain should be filtered out during indexing
pub fn should_filter_domain(label: &str) -> bool {
    // Filter pure numeric labels longer than 5 chars
    if label.len() > 5 && label.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }

    // Filter repetitive patterns (e.g., "aaaaa")
    if label.len() >= 5 {
        let first = label.chars().next().unwrap();
        if label.chars().all(|c| c == first) {
            return true;
        }
    }

    // Filter labels that start with digit and contain only digits/hyphens
    if label.starts_with(|c: char| c.is_ascii_digit()) {
        if label.chars().all(|c| c.is_ascii_digit() || c == '-') {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_simple_domain() {
        let domain = Domain::new("Example.COM");
        let normalized = domain.normalize().unwrap();

        assert_eq!(normalized.domain_exact, "example.com");
        assert_eq!(normalized.label, "example");
        assert_eq!(normalized.tld, "com");
        assert_eq!(normalized.len, 7);
        assert!(!normalized.has_hyphen);
    }

    #[test]
    fn test_normalize_with_trailing_dot() {
        let domain = Domain::new("example.com.");
        let normalized = domain.normalize().unwrap();

        assert_eq!(normalized.domain_exact, "example.com");
    }

    #[test]
    fn test_normalize_hyphenated() {
        let domain = Domain::new("my-domain.net");
        let normalized = domain.normalize().unwrap();

        assert_eq!(normalized.label, "my-domain");
        assert!(normalized.has_hyphen);
    }

    #[test]
    fn test_normalize_unicode_domain() {
        let domain = Domain::new("münchen.de");
        let normalized = domain.normalize().unwrap();

        // Should be converted to punycode
        assert_eq!(normalized.domain_exact, "xn--mnchen-3ya.de");
    }

    #[test]
    fn test_generate_id_deterministic() {
        let domain = Domain::new("example.com");
        let normalized = domain.normalize().unwrap();

        let id1 = normalized.generate_id();
        let id2 = normalized.generate_id();

        assert_eq!(id1, id2);
    }

    #[test]
    fn test_invalid_domain_no_dot() {
        let domain = Domain::new("nodot");
        assert!(domain.normalize().is_err());
    }

    #[test]
    fn test_should_filter_numeric() {
        assert!(should_filter_domain("123456"));
        assert!(!should_filter_domain("12345")); // 5 chars is ok
        assert!(!should_filter_domain("abc123"));
    }

    #[test]
    fn test_should_filter_repetitive() {
        assert!(should_filter_domain("aaaaa"));
        assert!(should_filter_domain("xxxxxxx"));
        assert!(!should_filter_domain("ababa"));
    }

    #[test]
    fn test_should_filter_numeric_hyphen() {
        assert!(should_filter_domain("1-2-3"));
        assert!(!should_filter_domain("a-1-2"));
    }
}
