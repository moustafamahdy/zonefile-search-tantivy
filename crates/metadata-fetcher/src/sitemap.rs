use futures::StreamExt;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use reqwest::Client;
use tracing::debug;

const SITEMAP_INDEX_EARLY_EXIT: u16 = 10;
const URLS_PER_SUBSITEMAP_ESTIMATE: u32 = 1000;
const MAX_SUBSITEMAP_SAMPLES: usize = 3;
const SUB_SITEMAP_TIMEOUT_SECS: u64 = 5;

/// Result of parsing a sitemap
#[derive(Debug, Default)]
pub struct SitemapInfo {
    pub status: u16,
    pub found: bool,
    pub url_count: u32,
    pub url_count_estimated: bool,
    pub latest_lastmod: Option<String>,
    pub oldest_lastmod: Option<String>,
    pub is_sitemap_index: bool,
    pub child_sitemap_count: u16,
    pub sample_urls: Vec<String>,
}

/// Common sitemap paths to probe in parallel with robots.txt directives
const COMMON_SITEMAP_PATHS: &[&str] = &[
    "/sitemap.xml",
    "/sitemap_index.xml",
    "/sitemap-index.xml",
];

/// Discover the sitemap URL for a domain by racing robots.txt directives
/// against common paths. Returns the first valid sitemap URL found.
pub async fn discover_sitemap_url(
    client: &Client,
    domain: &str,
    robots_sitemap_urls: &[String],
) -> Option<String> {
    // If robots.txt gave us sitemap URLs, use the first one directly
    if !robots_sitemap_urls.is_empty() {
        return Some(robots_sitemap_urls[0].clone());
    }

    // Race common paths - first valid response wins
    let mut futures = Vec::new();
    for path in COMMON_SITEMAP_PATHS {
        let url = format!("https://{}{}", domain, path);
        let client = client.clone();
        futures.push(tokio::spawn(async move {
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    // Quick check: read first few bytes to verify it's XML-like
                    if let Ok(bytes) = resp.bytes().await {
                        let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
                        if preview.contains('<') {
                            return Some(url);
                        }
                    }
                    None
                }
                _ => None,
            }
        }));
    }

    // Return first non-None result
    for future in futures {
        if let Ok(Some(url)) = future.await {
            return Some(url);
        }
    }

    None
}

/// Fetch and parse a sitemap URL, stopping after max_bytes
pub async fn fetch_sitemap(
    client: &Client,
    url: &str,
    max_bytes: usize,
) -> SitemapInfo {
    match client.get(url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if !resp.status().is_success() {
                return SitemapInfo {
                    status,
                    ..Default::default()
                };
            }

            // Check for .txt sitemaps
            if url.ends_with(".txt") {
                return fetch_text_sitemap(resp, status, max_bytes).await;
            }

            // Stream body up to max_bytes
            let body = stream_body(resp, max_bytes).await;
            parse_sitemap_body(&body, status, client, max_bytes).await
        }
        Err(e) => {
            debug!(url, error = %e, "Failed to fetch sitemap");
            SitemapInfo::default()
        }
    }
}

/// Handle plain text sitemaps (one URL per line)
async fn fetch_text_sitemap(
    resp: reqwest::Response,
    status: u16,
    max_bytes: usize,
) -> SitemapInfo {
    let body = stream_body(resp, max_bytes).await;
    let text = String::from_utf8_lossy(&body);
    let urls: Vec<String> = text
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty() && trimmed.starts_with("http")
        })
        .map(|l| l.trim().to_string())
        .collect();

    let sample_urls: Vec<String> = urls.iter().take(20).cloned().collect();

    SitemapInfo {
        status,
        found: true,
        url_count: urls.len() as u32,
        url_count_estimated: false,
        sample_urls,
        ..Default::default()
    }
}

/// Stream response body up to max_bytes
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
                debug!(error = %e, "Sitemap stream error");
                break;
            }
        }
    }

    body
}

