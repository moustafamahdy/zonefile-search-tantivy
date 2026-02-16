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

    // Core fields (always present)
    pub domain_exact: Field,
    pub tokens: Field,
    pub tld: Field,
    pub len: Field,
    pub has_hyphen: Field,
    pub label: Field,

    // Detailed fields (may not be present in older indexes)
    pub country: Option<Field>,
    pub web_server: Option<Field>,
    pub ip: Option<Field>,
    pub dns_servers: Option<Field>,
    pub email: Option<Field>,
    pub phone: Option<Field>,
    pub seo_rank: Option<Field>,
}

impl DomainSchema {
    /// Create a new schema with all fields (for index creation)
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
        let label_options = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("default")
                    .set_index_option(tantivy::schema::IndexRecordOption::WithFreqs),
            )
            .set_stored();
        let label = schema_builder.add_text_field("label", label_options);

        // Detailed fields - filterable (STRING for exact match)
        let country = schema_builder.add_text_field("country", STRING | STORED);
        let web_server = schema_builder.add_text_field("web_server", STRING | STORED);

        // Detailed fields - stored only (display)
        let ip = schema_builder.add_text_field("ip", STORED);
        let dns_servers = schema_builder.add_text_field("dns_servers", STORED);
        let email = schema_builder.add_text_field("email", STORED);
        let phone = schema_builder.add_text_field("phone", STORED);
        let seo_rank = schema_builder.add_text_field("seo_rank", STORED);

        let schema = schema_builder.build();

        Self {
            schema,
            domain_exact,
            tokens,
            tld,
            len,
            has_hyphen,
            label,
            country: Some(country),
            web_server: Some(web_server),
            ip: Some(ip),
            dns_servers: Some(dns_servers),
            email: Some(email),
            phone: Some(phone),
            seo_rank: Some(seo_rank),
        }
    }

    /// Load schema from an existing index (for API/reader use)
    ///
    /// Core fields are required (panics if missing).
    /// Detailed fields are optional — returns None for older indexes.
    pub fn from_existing(schema: &Schema) -> Self {
        Self {
            schema: schema.clone(),
            domain_exact: schema.get_field("domain_exact").expect("missing domain_exact field"),
            tokens: schema.get_field("tokens").expect("missing tokens field"),
            tld: schema.get_field("tld").expect("missing tld field"),
            len: schema.get_field("len").expect("missing len field"),
            has_hyphen: schema.get_field("has_hyphen").expect("missing has_hyphen field"),
            label: schema.get_field("label").expect("missing label field"),
            country: schema.get_field("country").ok(),
            web_server: schema.get_field("web_server").ok(),
            ip: schema.get_field("ip").ok(),
            dns_servers: schema.get_field("dns_servers").ok(),
            email: schema.get_field("email").ok(),
            phone: schema.get_field("phone").ok(),
            seo_rank: schema.get_field("seo_rank").ok(),
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

        // Detailed fields (only added if present in schema and domain has detailed data)
        if let Some(ref detail) = domain.detailed {
            if let Some(field) = self.country {
                doc.add_text(field, &detail.country.to_lowercase());
            }
            if let Some(field) = self.web_server {
                doc.add_text(field, &detail.web_server.to_lowercase());
            }
            if let Some(field) = self.ip {
                doc.add_text(field, &detail.ip);
            }
            if let Some(field) = self.dns_servers {
                doc.add_text(field, &detail.dns_servers);
            }
            if let Some(field) = self.email {
                doc.add_text(field, &detail.email);
            }
            if let Some(field) = self.phone {
                doc.add_text(field, &detail.phone);
            }
            if let Some(field) = self.seo_rank {
                doc.add_text(field, &detail.seo_rank);
            }
        }

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
    use crate::domain::{DetailedRecord, Domain};

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

        // Verify detailed fields exist
        assert!(schema.schema.get_field("country").is_ok());
        assert!(schema.schema.get_field("web_server").is_ok());
        assert!(schema.schema.get_field("ip").is_ok());
        assert!(schema.schema.get_field("dns_servers").is_ok());
        assert!(schema.schema.get_field("email").is_ok());
        assert!(schema.schema.get_field("phone").is_ok());
        assert!(schema.schema.get_field("seo_rank").is_ok());
    }

    #[test]
    fn test_from_existing() {
        let original = DomainSchema::new();
        let loaded = DomainSchema::from_existing(&original.schema);

        // All fields should be present
        assert!(loaded.country.is_some());
        assert!(loaded.web_server.is_some());
        assert!(loaded.ip.is_some());
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

    #[test]
    fn test_to_document_with_detailed() {
        let schema = DomainSchema::new();

        let domain = Domain::new("example.com");
        let mut normalized = domain.normalize().unwrap();
        normalized.tokens = vec!["example".to_string()];
        normalized = normalized.with_detailed(DetailedRecord {
            dns_servers: "ns1.example.com".to_string(),
            ip: "93.184.216.34".to_string(),
            country: "US".to_string(),
            web_server: "nginx".to_string(),
            email: "admin@example.com".to_string(),
            phone: "+1234567890".to_string(),
            seo_rank: "42".to_string(),
        });

        let doc = schema.to_document(&normalized);

        use tantivy::schema::Value;

        // Verify detailed fields are present and lowercased where appropriate
        let country_val = doc.get_first(schema.country.unwrap())
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(country_val, "us"); // lowercased

        let ws_val = doc.get_first(schema.web_server.unwrap())
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(ws_val, "nginx"); // lowercased

        let ip_val = doc.get_first(schema.ip.unwrap())
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(ip_val, "93.184.216.34");
    }
}
