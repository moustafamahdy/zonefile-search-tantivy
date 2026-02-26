use reqwest::Client;
use texting_robots::Robot;
use tracing::debug;

/// Parsed information from a robots.txt file
#[derive(Debug, Default)]
pub struct RobotsInfo {
    pub status: u16,
    pub found: bool,
    pub sitemap_urls: Vec<String>,
    pub disallow_paths: Vec<String>,
    pub crawl_delay: Option<f32>,
}

/// Fetch and parse robots.txt for a domain.
/// Tries HTTPS first, then HTTP. Returns RobotsInfo with status 0 on connection failure.
pub async fn fetch_robots(client: &Client, domain: &str) -> RobotsInfo {
    for scheme in &["https", "http"] {
        let url = format!("{}://{}/robots.txt", scheme, domain);

        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();

                if resp.status().is_success() {
                    match resp.bytes().await {
                        Ok(bytes) => {
                            if bytes.len() > 512 * 1024 {
                                // Cap at 512KB
                                let body = String::from_utf8_lossy(&bytes[..512 * 1024]);
                                return parse_robots_body(&body, status);
                            }
                            let body = String::from_utf8_lossy(&bytes);
                            return parse_robots_body(&body, status);
                        }
                        Err(e) => {
                            debug!(domain, scheme, error = %e, "Failed to read robots.txt body");
                            return RobotsInfo {
                                status,
                                ..Default::default()
                            };
                        }
                    }
                } else if status == 404 || status == 403 || status == 410 {
                    return RobotsInfo {
                        status,
                        ..Default::default()
                    };
                } else {
                    continue;
                }
            }
            Err(_) => continue,
        }
    }

    RobotsInfo::default()
}

fn parse_robots_body(body: &str, status: u16) -> RobotsInfo {
    // Manually extract Disallow paths for CMS detection
    let disallow_paths = extract_disallow_paths(body);

    // Use texting_robots for structured data (sitemaps, crawl-delay)
    match Robot::new("*", body.as_bytes()) {
        Ok(robot) => {
            let sitemap_urls: Vec<String> =
                robot.sitemaps.iter().map(|s| s.to_string()).collect();
            let crawl_delay = robot.delay.map(|d| d as f32);

            RobotsInfo {
                status,
                found: true,
                sitemap_urls,
                disallow_paths,
                crawl_delay,
            }
        }
        Err(_) => {
            // Parsing failed — still extract what we can from raw text
            let sitemap_urls = extract_sitemap_urls(body);
            RobotsInfo {
                status,
                found: true,
                sitemap_urls,
                disallow_paths,
                crawl_delay: None,
            }
        }
    }
}

/// Extract Disallow paths from raw robots.txt text
fn extract_disallow_paths(body: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Disallow:") {
            let path = rest.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        }
    }
    paths
}

/// Fallback: extract Sitemap URLs from raw robots.txt text
fn extract_sitemap_urls(body: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Sitemap:") {
            let url = rest.trim();
            if !url.is_empty() {
                urls.push(url.to_string());
            }
        }
    }
    urls
}
