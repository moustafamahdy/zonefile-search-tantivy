use crate::config::FetcherConfig;
use crate::error::Result;
use crate::model::PageMetadataResult;
use crate::progress::CrawlProgress;
use crate::store::MetadataStore;
use flate2::read::GzDecoder;
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;
use tracing::{debug, info, warn};

const CDX_BASE_URL: &str = "https://index.commoncrawl.org";
const S3_BASE_URL: &str = "https://data.commoncrawl.org";

/// CDX API response record
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CdxRecord {
    url: String,
    filename: String,
    offset: String,
    length: String,
    #[serde(default)]
    status: String,
}

/// Query CDX API for a single domain's best capture.
async fn lookup_cdx(
    client: &reqwest::Client,
    domain: &str,
    cc_index: &str,
) -> Option<CdxRecord> {
    let url = format!(
        "{}/{}-index?url=https://{}/*&output=json&filter=statuscode:200&limit=1",
        CDX_BASE_URL, cc_index, domain
    );

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            debug!(domain, error = %e, "CDX lookup failed");
            return None;
        }
    };

    if !resp.status().is_success() {
        return None;
    }

    let body = match resp.text().await {
        Ok(b) => b,
        Err(_) => return None,
    };

    // CDX returns one JSON object per line (NDJSON), take the first line
    let first_line = body.lines().next()?;
    if first_line.is_empty() {
        return None;
    }

    serde_json::from_str::<CdxRecord>(first_line).ok()
}

/// Fetch a WARC record from S3 using HTTP Range request, decompress gzip,
/// and extract the HTTP response body (HTML).
async fn fetch_warc_html(
    client: &reqwest::Client,
    warc_filename: &str,
    offset: u64,
    length: u64,
) -> Option<String> {
    let url = format!("{}/{}", S3_BASE_URL, warc_filename);
    let end = offset + length - 1;
    let range = format!("bytes={}-{}", offset, end);

    let resp = match client
        .get(&url)
        .header("Range", &range)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            debug!(error = %e, "WARC S3 fetch failed");
            return None;
        }
    };

    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        return None;
    }

    let compressed = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return None,
    };

    // Decompress gzip
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut decompressed = Vec::new();
    if decoder.read_to_end(&mut decompressed).is_err() {
        return None;
    }

    let raw = String::from_utf8_lossy(&decompressed);

    // WARC record structure:
    // WARC/1.0 headers\r\n\r\n
    // HTTP/1.1 200 OK\r\nheaders\r\n\r\n
    // <html>...</html>
    //
    // Find the HTTP response body by looking for double CRLF after HTTP headers
    let warc_str = raw.as_ref();

    // Skip WARC header block (first \r\n\r\n)
    let after_warc = warc_str.find("\r\n\r\n").map(|i| &warc_str[i + 4..])?;

    // Skip HTTP header block (second \r\n\r\n)
    let html_start = after_warc.find("\r\n\r\n").map(|i| &after_warc[i + 4..])?;

    // Limit to ~200KB to avoid huge pages
    let html = if html_start.len() > 200_000 {
        &html_start[..200_000]
    } else {
        html_start
    };

    Some(html.to_string())
}

