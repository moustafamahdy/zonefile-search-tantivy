use crate::model::{detect_cms_from_disallow_paths, detect_site_category, DomainMetadataResult};
use crate::robots::fetch_robots;
use crate::sitemap::{discover_sitemap_url, fetch_sitemap};
use reqwest::Client;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

/// Process a single domain through the full metadata pipeline.
/// Never returns Err — all failures are captured in the error field.
pub async fn process_domain(
    client: &Client,
    domain: &str,
    max_sitemap_bytes: usize,
) -> DomainMetadataResult {
    let fetched_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // Step 1: Fetch robots.txt
    let robots = fetch_robots(client, domain).await;

    // Step 2: CMS detection from Disallow paths
    let cms_hint = detect_cms_from_disallow_paths(&robots.disallow_paths);

    // Step 3: Discover sitemap URL (race robots.txt directives vs common paths)
    let sitemap_url = discover_sitemap_url(client, domain, &robots.sitemap_urls).await;

    // Step 4: Fetch + parse sitemap
    let sitemap_info = if let Some(ref url) = sitemap_url {
        debug!(domain, url, "Fetching sitemap");
        Some(fetch_sitemap(client, url, max_sitemap_bytes).await)
    } else {
        None
    };

    // Step 5: Site category from sitemap URL samples
    let site_category = sitemap_info
        .as_ref()
        .and_then(|s| detect_site_category(&s.sample_urls));

    let sitemap_found = sitemap_info.as_ref().map(|s| s.found).unwrap_or(false);

    DomainMetadataResult {
        domain: domain.to_string(),
        robots_found: robots.found,
        robots_status: robots.status,
        sitemap_url: if sitemap_found { sitemap_url } else { None },
        sitemap_found,
        sitemap_status: sitemap_info.as_ref().map(|s| s.status),
        url_count: sitemap_info.as_ref().map(|s| s.url_count).unwrap_or(0),
        url_count_estimated: sitemap_info
            .as_ref()
            .map(|s| s.url_count_estimated)
            .unwrap_or(false),
        latest_lastmod: sitemap_info
            .as_ref()
            .and_then(|s| s.latest_lastmod.clone()),
        oldest_lastmod: sitemap_info
            .as_ref()
            .and_then(|s| s.oldest_lastmod.clone()),
        is_sitemap_index: sitemap_info
            .as_ref()
            .map(|s| s.is_sitemap_index)
            .unwrap_or(false),
        child_sitemap_count: sitemap_info
            .as_ref()
            .map(|s| s.child_sitemap_count)
            .unwrap_or(0),
        crawl_delay: robots.crawl_delay,
        cms_hint,
        site_category,
        fetched_at,
        error: None,
    }
}
