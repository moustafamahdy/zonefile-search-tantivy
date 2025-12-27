use crate::AppState;
use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub index_documents: u64,
    pub index_segments: usize,
    pub cache_enabled: bool,
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub index: IndexStats,
    pub cache: Option<CacheStats>,
}

#[derive(Serialize)]
pub struct IndexStats {
    pub documents: u64,
    pub segments: usize,
    pub size_bytes: u64,
}

#[derive(Serialize)]
pub struct CacheStats {
    pub connected: bool,
    pub hits: u64,
    pub misses: u64,
}

/// Health check endpoint
pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let reader = state.index.reader().expect("Failed to get reader");
    let searcher = reader.searcher();

    Json(HealthResponse {
        status: "ok",
        index_documents: searcher.num_docs(),
        index_segments: searcher.segment_readers().len(),
        cache_enabled: state.cache.is_some(),
    })
}

/// Detailed statistics endpoint
pub async fn stats(State(state): State<Arc<AppState>>) -> Json<StatsResponse> {
    let reader = state.index.reader().expect("Failed to get reader");
    let searcher = reader.searcher();

    // Calculate index size
    let mut size_bytes: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(&state.config.index_path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    size_bytes += meta.len();
                }
            }
        }
    }

    let index_stats = IndexStats {
        documents: searcher.num_docs(),
        segments: searcher.segment_readers().len(),
        size_bytes,
    };

    let cache_stats = if let Some(cache) = &state.cache {
        let connected = cache.ping().await;
        let stats = cache.stats().await.ok();

        Some(CacheStats {
            connected,
            hits: stats.as_ref().map(|s| s.hits).unwrap_or(0),
            misses: stats.as_ref().map(|s| s.misses).unwrap_or(0),
        })
    } else {
        None
    };

    Json(StatsResponse {
        index: index_stats,
        cache: cache_stats,
    })
}
