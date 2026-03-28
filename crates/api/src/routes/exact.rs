use crate::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use domain_core::Domain;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::Term;

#[derive(Deserialize)]
pub struct ExactQuery {
    pub domain: String,
}

#[derive(Serialize)]
pub struct ExactResponse {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<DomainResult>,
    pub query_time_ms: f64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DomainResult {
    pub domain: String,
    pub label: String,
    pub tld: String,
    pub length: u64,
    pub has_hyphen: bool,
    pub tokens: Vec<String>,

    // Detailed fields (omitted from JSON when None)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_servers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seo_rank: Option<String>,

    // Page metadata fields (omitted from JSON when None)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub og_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    // Precheck reachability (true = alive, false = unreachable/not checked)
    pub is_reachable: Option<bool>,
}

// Bulk exact lookup types
#[derive(Deserialize)]
pub struct BulkExactRequest {
    pub domains: Vec<String>,
}

#[derive(Serialize)]
pub struct BulkExactResponse {
    pub results: HashMap<String, bool>,
    pub found_count: usize,
    pub not_found_count: usize,
    pub query_time_ms: f64,
}

/// Exact domain lookup
pub async fn exact_lookup(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ExactQuery>,
) -> Result<Json<ExactResponse>, (StatusCode, String)> {
    let start = std::time::Instant::now();

    // Normalize the input domain
    let domain = Domain::new(&params.domain);
    let normalized = domain.normalize().map_err(|e| {
        (StatusCode::BAD_REQUEST, format!("Invalid domain: {}", e))
    })?;

    // Search for exact match
    let reader = state.index.reader().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Index error: {}", e))
    })?;
    let searcher = reader.searcher();

    let term = Term::from_field_text(state.schema.domain_exact, &normalized.domain_exact);
    let query = TermQuery::new(term, IndexRecordOption::Basic);

    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(1))
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Search error: {}", e))
        })?;

    let query_time_ms = start.elapsed().as_secs_f64() * 1000.0;

    if let Some((_score, doc_address)) = top_docs.first() {
        let doc = searcher.doc(*doc_address).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Doc error: {}", e))
        })?;

        let mut result = extract_domain_result(&state.schema, &doc);

        // Enrich with is_reachable
        if let Some(ref precheck_db) = state.precheck_db {
            let lookup = precheck_db.batch_lookup(&[result.domain.as_str()]);
            result.is_reachable = lookup.get(&result.domain).copied();
        }

        Ok(Json(ExactResponse {
            found: true,
            domain: Some(result),
            query_time_ms,
        }))
    } else {
        Ok(Json(ExactResponse {
            found: false,
            domain: None,
            query_time_ms,
        }))
    }
}

/// Bulk exact domain lookup - check multiple domains at once
pub async fn bulk_exact_lookup(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BulkExactRequest>,
) -> Result<Json<BulkExactResponse>, (StatusCode, String)> {
    let start = std::time::Instant::now();

    // Limit to 1000 domains per request to prevent abuse
    if payload.domains.len() > 1000 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Maximum 1000 domains per request".to_string(),
        ));
    }

    let reader = state.index.reader().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Index error: {}", e))
    })?;
    let searcher = reader.searcher();

    let mut results: HashMap<String, bool> = HashMap::new();
    let mut found_count = 0;
    let mut not_found_count = 0;

    for domain_str in &payload.domains {
        // Normalize the input domain
        let domain = Domain::new(domain_str);
        let normalized = match domain.normalize() {
            Ok(n) => n,
            Err(_) => {
                // Invalid domain, mark as not found
                results.insert(domain_str.to_lowercase(), false);
                not_found_count += 1;
                continue;
            }
        };

        // Search for exact match
        let term = Term::from_field_text(state.schema.domain_exact, &normalized.domain_exact);
        let query = TermQuery::new(term, IndexRecordOption::Basic);

        let top_docs = match searcher.search(&query, &TopDocs::with_limit(1)) {
            Ok(docs) => docs,
            Err(_) => {
                results.insert(domain_str.to_lowercase(), false);
                not_found_count += 1;
                continue;
            }
        };

        let found = !top_docs.is_empty();
        results.insert(domain_str.to_lowercase(), found);

        if found {
            found_count += 1;
        } else {
            not_found_count += 1;
        }
    }

    let query_time_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(Json(BulkExactResponse {
        results,
        found_count,
        not_found_count,
        query_time_ms,
    }))
}

/// Extract domain result from a Tantivy document
pub fn extract_domain_result(
    schema: &domain_core::DomainSchema,
    doc: &tantivy::TantivyDocument,
) -> DomainResult {
    use tantivy::schema::Value;

    let domain = doc
        .get_first(schema.domain_exact)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let label = doc
        .get_first(schema.label)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Extract TLD from domain string (facet not stored)
    let tld = domain
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_string();

    let length = doc
        .get_first(schema.len)
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let has_hyphen = doc
        .get_first(schema.has_hyphen)
        .and_then(|v| v.as_u64())
        .map(|v| v == 1)
        .unwrap_or(false);

    // Extract tokens
    let tokens_str = doc
        .get_first(schema.tokens)
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let tokens: Vec<String> = if tokens_str.is_empty() {
        vec![]
    } else {
        tokens_str.split_whitespace().map(String::from).collect()
    };

    // Extract optional detailed fields
    let extract_optional = |field: Option<tantivy::schema::Field>| -> Option<String> {
        field.and_then(|f| {
            doc.get_first(f)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
    };

    let ip = extract_optional(schema.ip);
    let country = extract_optional(schema.country);
    let dns_servers = extract_optional(schema.dns_servers);
    let web_server = extract_optional(schema.web_server);
    let email = extract_optional(schema.email);
    let phone = extract_optional(schema.phone);
    let seo_rank = extract_optional(schema.seo_rank);

    let page_title = extract_optional(schema.page_title);
    let meta_description = extract_optional(schema.meta_description);
    let og_image = extract_optional(schema.og_image);
    let snippet = extract_optional(schema.snippet);
    let language = extract_optional(schema.language);

    DomainResult {
        domain,
        label,
        tld,
        length,
        has_hyphen,
        tokens,
        ip,
        country,
        dns_servers,
        web_server,
        email,
        phone,
        seo_rank,
        page_title,
        meta_description,
        og_image,
        snippet,
        language,
        is_reachable: None, // Populated after search from precheck DB
    }
}
