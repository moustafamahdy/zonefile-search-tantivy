use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Word splitter API base URL
    pub word_splitter_url: String,

    /// Word splitter API username
    pub word_splitter_user: String,

    /// Word splitter API password
    pub word_splitter_pass: String,

    /// Zonefile API token
    pub zonefile_token: String,

    /// Zonefile API base URL
    pub zonefile_api_url: String,

    /// Path to the Tantivy index
    pub index_path: PathBuf,

    /// Redis URL for caching
    pub redis_url: Option<String>,

    /// API server port
    pub api_port: u16,

    /// IndexWriter heap size in bytes (default: 4GB)
    pub index_heap_size: usize,

    /// Batch size for word segmentation API calls
    pub word_batch_size: usize,

    /// Batch size for indexing commits
    pub index_batch_size: usize,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            word_splitter_url: env::var("WORD_SPLITTER_URL")
                .unwrap_or_else(|_| "https://moustafamahdy.xyz/word-splitter-api".to_string()),

            word_splitter_user: env::var("WORD_SPLITTER_USER")
                .map_err(|_| Error::Config("WORD_SPLITTER_USER not set".to_string()))?,

            word_splitter_pass: env::var("WORD_SPLITTER_PASS")
                .map_err(|_| Error::Config("WORD_SPLITTER_PASS not set".to_string()))?,

            zonefile_token: env::var("ZONEFILE_TOKEN")
                .map_err(|_| Error::Config("ZONEFILE_TOKEN not set".to_string()))?,

            zonefile_api_url: env::var("ZONEFILE_API_URL")
                .unwrap_or_else(|_| "https://domains-monitor.com/api/v1".to_string()),

            index_path: env::var("INDEX_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/index")),

            redis_url: env::var("REDIS_URL").ok(),

            api_port: env::var("API_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3000),

            index_heap_size: env::var("INDEX_HEAP_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4 * 1024 * 1024 * 1024), // 4GB default

            word_batch_size: env::var("WORD_BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(500), // Max allowed by API

            index_batch_size: env::var("INDEX_BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1_000_000), // Commit every 1M docs
        })
    }

    /// Create a test configuration
    #[cfg(test)]
    pub fn test() -> Self {
        Self {
            word_splitter_url: "http://localhost:8080".to_string(),
            word_splitter_user: "test".to_string(),
            word_splitter_pass: "test".to_string(),
            zonefile_token: "test-token".to_string(),
            zonefile_api_url: "http://localhost:8081".to_string(),
            index_path: PathBuf::from("/tmp/test-index"),
            redis_url: None,
            api_port: 3000,
            index_heap_size: 50 * 1024 * 1024, // 50MB for tests
            word_batch_size: 10,
            index_batch_size: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = Config::test();
        assert_eq!(config.api_port, 3000);
        assert_eq!(config.word_batch_size, 10);
    }
}
