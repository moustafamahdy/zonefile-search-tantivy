use crate::html_parser::{parse_html_metadata, ParsedPageMeta};
use crate::model::PageMetadataResult;
use futures::StreamExt;
use reqwest::Client;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

/// Fetch the homepage of a domain and extract page metadata.
/// Tries HTTPS first, then HTTP. Never returns Err — all failures are captured in the error field.
pub async fn fetch_page_metadata(
    client: &Client,
    domain: &str,
    max_bytes: usize,
) -> PageMetadataResult {
    let fetched_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    for scheme in &["https", "http"] {
        let url = format!("{}://{}/", scheme, domain);

        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();

                // Check Content-Type — skip non-HTML responses
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                if let Some(ref ct) = content_type {
                    let ct_lower = ct.to_lowercase();
                    if !ct_lower.contains("text/html") && !ct_lower.contains("text/xhtml") {
                        let error_msg = format!("non-html content-type: {}", ct);
                        return PageMetadataResult {
                            domain: domain.to_string(),
                            http_status: status,
                            content_type,
                            fetched_at,
                            source: "direct".to_string(),
                            error: Some(error_msg),
                            ..default_page_result()
                        };
                    }
                }

                if !resp.status().is_success() {
                    if status == 403 || status == 404 || status == 410 || status == 451 {
                        return PageMetadataResult {
                            domain: domain.to_string(),
                            http_status: status,
                            content_type,
                            fetched_at,
                            source: "direct".to_string(),
                            error: Some(format!("http {}", status)),
                            ..default_page_result()
                        };
                    }
                    // Other errors — try next scheme
                    continue;
                }

                // Stream body up to max_bytes
                let body = stream_body(resp, max_bytes).await;
                let html = String::from_utf8_lossy(&body);

                let parsed = parse_html_metadata(&html);

                return build_result(domain, status, content_type, parsed, fetched_at);
            }
            Err(e) => {
                debug!(domain, scheme, error = %e, "Failed to fetch page");
                continue;
            }
        }
    }

    // Both schemes failed
    PageMetadataResult {
        domain: domain.to_string(),
        http_status: 0,
        fetched_at,
        source: "direct".to_string(),
        error: Some("connection failed".to_string()),
        ..default_page_result()
    }
}

fn build_result(
    domain: &str,
    status: u16,
    content_type: Option<String>,
    parsed: ParsedPageMeta,
    fetched_at: i64,
) -> PageMetadataResult {
    PageMetadataResult {
        domain: domain.to_string(),
        http_status: status,
        page_title: parsed.title,
        meta_description: parsed.meta_description,
        og_title: None,
        og_description: parsed.og_description,
        og_image: None,
        canonical_url: None,
        language: parsed.language,
        generator: None,
        snippet: parsed.snippet,
        content_type: None,
        fetched_at,
        source: "direct".to_string(),
        error: None,
    }
}

fn default_page_result() -> PageMetadataResult {
    PageMetadataResult {
        domain: String::new(),
        http_status: 0,
        page_title: None,
        meta_description: None,
        og_title: None,
        og_description: None,
        og_image: None,
        canonical_url: None,
        language: None,
        generator: None,
        snippet: None,
        content_type: None,
        fetched_at: 0,
        source: "direct".to_string(),
        error: None,
    }
}

/// Stream response body up to max_bytes (reuses pattern from sitemap.rs)
async fn stream_body(resp: reqwest::Response, max_bytes: usize) -> Vec<u8> {
    let mut body = Vec::with_capacity(64 * 1024);
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                let remaining = max_bytes.saturating_sub(body.len());
                if remaining == 0 {
                    break;
                }
                let take = bytes.len().min(remaining);
                body.extend_from_slice(&bytes[..take]);
            }
            Err(e) => {
                debug!(error = %e, "Page stream error");
                break;
            }
        }
    }

    body
}