/// Parse HTML to extract page metadata using the scraper crate.
fn parse_html_metadata(domain: &str, html: &str) -> PageMetadataResult {
    use scraper::{Html, Selector};

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut result = PageMetadataResult {
        domain: domain.to_string(),
        http_status: 200,
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
        fetched_at: now,
        source: "commoncrawl".to_string(),
        error: None,
    };

    let document = Html::parse_document(html);

    // Title
    if let Ok(sel) = Selector::parse("title") {
        if let Some(el) = document.select(&sel).next() {
            let title = el.text().collect::<String>();
            let title = title.trim();
            if !title.is_empty() {
                result.page_title = Some(title[..title.len().min(500)].to_string());
            }
        }
    }

    // Meta description
    if let Ok(sel) = Selector::parse(r#"meta[name="description"]"#) {
        if let Some(el) = document.select(&sel).next() {
            if let Some(content) = el.value().attr("content") {
                let content = content.trim();
                if !content.is_empty() {
                    result.meta_description = Some(content[..content.len().min(1000)].to_string());
                }
            }
        }
    }

    // OG title
    if let Ok(sel) = Selector::parse(r#"meta[property="og:title"]"#) {
        if let Some(el) = document.select(&sel).next() {
            if let Some(content) = el.value().attr("content") {
                let content = content.trim();
                if !content.is_empty() {
                    result.og_title = Some(content[..content.len().min(500)].to_string());
                }
            }
        }
    }

    // OG description
    if let Ok(sel) = Selector::parse(r#"meta[property="og:description"]"#) {
        if let Some(el) = document.select(&sel).next() {
            if let Some(content) = el.value().attr("content") {
                let content = content.trim();
                if !content.is_empty() {
                    result.og_description = Some(content[..content.len().min(1000)].to_string());
                }
            }
        }
    }

    // OG image
    if let Ok(sel) = Selector::parse(r#"meta[property="og:image"]"#) {
        if let Some(el) = document.select(&sel).next() {
            if let Some(content) = el.value().attr("content") {
                let content = content.trim();
                if !content.is_empty() {
                    result.og_image = Some(content[..content.len().min(2000)].to_string());
                }
            }
        }
    }

    // Canonical URL
    if let Ok(sel) = Selector::parse(r#"link[rel="canonical"]"#) {
        if let Some(el) = document.select(&sel).next() {
            if let Some(href) = el.value().attr("href") {
                let href = href.trim();
                if !href.is_empty() {
                    result.canonical_url = Some(href[..href.len().min(2000)].to_string());
                }
            }
        }
    }

    // Language from html tag
    if let Ok(sel) = Selector::parse("html") {
        if let Some(el) = document.select(&sel).next() {
            if let Some(lang) = el.value().attr("lang") {
                let lang = lang.split(['-', '_']).next().unwrap_or(lang).trim();
                if !lang.is_empty() {
                    result.language = Some(lang.to_lowercase());
                }
            }
        }
    }

    // Language from meta tag (fallback)
    if result.language.is_none() {
        if let Ok(sel) = Selector::parse(r#"meta[http-equiv="content-language"]"#) {
            if let Some(el) = document.select(&sel).next() {
                if let Some(content) = el.value().attr("content") {
                    let lang = content.split(['-', '_']).next().unwrap_or(content).trim();
                    if !lang.is_empty() {
                        result.language = Some(lang.to_lowercase());
                    }
                }
            }
        }
    }

    // Generator
    if let Ok(sel) = Selector::parse(r#"meta[name="generator"]"#) {
        if let Some(el) = document.select(&sel).next() {
            if let Some(content) = el.value().attr("content") {
                let content = content.trim();
                if !content.is_empty() {
                    result.generator = Some(content[..content.len().min(200)].to_string());
                }
            }
        }
    }

    // Snippet (prefer description, fallback to og:description)
    result.snippet = result.meta_description.clone().or_else(|| result.og_description.clone());

    result
}

