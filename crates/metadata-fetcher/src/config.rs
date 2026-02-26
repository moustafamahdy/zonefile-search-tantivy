use crate::error::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FetcherConfig {
    /// Path to the Tantivy index
    pub index_path: PathBuf,

    /// Path to SQLite database for crawl results
    pub db_path: PathBuf,

    /// Maximum concurrent HTTP requests
    pub concurrency: usize,

    /// Per-request total timeout in seconds
    pub request_timeout_secs: u64,

    /// Connect timeout in seconds
    pub connect_timeout_secs: u64,

    /// Maximum sitemap body size in bytes before truncation
    pub max_sitemap_bytes: usize,

    /// Batch size for domain export from Tantivy
    pub export_batch_size: usize,

    /// Commit interval for Tantivy import
    pub import_commit_interval: usize,
}

impl FetcherConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            index_path: std::env::var("INDEX_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/index")),

            db_path: std::env::var("METADATA_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/metadata.db")),

            concurrency: std::env::var("METADATA_CONCURRENCY")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3000),

            request_timeout_secs: std::env::var("METADATA_REQUEST_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),

            connect_timeout_secs: std::env::var("METADATA_CONNECT_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),

            max_sitemap_bytes: std::env::var("METADATA_MAX_SITEMAP_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5 * 1024 * 1024), // 5MB

            export_batch_size: std::env::var("METADATA_EXPORT_BATCH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(50_000),

            import_commit_interval: std::env::var("METADATA_IMPORT_COMMIT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500_000),
        })
    }
}
