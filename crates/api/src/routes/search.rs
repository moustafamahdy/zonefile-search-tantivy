use crate::cache::Cache;
use crate::routes::exact::{extract_domain_result, DomainResult};
use crate::search::ranking::RankedResult;
use crate::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use domain_core::generate_trigrams;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, EnableScoring, Occur, Query as _, TermQuery};
use tantivy::schema::IndexRecordOption;
use tantivy::{DocAddress, DocSet, Term, TERMINATED};

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

    let query_tokens: Vec<String> = params
        .q
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();

    if query_tokens.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Query cannot be empty".to_string()));
    }

    // Use trigram search if the label_ngrams field is available, otherwise fall back
    if let Some(ngram_field) = state.schema.label_ngrams {
        execute_trigram_search(state, params, &query_tokens, ngram_field, start).await
    } else {
        execute_legacy_search(state, params, &query_tokens, start).await
    }
}

/// Trigram-based substring search — primary search method for new indexes
///
/// Uses manual segment iteration with scoring disabled instead of TopDocs.
/// This avoids BM25 scoring overhead (which requires evaluating ALL matching docs
/// to maintain a top-K heap) and instead iterates through matching docs directly,
/// stopping as soon as we have enough results.
async fn execute_trigram_search(
    state: &AppState,
    params: &SearchQuery,
    query_tokens: &[String],
    ngram_field: tantivy::schema::Field,
    start: std::time::Instant,
) -> Result<SearchResponse, (StatusCode, String)> {
    let num_query_tokens = query_tokens.len();
    let min_match = params.min_match.unwrap_or(num_query_tokens as u32) as usize;
    let tld_filter = params.tld.as_ref().map(|t| t.to_lowercase());
    let country_filter = params.country.as_ref().map(|c| c.to_lowercase());
    let web_server_filter = params.web_server.as_ref().map(|w| w.to_lowercase());
    let target_results = params.limit as usize;

    // Build trigram query for each keyword
    let mut keyword_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
    let mut short_keywords: Vec<&str> = Vec::new(); // keywords < 3 chars (no trigrams)
    let require_all = min_match >= num_query_tokens;

    for keyword in query_tokens {
        let trigrams = generate_trigrams(keyword);
        if trigrams.is_empty() {
            // Keyword too short for trigrams — will verify via post-filter
            short_keywords.push(keyword);
            continue;
        }
        // AND all trigrams for this keyword (all must be present in the label)
        let trigram_terms: Vec<(Occur, Box<dyn tantivy::query::Query>)> = trigrams
            .iter()
            .map(|tri| {
                let term = Term::from_field_text(ngram_field, tri);
                (
                    Occur::Must,
                    Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs))
                        as Box<dyn tantivy::query::Query>,
                )
            })
            .collect();
        let keyword_query = BooleanQuery::new(trigram_terms);

        // MUST when all keywords required (fast intersection), SHOULD for partial matching
        let occur = if require_all { Occur::Must } else { Occur::Should };
        keyword_clauses.push((occur, Box::new(keyword_query)));
    }

    if keyword_clauses.is_empty() {
        // All keywords are < 3 chars — can't use trigrams, fall back to legacy
        return execute_legacy_search(state, params, query_tokens, start).await;
    }

    let combined_query = BooleanQuery::new(keyword_clauses);

    let reader = state.index.reader().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Index error: {}", e))
    })?;
    let searcher = reader.searcher();

    // Disable BM25 scoring — we rank by our own criteria (has_hyphen, length).
    // This avoids the expensive scoring heap that TopDocs requires, which must
    // evaluate ALL matching docs even when we only need a few hundred.
    let weight = combined_query
        .weight(EnableScoring::disabled_from_searcher(&searcher))
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Weight error: {}", e))
        })?;

    let mut ranked_results: Vec<RankedResult> = Vec::with_capacity(target_results);
    let mut seen_domains: HashSet<String> = HashSet::new();
    let num_segments = searcher.segment_readers().len().max(1);

    // ── Phase 0: Exact label lookup ──
    // For popular single keywords like "crypto", the trigram/token scans sample
    // too few docs per segment to guarantee finding crypto.com among millions
    // of matches. Solve this with O(1) term lookups: "keyword.tld" on domain_exact.
    if query_tokens.len() == 1 {
        let keyword = &query_tokens[0];
        const TOP_TLDS: &[&str] = &[
            "com", "net", "org", "io", "co", "info", "xyz", "app", "dev", "ai",
            "us", "uk", "de", "ca", "fr", "nl", "it", "es", "eu", "me",
            "biz", "tech", "online", "site", "store", "shop", "club", "live",
        ];
        for tld in TOP_TLDS {
            let domain_key = format!("{}.{}", keyword, tld);
            let term = Term::from_field_text(state.schema.domain_exact, &domain_key);
            let exact_query = TermQuery::new(term, IndexRecordOption::Basic);
            if let Ok(hits) = searcher.search(&exact_query, &TopDocs::with_limit(1)) {
                for (_score, doc_address) in hits {
                    if let Ok(doc) = searcher.doc::<tantivy::TantivyDocument>(doc_address) {
                        let domain_result = extract_domain_result(&state.schema, &doc);
                        if let Some(ref f) = tld_filter {
                            if &domain_result.tld != f { continue; }
                        }
                        if let Some(ref f) = country_filter {
                            if !domain_result.country.as_ref().is_some_and(|c| c.to_lowercase() == *f) { continue; }
                        }
                        if let Some(ref f) = web_server_filter {
                            if !domain_result.web_server.as_ref().is_some_and(|w| w.to_lowercase().contains(f.as_str())) { continue; }
                        }
                        seen_domains.insert(domain_result.domain.clone());
                        ranked_results.push(RankedResult {
                            domain: domain_result,
                            match_count: num_query_tokens,
                            label_match_count: 0,
                            bm25_score: 0.0,
                        });
                    }
                }
            }
        }
    }

    // ── Phase 1: Token-based search (exact word matches) ──
    // Query the `tokens` field (word-segmented) via segment iteration (no scoring)
    // to find domains where keywords are exact tokens. This ensures globally best
    // matches like crypto.com are always found regardless of segment ordering.
    // Uses a small per-segment budget since token matches are high-precision.
    {
        let mut token_clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
        for keyword in query_tokens {
            let term = Term::from_field_text(state.schema.tokens, keyword);
            let occur = if require_all { Occur::Must } else { Occur::Should };
            token_clauses.push((
                occur,
                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs))
                    as Box<dyn tantivy::query::Query>,
            ));
        }
        let token_query = BooleanQuery::new(token_clauses);
        let token_weight = token_query
            .weight(EnableScoring::disabled_from_searcher(&searcher))
            .map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Weight error: {}", e))
            })?;

        // Small budget per segment: token matches are precise, so a few per segment
        // is enough to catch the best exact-word matches (crypto.com, etc.)
        let token_per_seg = (target_results * 2 / num_segments).max(50);

        for (segment_ord, segment_reader) in searcher.segment_readers().iter().enumerate() {
            let mut scorer: Box<dyn tantivy::query::Scorer> =
                match token_weight.scorer(segment_reader, 1.0) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

            let mut seg_count: usize = 0;
            let mut doc_id = scorer.doc();
            while doc_id != TERMINATED {
                seg_count += 1;
                if seg_count > token_per_seg {
                    break;
                }

                let doc_address = DocAddress::new(segment_ord as u32, doc_id);
                let doc: tantivy::TantivyDocument =
                    match searcher.doc(doc_address) {
                        Ok(d) => d,
                        Err(_) => {
                            doc_id = scorer.advance();
                            continue;
                        }
                    };

                use tantivy::schema::Value;
                let domain_name = doc
                    .get_first(state.schema.domain_exact)
                    .and_then(|v: &tantivy::schema::OwnedValue| v.as_str())
                    .unwrap_or("");

                if seen_domains.contains(domain_name) {
                    doc_id = scorer.advance();
                    continue;
                }

                let label = doc
                    .get_first(state.schema.label)
                    .and_then(|v: &tantivy::schema::OwnedValue| v.as_str())
                    .unwrap_or("");
                let label_lower = label.to_lowercase();

                let match_count = query_tokens
                    .iter()
                    .filter(|kw| label_lower.contains(kw.as_str()))
                    .count();

                if match_count < min_match {
                    doc_id = scorer.advance();
                    continue;
                }
                if !short_keywords.iter().all(|kw| label_lower.contains(kw)) {
                    doc_id = scorer.advance();
                    continue;
                }

                let domain_result = extract_domain_result(&state.schema, &doc);

                if let Some(ref tld) = tld_filter {
                    if &domain_result.tld != tld {
                        doc_id = scorer.advance();
                        continue;
                    }
                }
                if let Some(ref country) = country_filter {
                    match &domain_result.country {
                        Some(dc) if dc.to_lowercase() == *country => {}
                        _ => {
                            doc_id = scorer.advance();
                            continue;
                        }
                    }
                }
                if let Some(ref ws) = web_server_filter {
                    match &domain_result.web_server {
                        Some(dws) if dws.to_lowercase().contains(ws.as_str()) => {}
                        _ => {
                            doc_id = scorer.advance();
                            continue;
                        }
                    }
                }

                seen_domains.insert(domain_result.domain.clone());
                ranked_results.push(RankedResult {
                    domain: domain_result,
                    match_count,
                    label_match_count: 0,
                    bm25_score: 0.0,
                });

                doc_id = scorer.advance();
            }
        }
    }

    // ── Phase 2: Trigram-based segment scan (substring matches) ──
    // Finds domains where keywords appear as substrings but aren't exact tokens
    // (e.g., "pay" inside "executivepayrollsolutions").
    let total_scan_budget = if require_all {
        (target_results * 100).min(500_000)
    } else {
        (target_results * 50).min(200_000)
    };
    let per_segment_budget = (total_scan_budget / num_segments).max(200);
    let collect_target = target_results * 5;

    for (segment_ord, segment_reader) in searcher.segment_readers().iter().enumerate() {
        if ranked_results.len() >= collect_target {
            break;
        }

        let mut scorer: Box<dyn tantivy::query::Scorer> =
            match weight.scorer(segment_reader, 1.0) {
                Ok(s) => s,
                Err(_) => continue,
            };

        let mut segment_scanned: usize = 0;
        let mut doc_id = scorer.doc();
        while doc_id != TERMINATED {
            segment_scanned += 1;
            if segment_scanned > per_segment_budget {
                break;
            }

            let doc_address = DocAddress::new(segment_ord as u32, doc_id);
            let doc: tantivy::TantivyDocument = searcher.doc(doc_address).map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Doc error: {}", e))
            })?;

            use tantivy::schema::Value;
            let domain_name = doc
                .get_first(state.schema.domain_exact)
                .and_then(|v: &tantivy::schema::OwnedValue| v.as_str())
                .unwrap_or("");

            if seen_domains.contains(domain_name) {
                doc_id = scorer.advance();
                continue;
            }

            let label = doc
                .get_first(state.schema.label)
                .and_then(|v: &tantivy::schema::OwnedValue| v.as_str())
                .unwrap_or("");
            let label_lower = label.to_lowercase();

            let match_count = query_tokens
                .iter()
                .filter(|kw| label_lower.contains(kw.as_str()))
                .count();

            if match_count < min_match {
                doc_id = scorer.advance();
                continue;
            }

            if !short_keywords.iter().all(|kw| label_lower.contains(kw)) {
                doc_id = scorer.advance();
                continue;
            }

            let domain_result = extract_domain_result(&state.schema, &doc);

            if let Some(ref tld) = tld_filter {
                if &domain_result.tld != tld {
                    doc_id = scorer.advance();
                    continue;
                }
            }
            if let Some(ref country) = country_filter {
                match &domain_result.country {
                    Some(dc) if dc.to_lowercase() == *country => {}
                    _ => {
                        doc_id = scorer.advance();
                        continue;
                    }
                }
            }
            if let Some(ref ws) = web_server_filter {
                match &domain_result.web_server {
                    Some(dws) if dws.to_lowercase().contains(ws.as_str()) => {}
                    _ => {
                        doc_id = scorer.advance();
                        continue;
                    }
                }
            }

            seen_domains.insert(domain_result.domain.clone());
            ranked_results.push(RankedResult {
                domain: domain_result,
                match_count,
                label_match_count: 0,
                bm25_score: 0.0,
            });

            doc_id = scorer.advance();
        }
    }

    let total_candidates = ranked_results.len();

    // Sort by: match_count DESC, has_hyphen ASC, length ASC
    ranked_results.sort_by(|a, b| {
        b.match_count
            .cmp(&a.match_count)
            .then_with(|| a.domain.has_hyphen.cmp(&b.domain.has_hyphen))
            .then_with(|| a.domain.length.cmp(&b.domain.length))
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

/// Legacy two-pass search — fallback for old indexes without label_ngrams field
async fn execute_legacy_search(
    state: &AppState,
    params: &SearchQuery,
    query_tokens: &[String],
    start: std::time::Instant,
) -> Result<SearchResponse, (StatusCode, String)> {
    let min_match = params.min_match.unwrap_or(1) as usize;

    // Build Tantivy query (OR of all tokens)
    let mut token_queries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

    for token in query_tokens {
        let term = Term::from_field_text(state.schema.tokens, token);
        let term_query = TermQuery::new(term, IndexRecordOption::WithFreqs);
        token_queries.push((Occur::Should, Box::new(term_query)));
    }

    let query = BooleanQuery::new(token_queries);
    let num_query_tokens = query_tokens.len();
    let tld_filter = params.tld.as_ref().map(|t| t.to_lowercase());
    let country_filter = params.country.as_ref().map(|c| c.to_lowercase());
    let web_server_filter = params.web_server.as_ref().map(|w| w.to_lowercase());

    let reader = state.index.reader().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Index error: {}", e))
    })?;
    let searcher = reader.searcher();

    let target_results = params.limit as usize;

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

        seen_domains.insert(domain_result.domain.clone());
        ranked_results.push(RankedResult {
            domain: domain_result,
            match_count,
            label_match_count,
            bm25_score,
        });
    }

    let total_candidates = ranked_results.len();

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
