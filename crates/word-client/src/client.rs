use crate::error::{Error, Result};
use futures::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Request body for bulk segmentation
#[derive(Debug, Serialize)]
struct BulkRequest {
    labels: Vec<String>,
}

/// Response from bulk segmentation
#[derive(Debug, Deserialize)]
struct BulkResponse {
    results: Vec<SegmentResult>,
}

/// Individual segmentation result
#[derive(Debug, Deserialize)]
struct SegmentResult {
    label: String,
    /// The segmented words (this is the main output)
    segmentation: Vec<String>,
    /// Keywords extracted (includes compounds like "marketing" -> "market")
    #[serde(default)]
    keywords: Vec<String>,
}

/// Client for the word segmentation API
#[derive(Clone)]
pub struct WordClient {
    client: Client,
    base_url: String,
    max_batch_size: usize,
    parallel_requests: usize,
}

impl WordClient {
    /// Create a new WordClient
    ///
    /// # Arguments
    /// * `base_url` - Base URL of the word splitter API
    /// * `username` - Basic auth username
    /// * `password` - Basic auth password
    /// * `max_batch_size` - Maximum labels per batch request (default: 50000)
    /// * `parallel_requests` - Number of parallel API requests (default: 4)
    pub fn new(
        base_url: impl Into<String>,
        username: impl AsRef<str>,
        password: impl AsRef<str>,
        max_batch_size: Option<usize>,
        parallel_requests: Option<usize>,
    ) -> Result<Self> {
        let base_url = base_url.into();

        // Pre-encode basic auth header
        let auth = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!("{}:{}", username.as_ref(), password.as_ref()),
        );

        let client = Client::builder()
            .timeout(Duration::from_secs(120)) // Increased timeout for large batches
            .pool_max_idle_per_host(20)
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    format!("Basic {}", auth).parse().unwrap(),
                );
                headers
            })
            .build()?;

        Ok(Self {
            client,
            base_url,
            max_batch_size: max_batch_size.unwrap_or(50000),
            parallel_requests: parallel_requests.unwrap_or(4),
        })
    }

    /// Segment a batch of labels using parallel API calls
    ///
    /// Returns a Vec of (label, segments) pairs in the same order as input
    pub async fn segment_batch(&self, labels: Vec<String>) -> Result<Vec<(String, Vec<String>)>> {
        if labels.is_empty() {
            return Ok(Vec::new());
        }

        // Split into chunks for API batching
        let chunks: Vec<Vec<String>> = labels
            .chunks(self.max_batch_size)
            .map(|c| c.to_vec())
            .collect();

        let total_chunks = chunks.len();

        if total_chunks == 1 {
            // Single batch, no parallelization needed
            return self.segment_batch_internal(chunks.into_iter().next().unwrap()).await;
        }

        info!(
            total_labels = labels.len(),
            chunks = total_chunks,
            parallel = self.parallel_requests,
            "Processing with parallel API calls"
        );

        // Process chunks in parallel batches
        let mut all_results = Vec::with_capacity(labels.len());

        for parallel_batch in chunks.chunks(self.parallel_requests) {
            // Launch parallel requests
            let futures: Vec<_> = parallel_batch
                .iter()
                .map(|chunk| self.segment_batch_internal(chunk.clone()))
                .collect();

            // Wait for all parallel requests
            let results = join_all(futures).await;

            // Collect results in order
            for result in results {
                match result {
                    Ok(batch_results) => all_results.extend(batch_results),
                    Err(e) => {
                        warn!("Parallel batch failed: {}", e);
                        return Err(e);
                    }
                }
            }
        }

        Ok(all_results)
    }

    async fn segment_batch_internal(
        &self,
        labels: Vec<String>,
    ) -> Result<Vec<(String, Vec<String>)>> {
        let url = format!("{}/segment/bulk", self.base_url);

        debug!(count = labels.len(), "Sending batch segmentation request");

        let request = BulkRequest { labels: labels.clone() };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                message,
            });
        }

        let bulk_response: BulkResponse = response.json().await?;

        // Convert to (label, segments) pairs
        // The API returns results in the same order as input
        let results: Vec<(String, Vec<String>)> = bulk_response
            .results
            .into_iter()
            .map(|r| (r.label, r.segmentation))
            .collect();

        // Verify we got the expected number of results
        if results.len() != labels.len() {
            warn!(
                expected = labels.len(),
                got = results.len(),
                "Segment response count mismatch"
            );
        }

        Ok(results)
    }

    /// Segment a single label (convenience method)
    pub async fn segment_single(&self, label: &str) -> Result<Vec<String>> {
        let results = self.segment_batch(vec![label.to_string()]).await?;

        results
            .into_iter()
            .next()
            .map(|(_, segments)| segments)
            .ok_or_else(|| Error::InvalidResponse("Empty response".to_string()))
    }
}

// Need to add base64 dependency - let's inline it for now
mod base64 {
    pub trait Engine {
        fn encode(engine: &Self, input: impl AsRef<[u8]>) -> String;
    }

    pub mod engine {
        pub mod general_purpose {
            pub struct StandardEngine;
            pub static STANDARD: StandardEngine = StandardEngine;
        }
    }

    impl Engine for engine::general_purpose::StandardEngine {
        fn encode(_engine: &Self, input: impl AsRef<[u8]>) -> String {
            // Simple base64 encoding
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

            let input = input.as_ref();
            let mut output = String::new();

            for chunk in input.chunks(3) {
                let mut n = (chunk[0] as u32) << 16;
                if chunk.len() > 1 {
                    n |= (chunk[1] as u32) << 8;
                }
                if chunk.len() > 2 {
                    n |= chunk[2] as u32;
                }

                output.push(ALPHABET[(n >> 18 & 0x3F) as usize] as char);
                output.push(ALPHABET[(n >> 12 & 0x3F) as usize] as char);

                if chunk.len() > 1 {
                    output.push(ALPHABET[(n >> 6 & 0x3F) as usize] as char);
                } else {
                    output.push('=');
                }

                if chunk.len() > 2 {
                    output.push(ALPHABET[(n & 0x3F) as usize] as char);
                } else {
                    output.push('=');
                }
            }

            output
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        use base64::Engine;
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            "user:pass",
        );
        assert_eq!(encoded, "dXNlcjpwYXNz");
    }
}