/// Parse XML sitemap body. Handles both regular sitemaps and sitemap indexes.
async fn parse_sitemap_body(
    body: &[u8],
    status: u16,
    client: &Client,
    max_bytes: usize,
) -> SitemapInfo {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);

    let mut info = SitemapInfo {
        status,
        found: true,
        ..Default::default()
    };

    let mut in_url = false;
    let mut in_sitemap = false;
    let mut in_lastmod = false;
    let mut in_loc = false;
    let mut child_sitemap_urls: Vec<String> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"sitemapindex" => {
                    info.is_sitemap_index = true;
                }
                b"url" => {
                    in_url = true;
                }
                b"sitemap" if info.is_sitemap_index => {
                    in_sitemap = true;
                    info.child_sitemap_count = info.child_sitemap_count.saturating_add(1);
                }
                b"lastmod" => {
                    in_lastmod = true;
                }
                b"loc" => {
                    in_loc = true;
                }
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"url" => {
                    in_url = false;
                    in_loc = false;
                    in_lastmod = false;
                }
                b"sitemap" => {
                    in_sitemap = false;
                    in_loc = false;
                    in_lastmod = false;
                }
                b"lastmod" => {
                    in_lastmod = false;
                }
                b"loc" => {
                    in_loc = false;
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if let Ok(text) = e.unescape() {
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        buf.clear();
                        continue;
                    }

                    if in_lastmod && (in_url || in_sitemap) {
                        update_lastmod(&mut info, &text);
                    }
                    if in_loc && in_url {
                        info.url_count += 1;
                        if info.sample_urls.len() < 20 {
                            info.sample_urls.push(text.clone());
                        }
                    }
                    if in_loc && in_sitemap {
                        child_sitemap_urls.push(text);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    // Handle sitemap indexes: estimate total URL count
    if info.is_sitemap_index && !child_sitemap_urls.is_empty() {
        let (count, estimated) = estimate_sitemap_index_urls(
            client,
            &child_sitemap_urls,
            max_bytes,
        )
        .await;
        info.url_count = count;
        info.url_count_estimated = estimated;
    }

    info
}

/// Estimate total URLs in a sitemap index by sampling child sitemaps.
/// Uses the same strategy as domain-filter.ts:
/// - >10 children: estimate at 1000 URLs each (no network)
/// - <=10: sample first 3, extrapolate
async fn estimate_sitemap_index_urls(
    client: &Client,
    child_urls: &[String],
    max_bytes: usize,
) -> (u32, bool) {
    let count = child_urls.len();

    // Early exit for large sitemap indexes
    if count > SITEMAP_INDEX_EARLY_EXIT as usize {
        return (
            count as u32 * URLS_PER_SUBSITEMAP_ESTIMATE,
            true,
        );
    }

    // Sample first N child sitemaps in parallel
    let samples_to_fetch = count.min(MAX_SUBSITEMAP_SAMPLES);
    let sample_urls = &child_urls[..samples_to_fetch];

    let mut handles = Vec::new();
    for url in sample_urls {
        let client = client.clone();
        let url = url.clone();
        let max = max_bytes;
        handles.push(tokio::spawn(async move {
            count_subsitemap_urls(&client, &url, max).await
        }));
    }

    let mut total_sampled: u32 = 0;
    let mut samples_completed: usize = 0;
    for handle in handles {
        if let Ok(count) = handle.await {
            total_sampled += count;
            samples_completed += 1;
        }
    }

    if samples_completed == 0 {
        return (0, true);
    }

    // If we sampled all children, return actual count
    if count <= samples_to_fetch {
        return (total_sampled, false);
    }

    // Extrapolate from sample average
    let avg = total_sampled as f64 / samples_completed as f64;
    let estimated = (avg * count as f64).round() as u32;
    (estimated, true)
}

/// Count URLs in a single sub-sitemap
async fn count_subsitemap_urls(client: &Client, url: &str, max_bytes: usize) -> u32 {
    let resp = match client.get(url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return 0,
    };

    if url.ends_with(".txt") {
        let body = stream_body(resp, max_bytes).await;
        let text = String::from_utf8_lossy(&body);
        return text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count() as u32;
    }

    let body = stream_body(resp, max_bytes).await;
    // Fast regex-like count of <loc> tags
    count_loc_tags(&body)
}

/// Fast count of <loc> tags in XML body
fn count_loc_tags(body: &[u8]) -> u32 {
    let text = String::from_utf8_lossy(body);
    text.matches("<loc>").count() as u32
}

fn update_lastmod(info: &mut SitemapInfo, date: &str) {
    match &info.latest_lastmod {
        None => info.latest_lastmod = Some(date.to_string()),
        Some(existing) if date > existing.as_str() => {
            info.latest_lastmod = Some(date.to_string());
        }
        _ => {}
    }
    match &info.oldest_lastmod {
        None => info.oldest_lastmod = Some(date.to_string()),
        Some(existing) if date < existing.as_str() => {
            info.oldest_lastmod = Some(date.to_string());
        }
        _ => {}
    }
}
