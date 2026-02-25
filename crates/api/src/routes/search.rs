use crate::cache::Cache;
use crate::routes::exact::{extract_domain_result, DomainResult};
use crate::search::ranking::RankedResult;
use crate::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, TermQuery};
use tantivy::schema::IndexRecordOption;
use tantivy::Term;

#[derive(Deserialize)]
pub struct SearchQuery {
    /// Search keywords (space-separated)
    pub q: String,

    /// Filter by TLD (e.g., "com", "net")
    pub tld: Option<String>,

    /// Maximum results to return
    #[serde(default = "default_limit")]
    pub limit: u32,

    /// Minimum number of keywords that must match
    pub min_match: Option<u32>,

    /// Filter by country code (e.g., "us", "de")
    pub country: Option<String>,

    /// Filter by web server (e.g., "nginx", "apache")
    pub web_server: Option<String>,
}

fn default_limit() -> u32 {
    50
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub total_candidates: usize,
    pub query_time_ms: f64,
    pub cached: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SearchResult {
    #[serde(flatten)]
    pub domain: DomainResult,
    pub match_count: usize,
    pub label_match_count: usize,
    pub score: f32,
}

#[derive(Deserialize)]
pub struct BulkSearchRequest {
    pub queries: Vec<BulkQuery>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Deserialize)]
pub struct BulkQuery {
    pub q: String,
    pub tld: Option<String>,
    pub min_match: Option<u32>,
    pub country: Option<String>,
    pub web_server: Option<String>,
}

#[derive(Serialize)]
pub struct BulkSearchResponse {
    pub results: Vec<SearchResponse>,
    pub total_time_ms: f64,
}

/// Keyword search endpoint
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    let start = std::time::Instant::now();

    // Check cache first
    if let Some(cache) = &state.cache {
        let cache_key = Cache::make_key(
            &params.q,
            params.tld.as_deref(),
            params.limit,
            params.min_match,
            params.country.as_deref(),
            params.web_server.as_deref(),
        );

        if let Ok(Some(cached)) = cache.get::<SearchResponse>(&cache_key).await {
            let mut response = cached;
            response.cached = true;
            response.query_time_ms = start.elapsed().as_secs_f64() * 1000.0;
            return Ok(Json(response));
        }
    }

    // Execute search
    let response = execute_search(&state, &params).await?;

    // Store in cache
    if let Some(cache) = &state.cache {
        let cache_key = Cache::make_key(
            &params.q,
            params.tld.as_deref(),
            params.limit,
            params.min_match,
            params.country.as_deref(),
            params.web_server.as_deref(),
        );
        let _ = cache.set(&cache_key, &response).await;
    }

    Ok(Json(response))
}

