use crate::config::FetcherConfig;
use crate::error::Result;
use crate::model::PageMetadataResult;
use crate::progress::CrawlProgress;
use crate::store::MetadataStore;
use crate::wat_parser::parse_wat_metadata;
use flate2::read::GzDecoder;
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;
use tokio::sync::Semaphore;
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
/// Returns the WARC filename + byte range if found.
async fn lookup_cdx(
    client: &reqwest::Client,
    domain: &str,
    cc_index: &str,
) -> Option<CdxRecord> {
    let url = format!(
        "{}/{}-index?url={}&output=json&filter=statuscode:200&limit=1",
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
        debug!(domain, status = %resp.status(), "CDX non-200 response");
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

    match serde_json::from_str::<CdxRecord>(first_line) {
        Ok(record) => Some(record),
        Err(e) => {
            debug!(domain, error = %e, "CDX JSON parse failed");
            None
        }
    }
}

/// Convert a WARC filename to its corresponding WAT filename.
/// e.g., crawl-data/CC-MAIN-.../warc/CC-MAIN-...-00000.warc.gz
///    -> crawl-data/CC-MAIN-.../wat/CC-MAIN-...-00000.warc.wat.gz
fn warc_to_wat_path(warc_filename: &str) -> String {
    warc_filename
        .replace("/warc/", "/wat/")
        .replace(".warc.gz", ".warc.wat.gz")
}

/// Fetch a WAT record from S3 using HTTP Range request, decompress gzip.
async fn fetch_wat_record(
    client: &reqwest::Client,
    warc_filename: &str,
    offset: u64,
    length: u64,
) -> Option<Vec<u8>> {
    let wat_path = warc_to_wat_path(warc_filename);
    let url = format!("{}/{}", S3_BASE_URL, wat_path);

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
            debug!(error = %e, "WAT S3 fetch failed");
            return None;
        }
    };

    if !resp.status().is_success() && resp.status().as_u16() != 206 {
        debug!(status = %resp.status(), "WAT S3 non-206 response");
        return None;
    }

    let compressed = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return None,
    };

    // Decompress gzip
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut decompressed = Vec::new();
    match decoder.read_to_end(&mut decompressed) {
        Ok(_) => Some(decompressed),
        Err(e) => {
            debug!(error = %e, "WAT gzip decompress failed");
            None
        }
    }
}

/// Extract the JSON payload from a raw WAT record.
/// WAT records have WARC headers followed by a blank line, then the JSON.
fn extract_json_from_wat(raw: &[u8]) -> Option<&[u8]> {
    // Find the first occurrence of "\r\n\r\n" or "\n\n" which separates headers from body
    // WAT records can have multiple WARC headers blocks; we need the JSON after the last one
    let raw_str = std::str::from_utf8(raw).ok()?;

    // WAT records typically have:
    // 1. WARC/1.0 header block (warc-type: metadata)
    // 2. blank line
    // 3. JSON payload
    // But some have nested WARC headers. Look for the JSON start.
    let json_start = raw_str.find('{')?;
    Some(raw[json_start..].as_ref())
}

/// Run the Common Crawl enrichment pipeline.
///
/// 1. Export failed domains from SQLite
/// 2. For each, query CDX API
/// 3. Fetch WAT records in parallel
/// 4. Parse metadata and upsert into SQLite
pub async fn run_commoncrawl_enrichment(
    config: &FetcherConfig,
    store: MetadataStore,
) -> Result<()> {
    info!(
        cc_index = %config.cc_index,
        concurrency = config.cc_wat_concurrency,
        cdx_delay_ms = config.cc_cdx_delay_ms,
        "Starting Common Crawl enrichment"
    );

    // 1. Export failed domains
    let failed_domains = store.export_failed_domains().await?;
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
    let sem = std::sync::Arc::new(Semaphore::new(config.cc_wat_concurrency));
    let cdx_delay = Duration::from_millis(config.cc_cdx_delay_ms);

    let mut found: u64 = 0;
    let mut enriched: u64 = 0;
    let mut no_cdx: u64 = 0;
    let mut wat_failed: u64 = 0;
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
                no_cdx += 1;
                progress.inc(1);
                continue;
            }
        };
        let length: u64 = match cdx.length.parse() {
            Ok(v) => v,
            Err(_) => {
                no_cdx += 1;
                progress.inc(1);
                continue;
            }
        };

        // WAT fetch (concurrent via semaphore)
        let _permit = sem.acquire().await.unwrap();
        let raw_wat = fetch_wat_record(&s3_client, &cdx.filename, offset, length).await;

        match raw_wat {
            Some(raw) => {
                // Extract JSON from WAT record
                if let Some(json_bytes) = extract_json_from_wat(&raw) {
                    let meta = parse_wat_metadata(domain, json_bytes);
                    if meta.page_title.is_some() {
                        enriched += 1;
                    }
                    batch.push(meta);
                } else {
                    wat_failed += 1;
                    debug!(domain, "No JSON found in WAT record");
                }
            }
            None => {
                wat_failed += 1;
            }
        }

        progress.inc(1);

        // Flush batch
        if batch.len() >= batch_size {
            store.upsert_page_batch(batch.clone()).await?;
            batch.clear();
        }

        // Log progress periodically
        if (i + 1) % 10_000 == 0 {
            info!(
                processed = i + 1,
                total,
                found,
                enriched,
                no_cdx,
                wat_failed,
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
        wat_failed,
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