/// Run the Common Crawl enrichment pipeline.
///
/// 1. Export failed domains from SQLite
/// 2. For each, query CDX API for a cached copy
/// 3. Fetch WARC record from S3, decompress, parse HTML
/// 4. Upsert metadata into SQLite
pub async fn run_commoncrawl_enrichment(
    config: &FetcherConfig,
    store: MetadataStore,
) -> Result<()> {
    info!(
        cc_index = %config.cc_index,
        concurrency = config.cc_wat_concurrency,
        cdx_delay_ms = config.cc_cdx_delay_ms,
        http_only = config.cc_http_only,
        "Starting Common Crawl enrichment"
    );

    // 1. Export failed domains
    let failed_domains = store.export_failed_domains(config.cc_http_only).await?;
    let total = failed_domains.len();
    info!(total, "Failed domains exported for CC enrichment");

    if total == 0 {
        info!("No failed domains to enrich");
        return Ok(());
    }

    // 2. Build HTTP clients
    let cdx_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (compatible; DomainMetadataBot/1.0)")
        .build()
        .map_err(|e| crate::error::Error::Config(format!("Failed to build CDX client: {}", e)))?;

    let s3_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| crate::error::Error::Config(format!("Failed to build S3 client: {}", e)))?;

    let progress = CrawlProgress::new(total as u64);
    let cdx_delay = Duration::from_millis(config.cc_cdx_delay_ms);

    let mut found: u64 = 0;
    let mut enriched: u64 = 0;
    let mut no_cdx: u64 = 0;
    let mut fetch_failed: u64 = 0;
    let mut batch: Vec<PageMetadataResult> = Vec::new();
    let batch_size = 500;

    // 3. Process domains
    for (i, domain) in failed_domains.iter().enumerate() {
        // CDX lookup (serial with delay to avoid rate limiting)
        let cdx_record = lookup_cdx(&cdx_client, domain, &config.cc_index).await;

        if cdx_delay.as_millis() > 0 {
            tokio::time::sleep(cdx_delay).await;
        }

        let cdx = match cdx_record {
            Some(r) => {
                found += 1;
                r
            }
            None => {
                no_cdx += 1;
                progress.inc(1);
                continue;
            }
        };

        // Parse offset and length
        let offset: u64 = match cdx.offset.parse() {
            Ok(v) => v,
            Err(_) => {
                fetch_failed += 1;
                progress.inc(1);
                continue;
            }
        };
        let length: u64 = match cdx.length.parse() {
            Ok(v) => v,
            Err(_) => {
                fetch_failed += 1;
                progress.inc(1);
                continue;
            }
        };

        // Fetch WARC record and extract HTML
        match fetch_warc_html(&s3_client, &cdx.filename, offset, length).await {
            Some(html) => {
                let meta = parse_html_metadata(domain, &html);
                if meta.page_title.is_some() {
                    enriched += 1;
                }
                batch.push(meta);
            }
            None => {
                fetch_failed += 1;
            }
        }

        progress.inc(1);

        // Flush batch
        if batch.len() >= batch_size {
            store.upsert_page_batch(batch.clone()).await?;
            batch.clear();
        }

        // Log progress periodically
        if (i + 1) % 1_000 == 0 {
            info!(
                processed = i + 1,
                total,
                found,
                enriched,
                no_cdx,
                fetch_failed,
                "CC enrichment progress"
            );
        }
    }

    // Final flush
    if !batch.is_empty() {
        store.upsert_page_batch(batch).await?;
    }

    progress.finish();
    info!(
        total,
        found,
        enriched,
        no_cdx,
        fetch_failed,
        "Common Crawl enrichment complete"
    );

    Ok(())
}

/// Import JSONL file into page_metadata table.
/// Each line is a JSON object with the same fields as PageMetadataResult.
pub async fn import_jsonl(
    jsonl_path: &std::path::Path,
    store: &MetadataStore,
    source_override: Option<&str>,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    info!(path = ?jsonl_path, "Importing JSONL metadata");

    let file = tokio::fs::File::open(jsonl_path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut batch: Vec<PageMetadataResult> = Vec::new();
    let mut total = 0u64;
    let mut errors = 0u64;
    let batch_size = 500;

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        match serde_json::from_str::<PageMetadataResult>(&line) {
            Ok(mut meta) => {
                if let Some(src) = source_override {
                    meta.source = src.to_string();
                }
                batch.push(meta);
                total += 1;
            }
            Err(e) => {
                errors += 1;
                if errors <= 10 {
                    warn!(error = %e, "Failed to parse JSONL line");
                }
            }
        }

        if batch.len() >= batch_size {
            store.upsert_page_batch(batch.clone()).await?;
            batch.clear();
        }

        if total % 10_000 == 0 && total > 0 {
            info!(total, errors, "JSONL import progress");
        }
    }

    if !batch.is_empty() {
        store.upsert_page_batch(batch).await?;
    }

    info!(total, errors, "JSONL import complete");
    Ok(())
}
