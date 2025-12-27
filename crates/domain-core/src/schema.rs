use crate::domain::NormalizedDomain;
use tantivy::schema::{
    Facet, FacetOptions, Field, NumericOptions, Schema, TextFieldIndexing, TextOptions,
    STORED, STRING,
};
use tantivy::TantivyDocument;

/// Tantivy schema for domain search
#[derive(Clone)]
pub struct DomainSchema {
    pub schema: Schema,

    // Fields
    pub domain_exact: Field,
    pub tokens: Field,
    pub tld: Field,
    pub len: Field,
    pub has_hyphen: Field,
    pub label: Field,
}

impl DomainSchema {
    /// Create a new schema for domain indexing
    pub fn new() -> Self {
        let mut schema_builder = Schema::builder();

        // domain_exact: STRING (not tokenized) - for exact lookup + delete
        // STORED so we can retrieve the full domain
        let domain_exact = schema_builder.add_text_field("domain_exact", STRING | STORED);

        // tokens: TEXT (tokenized) - for keyword search
        // Using default tokenizer with lowercase
        let text_options = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("default")
                    .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored();
        let tokens = schema_builder.add_text_field("tokens", text_options);

        // tld: FACET - for filtering (e.g., /com, /net)
        let tld = schema_builder.add_facet_field("tld", FacetOptions::default());

        // len: u16 FAST - for tie-breaking and filtering
        let len = schema_builder.add_u64_field(
            "len",
            NumericOptions::default().set_fast().set_stored(),
        );

        // has_hyphen: u8 FAST - for filtering
        let has_hyphen = schema_builder.add_u64_field(
            "has_hyphen",
            NumericOptions::default().set_fast().set_stored(),
        );

        // label: TEXT (tokenized, stored) - the label without TLD
        // Useful for display and debugging
        let label_options = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("default")
                    .set_index_option(tantivy::schema::IndexRecordOption::WithFreqs),
            )
            .set_stored();
        let label = schema_builder.add_text_field("label", label_options);

        let schema = schema_builder.build();

        Self {
            schema,
            domain_exact,
            tokens,
            tld,
            len,
            has_hyphen,
            label,
        }
    }

    /// Convert a normalized domain to a Tantivy document
    pub fn to_document(&self, domain: &NormalizedDomain) -> TantivyDocument {
        let mut doc = TantivyDocument::new();

        // domain_exact - full normalized domain
        doc.add_text(self.domain_exact, &domain.domain_exact);

        // tokens - joined with space for default tokenizer
        let tokens_text = domain.tokens.join(" ");
        doc.add_text(self.tokens, &tokens_text);

        // tld as facet (e.g., "/com")
        let facet = Facet::from_path(vec![&domain.tld]);
        doc.add_facet(self.tld, facet);

        // len
        doc.add_u64(self.len, domain.len as u64);

        // has_hyphen (0 or 1)
        doc.add_u64(self.has_hyphen, if domain.has_hyphen { 1 } else { 0 });

        // label
        doc.add_text(self.label, &domain.label);

        doc
    }
}

impl Default for DomainSchema {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Domain;

    #[test]
    fn test_schema_creation() {
        let schema = DomainSchema::new();

        // Verify all fields exist
        assert!(schema.schema.get_field("domain_exact").is_ok());
        assert!(schema.schema.get_field("tokens").is_ok());
        assert!(schema.schema.get_field("tld").is_ok());
        assert!(schema.schema.get_field("len").is_ok());
        assert!(schema.schema.get_field("has_hyphen").is_ok());
        assert!(schema.schema.get_field("label").is_ok());
    }

    #[test]
    fn test_to_document() {
        let schema = DomainSchema::new();

        let domain = Domain::new("middleofnight.com");
        let mut normalized = domain.normalize().unwrap();
        normalized.tokens = vec!["middle".to_string(), "of".to_string(), "night".to_string()];

        let doc = schema.to_document(&normalized);

        // Verify document has the expected fields
        assert!(doc.get_first(schema.domain_exact).is_some());
        assert!(doc.get_first(schema.tokens).is_some());
        assert!(doc.get_first(schema.tld).is_some());
        assert!(doc.get_first(schema.len).is_some());
    }
}
