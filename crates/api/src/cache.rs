use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

const CACHE_TTL: u64 = 86400; // 24 hours in seconds
const KEY_PREFIX: &str = "ds:"; // domain-search prefix

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CacheError>;

/// Redis cache wrapper
#[derive(Clone)]
pub struct Cache {
    conn: ConnectionManager,
}

impl Cache {
    /// Create a new cache connection
    pub async fn new(redis_url: &str) -> Result<Self> {
        let client = redis::Client::open(redis_url)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Self { conn })
    }

    /// Get a cached value
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let full_key = format!("{}{}", KEY_PREFIX, key);
        let mut conn = self.conn.clone();

        let data: Option<String> = conn.get(&full_key).await?;

        match data {
            Some(json) => {
                let value: T = serde_json::from_str(&json)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Set a cached value with TTL
    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let full_key = format!("{}{}", KEY_PREFIX, key);
        let json = serde_json::to_string(value)?;
        let mut conn = self.conn.clone();

        let _: () = conn.set_ex(&full_key, json, CACHE_TTL).await?;
        Ok(())
    }

    /// Delete a cached value
    pub async fn delete(&self, key: &str) -> Result<()> {
        let full_key = format!("{}{}", KEY_PREFIX, key);
        let mut conn = self.conn.clone();

        let _: () = conn.del(&full_key).await?;
        Ok(())
    }

    /// Generate a cache key from query parameters
    pub fn make_key(query: &str, tld: Option<&str>, limit: u32, min_match: Option<u32>) -> String {
        let tld_part = tld.unwrap_or("any");
        let min_match_part = min_match.unwrap_or(1);
        format!("search:{}|{}|{}|{}", query, tld_part, limit, min_match_part)
    }

    /// Check if cache is healthy
    pub async fn ping(&self) -> bool {
        let mut conn = self.conn.clone();
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .is_ok()
    }

    /// Get cache statistics
    pub async fn stats(&self) -> Result<CacheStats> {
        let mut conn = self.conn.clone();

        let info: String = redis::cmd("INFO")
            .arg("stats")
            .query_async(&mut conn)
            .await?;

        // Parse basic stats from INFO output
        let mut hits: u64 = 0;
        let mut misses: u64 = 0;

        for line in info.lines() {
            if line.starts_with("keyspace_hits:") {
                hits = line.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            } else if line.starts_with("keyspace_misses:") {
                misses = line.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }

        Ok(CacheStats { hits, misses })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}
