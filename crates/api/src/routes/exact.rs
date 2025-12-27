use crate::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use domain_core::Domain;
use serde::{Deserialize, Serialize};
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

        let result = extract_domain_result(&state.schema, &doc);

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

    DomainResult {
        domain,
        label,
        tld,
        length,
        has_hyphen,
        tokens,
    }
}
