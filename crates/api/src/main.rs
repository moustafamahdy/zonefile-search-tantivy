use anyhow::Result;
use axum::{
    routing::{get, post},
    Router,
};
use domain_core::{register_ngram_tokenizer, Config, DomainSchema};
use std::sync::Arc;
use tantivy::Index;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod cache;
mod precheck_db;
mod routes;
mod search;

use cache::Cache;
use precheck_db::PrecheckDb;

/// Shared application state
pub struct AppState {
    pub config: Config,
    pub schema: DomainSchema,
    pub index: Index,
    pub cache: Option<Cache>,
    pub precheck_db: Option<PrecheckDb>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,tower_http=debug".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env()?;

    info!(index_path = ?config.index_path, "Opening index");

    // Open Tantivy index
    let index = Index::open_in_dir(&config.index_path)?;
    register_ngram_tokenizer(&index);
    let schema = DomainSchema::from_existing(&index.schema());

    // Warm up the index reader
    let reader = index.reader()?;
    let searcher = reader.searcher();
    info!(documents = searcher.num_docs(), "Index loaded");

    // Initialize Redis cache (optional)
    let cache = match &config.redis_url {
        Some(url) => {
            info!(url = url, "Connecting to Redis");
            match Cache::new(url).await {
                Ok(c) => {
                    info!("Redis cache enabled");
                    Some(c)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Redis unavailable, running without cache");
                    None
                }
            }
        }
        None => {
            info!("Running without cache (REDIS_URL not set)");
            None
        }
    };

    // Open precheck SQLite DB (optional — read-only)
    let precheck_db = {
        let db_path = std::env::var("METADATA_DB_PATH")
            .unwrap_or_else(|_| "./data/metadata.db".to_string());
        match PrecheckDb::open(&db_path) {
            Ok(db) => {
                info!(path = db_path, "Precheck DB loaded");
                Some(db)
            }
            Err(e) => {
                tracing::warn!(error = %e, path = db_path, "Precheck DB unavailable, is_reachable will be null");
                None
            }
        }
    };

    let state = Arc::new(AppState {
        config: config.clone(),
        schema,
        index,
        cache,
        precheck_db,
    });

    // Build router
    let app = Router::new()
        .route("/health", get(routes::health::health))
        .route("/stats", get(routes::health::stats))
        .route("/exact", get(routes::exact::exact_lookup))
        .route("/exact/bulk", post(routes::exact::bulk_exact_lookup))
        .route("/search", get(routes::search::search))
        .route("/search/bulk", post(routes::search::bulk_search))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.api_port);
    info!(address = addr, "Starting server");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
