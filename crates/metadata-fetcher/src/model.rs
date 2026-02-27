use serde::{Deserialize, Serialize};

/// Result of fetching metadata for a single domain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainMetadataResult {
    pub domain: String,
    pub robots_found: bool,
    pub robots_status: u16,
    pub sitemap_url: Option<String>,
    pub sitemap_found: bool,
    pub sitemap_status: Option<u16>,
    pub url_count: u32,
    pub url_count_estimated: bool,
    pub latest_lastmod: Option<String>,
    pub oldest_lastmod: Option<String>,
    pub is_sitemap_index: bool,
    pub child_sitemap_count: u16,
    pub crawl_delay: Option<f32>,
    pub cms_hint: Option<String>,
    pub site_category: Option<String>,
    pub fetched_at: i64,
    pub error: Option<String>,
}

/// Detect CMS from robots.txt disallow paths
pub fn detect_cms_from_disallow_paths(paths: &[String]) -> Option<String> {
    for path in paths {
        let p = path.to_lowercase();
        if p.contains("wp-content") || p.contains("wp-admin") || p.contains("wp-includes") {
            return Some("wordpress".to_string());
        }
        if p.contains("/sites/default") || p.contains("/sites/all") {
            return Some("drupal".to_string());
        }
        if p.contains("/admin/shopify") || p.contains("shopify") {
            return Some("shopify".to_string());
        }
        if p.contains("/administrator") && p.contains("joomla") {
            return Some("joomla".to_string());
        }
        if p.contains("/magento") || (p.contains("/pub/static") && p.contains("/pub/media")) {
            return Some("magento".to_string());
        }
        if p.contains("/ghost/") {
            return Some("ghost".to_string());
        }
        if p.contains("squarespace") {
            return Some("squarespace".to_string());
        }
    }
    None
}

/// Result of fetching page metadata for a single domain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageMetadataResult {
    pub domain: String,
    pub http_status: u16,
    pub page_title: Option<String>,
    pub meta_description: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_image: Option<String>,
    pub canonical_url: Option<String>,
    pub language: Option<String>,
    pub generator: Option<String>,
    pub snippet: Option<String>,
    pub content_type: Option<String>,
    pub fetched_at: i64,
    pub source: String,
    pub error: Option<String>,
}

/// Detect site category from sitemap URL samples
pub fn detect_site_category(sample_urls: &[String]) -> Option<String> {
    let combined: String = sample_urls.join("\n").to_lowercase();

    if combined.contains("/product") || combined.contains("/shop") || combined.contains("/store") {
        return Some("ecommerce".to_string());
    }
    if combined.contains("/blog") || combined.contains("/post") || combined.contains("/article") {
        return Some("blog".to_string());
    }
    if combined.contains("/news") || combined.contains("/press") {
        return Some("news".to_string());
    }
    if combined.contains("/portfolio") || combined.contains("/project") {
        return Some("portfolio".to_string());
    }
    None
}