/// Execute the actual search
async fn execute_search(
    state: &AppState,
    params: &SearchQuery,
) -> Result<SearchResponse, (StatusCode, String)> {
    let start = std::time::Instant::now();

    // Parse query into lowercase tokens (no stemming — index stores both forms from API keywords)
    let query_tokens: Vec<String> = params
        .q
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();

    if query_tokens.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Query cannot be empty".to_string()));
    }

    let min_match = params.min_match.unwrap_or(1) as usize;

    // Build Tantivy query (OR of all tokens)
    let mut token_queries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

    for token in &query_tokens {
        let term = Term::from_field_text(state.schema.tokens, token);
        let term_query = TermQuery::new(term, IndexRecordOption::WithFreqs);
        token_queries.push((Occur::Should, Box::new(term_query)));
    }

    // Note: TLD filtering is done post-query for better performance
    // Facet queries are expensive; filtering during result processing is faster

    let query = BooleanQuery::new(token_queries);
    let num_query_tokens = query_tokens.len();
    let tld_filter = params.tld.as_ref().map(|t| t.to_lowercase());
    let country_filter = params.country.as_ref().map(|c| c.to_lowercase());
    let web_server_filter = params.web_server.as_ref().map(|w| w.to_lowercase());

    // Get reader and searcher
    let reader = state.index.reader().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Index error: {}", e))
    })?;
    let searcher = reader.searcher();

    let target_results = params.limit as usize;

    // --- Pass 1: OR query across all tokens ---
    let pass1_limit = if num_query_tokens == 1 {
        (target_results * 20).min(10000)
    } else {
        (target_results * 50).min(10000)
    };

    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(pass1_limit))
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Search error: {}", e))
        })?;

    let mut ranked_results: Vec<RankedResult> = Vec::with_capacity(pass1_limit);
    let mut seen_domains: HashSet<String> = HashSet::new();
    let mut perfect_matches = 0usize;

    for (bm25_score, doc_address) in top_docs {
        let doc = searcher.doc(doc_address).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Doc error: {}", e))
        })?;

        let domain_result = extract_domain_result(&state.schema, &doc);

        let doc_tokens: HashSet<&str> =
            domain_result.tokens.iter().map(|s| s.as_str()).collect();

        let match_count = query_tokens
            .iter()
            .filter(|qt| doc_tokens.contains(qt.as_str()))
            .count();

        // Count query tokens found as substrings in the domain label (but not in tokens)
        let label_lower = domain_result.label.to_lowercase();
        let label_match_count = query_tokens
            .iter()
            .filter(|qt| !doc_tokens.contains(qt.as_str()) && label_lower.contains(qt.as_str()))
            .count();

        let total_match = match_count + label_match_count;

        if total_match < min_match {
            continue;
        }

        // Apply post-filters
        if let Some(ref tld) = tld_filter {
            if &domain_result.tld != tld {
                continue;
            }
        }
        if let Some(ref country) = country_filter {
            match &domain_result.country {
                Some(dc) if dc.to_lowercase() == *country => {}
                _ => continue,
            }
        }
        if let Some(ref ws) = web_server_filter {
            match &domain_result.web_server {
                Some(dws) if dws.to_lowercase().contains(ws.as_str()) => {}
                _ => continue,
            }
        }

        if total_match == num_query_tokens {
            perfect_matches += 1;
        }

        seen_domains.insert(domain_result.domain.clone());
        ranked_results.push(RankedResult {
            domain: domain_result,
            match_count,
            label_match_count,
            bm25_score,
        });

        // Early termination if we have enough perfect matches
        if perfect_matches >= target_results * 2
            || (perfect_matches >= target_results && ranked_results.len() >= target_results * 3)
        {
            break;
        }
    }

    // --- Pass 2: Targeted label substring search ---
    // Only for multi-token queries when pass 1 didn't find enough high-quality results
    if num_query_tokens > 1 && perfect_matches < target_results {
        let pass2_limit = (target_results * 20).min(5000);

        for (i, token) in query_tokens.iter().enumerate() {
            // Other tokens that need to appear as label substrings
            let other_tokens: Vec<&str> = query_tokens
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, t)| t.as_str())
                .collect();

            let term = Term::from_field_text(state.schema.tokens, token);
            let term_query = TermQuery::new(term, IndexRecordOption::WithFreqs);

            let pass2_docs = searcher
                .search(&term_query, &TopDocs::with_limit(pass2_limit))
                .map_err(|e| {
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("Search error: {}", e))
                })?;

            for (bm25_score, doc_address) in pass2_docs {
                let doc = searcher.doc(doc_address).map_err(|e| {
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("Doc error: {}", e))
                })?;

                let domain_result = extract_domain_result(&state.schema, &doc);

                // Skip already seen domains
                if seen_domains.contains(&domain_result.domain) {
                    continue;
                }

                // Check if other tokens appear as substrings in the label
                let label_lower = domain_result.label.to_lowercase();
                let label_match_count = other_tokens
                    .iter()
                    .filter(|t| label_lower.contains(*t))
                    .count();

                if label_match_count == 0 {
                    continue;
                }

                // Apply post-filters
                if let Some(ref tld) = tld_filter {
                    if &domain_result.tld != tld {
                        continue;
                    }
                }
                if let Some(ref country) = country_filter {
                    match &domain_result.country {
                        Some(dc) if dc.to_lowercase() == *country => {}
                        _ => continue,
                    }
                }
                if let Some(ref ws) = web_server_filter {
                    match &domain_result.web_server {
                        Some(dws) if dws.to_lowercase().contains(ws.as_str()) => {}
                        _ => continue,
                    }
                }

                seen_domains.insert(domain_result.domain.clone());
                ranked_results.push(RankedResult {
                    domain: domain_result,
                    match_count: 1, // matched the one token
                    label_match_count,
                    bm25_score,
                });
            }
        }
    }

    let total_candidates = ranked_results.len();

    // Sort by: match_count DESC, label_match_count DESC, has_hyphen ASC, length ASC, bm25 DESC
    ranked_results.sort_by(|a, b| {
        b.match_count
            .cmp(&a.match_count)
            .then_with(|| b.label_match_count.cmp(&a.label_match_count))
            .then_with(|| a.domain.has_hyphen.cmp(&b.domain.has_hyphen))
            .then_with(|| a.domain.length.cmp(&b.domain.length))
            .then_with(|| {
                b.bm25_score
                    .partial_cmp(&a.bm25_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let limit = params.limit as usize;
    let results: Vec<SearchResult> = ranked_results
        .into_iter()
        .take(limit)
        .map(|r| SearchResult {
            domain: r.domain,
            match_count: r.match_count,
            label_match_count: r.label_match_count,
            score: r.bm25_score,
        })
        .collect();

    let query_time_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(SearchResponse {
        results,
        total_candidates,
        query_time_ms,
        cached: false,
    })
}

/// Bulk search endpoint
pub async fn bulk_search(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BulkSearchRequest>,
) -> Result<Json<BulkSearchResponse>, (StatusCode, String)> {
    let start = std::time::Instant::now();

    if request.queries.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Maximum 100 queries per bulk request".to_string(),
        ));
    }

    let mut results = Vec::with_capacity(request.queries.len());

    for query in &request.queries {
        let params = SearchQuery {
            q: query.q.clone(),
            tld: query.tld.clone(),
            limit: request.limit,
            min_match: query.min_match,
            country: query.country.clone(),
            web_server: query.web_server.clone(),
        };

        // Check cache
        if let Some(cache) = &state.cache {
            let cache_key = Cache::make_key(
                &params.q,
                params.tld.as_deref(),
                params.limit,
                params.min_match,
                params.country.as_deref(),
                params.web_server.as_deref(),
            );

            if let Ok(Some(cached)) = cache.get::<SearchResponse>(&cache_key).await {
                let mut response = cached;
                response.cached = true;
                results.push(response);
                continue;
            }
        }

        // Execute search
        match execute_search(&state, &params).await {
            Ok(response) => {
                // Cache result
                if let Some(cache) = &state.cache {
                    let cache_key = Cache::make_key(
                        &params.q,
                        params.tld.as_deref(),
                        params.limit,
                        params.min_match,
                        params.country.as_deref(),
                        params.web_server.as_deref(),
                    );
                    let _ = cache.set(&cache_key, &response).await;
                }
                results.push(response);
            }
            Err((_, msg)) => {
                // Return empty result for failed queries
                results.push(SearchResponse {
                    results: vec![],
                    total_candidates: 0,
                    query_time_ms: 0.0,
                    cached: false,
                });
                tracing::warn!(query = %query.q, error = %msg, "Bulk query failed");
            }
        }
    }

    let total_time_ms = start.elapsed().as_secs_f64() * 1000.0;

    Ok(Json(BulkSearchResponse {
        results,
        total_time_ms,
    }))
}
