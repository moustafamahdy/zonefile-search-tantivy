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

    // Parse query into tokens
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

    // Get reader and searcher
    let reader = state.index.reader().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Index error: {}", e))
    })?;
    let searcher = reader.searcher();

    // Smart candidate limit based on query complexity
    // Single keyword: fewer candidates needed (BM25 order is already good)
    // Multi-keyword: need more candidates to find high match-count results
    // TLD filter: need more candidates since we'll filter many out
    let base_limit = if num_query_tokens == 1 {
        params.limit as usize * 20
    } else {
        params.limit as usize * 50
    };
    let candidate_limit = if tld_filter.is_some() {
        base_limit.min(3000) // More candidates for TLD filtering
    } else {
        base_limit.min(1000)
    };

    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(candidate_limit))
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Search error: {}", e))
        })?;

    // Rescore candidates by match count
    let mut ranked_results: Vec<RankedResult> = Vec::with_capacity(candidate_limit);
    let mut perfect_matches = 0usize;
    let target_results = params.limit as usize;

    for (bm25_score, doc_address) in top_docs {
        let doc = searcher.doc(doc_address).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Doc error: {}", e))
        })?;

        let domain_result = extract_domain_result(&state.schema, &doc);

        // Count how many query tokens appear in the domain's tokens
        let doc_tokens: std::collections::HashSet<&str> =
            domain_result.tokens.iter().map(|s| s.as_str()).collect();

        let match_count = query_tokens
            .iter()
            .filter(|qt| doc_tokens.contains(qt.as_str()))
            .count();

        // Filter by minimum match count
        if match_count < min_match {
            continue;
        }

        // Filter by TLD if specified
        if let Some(ref tld) = tld_filter {
            if &domain_result.tld != tld {
                continue;
            }
        }

        // Track perfect matches for early termination
        if match_count == num_query_tokens {
            perfect_matches += 1;
        }

        ranked_results.push(RankedResult {
            domain: domain_result,
            match_count,
            bm25_score,
        });

        // Early termination: if we have enough perfect matches, stop
        if perfect_matches >= target_results * 2 {
            break;
        }
    }

    // Separate hyphenated and non-hyphenated domains
    let (mut hyphenated, mut non_hyphenated): (Vec<_>, Vec<_>) = ranked_results
        .into_iter()
        .partition(|r| r.domain.has_hyphen);

    // Sort each group by: match_count DESC, length ASC, bm25 DESC
    let sort_fn = |a: &RankedResult, b: &RankedResult| {
        b.match_count
            .cmp(&a.match_count)
            .then_with(|| a.domain.length.cmp(&b.domain.length))
            .then_with(|| b.bm25_score.partial_cmp(&a.bm25_score).unwrap_or(std::cmp::Ordering::Equal))
    };
    hyphenated.sort_by(sort_fn);
    non_hyphenated.sort_by(sort_fn);

    let total_candidates = hyphenated.len() + non_hyphenated.len();

    // Interleave results 50/50 (hyphenated first, then non-hyphenated, alternating)
    let limit = params.limit as usize;
    let mut results: Vec<SearchResult> = Vec::with_capacity(limit);

    let mut hyp_iter = hyphenated.into_iter().peekable();
    let mut non_hyp_iter = non_hyphenated.into_iter().peekable();

    // Alternate: hyphenated, non-hyphenated, hyphenated, non-hyphenated...
    while results.len() < limit {
        // Add hyphenated first
        if let Some(r) = hyp_iter.next() {
            results.push(SearchResult {
                domain: r.domain,
                match_count: r.match_count,
                score: r.bm25_score,
            });
        }
        if results.len() >= limit {
            break;
        }
        // Then add non-hyphenated
        if let Some(r) = non_hyp_iter.next() {
            results.push(SearchResult {
                domain: r.domain,
                match_count: r.match_count,
                score: r.bm25_score,
            });
        }
        // If both are exhausted, break
        if hyp_iter.peek().is_none() && non_hyp_iter.peek().is_none() {
            break;
        }
    }

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
        };

        // Check cache
        if let Some(cache) = &state.cache {
            let cache_key = Cache::make_key(
                &params.q,
                params.tld.as_deref(),
                params.limit,
                params.min_match,
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
