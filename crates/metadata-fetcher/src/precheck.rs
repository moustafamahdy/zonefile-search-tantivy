use crate::progress::CrawlProgress;
use futures::stream::StreamExt;
use reqwest::Client;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Known parking/domain-sale hosts
const PARKING_HOSTS: &[&str] = &[
    "sedo.com",
    "sedoparking.com",
    "hugedomains.com",
    "brandsly.com",
    "afternic.com",
    "dan.com",
    "bodis.com",
    "domains.atom.com",
    "undeveloped.com",
    "parkingcrew.net",
    "domainmarket.com",
];

#[derive(Debug)]
#[allow(dead_code)]
pub enum FilterReason {
    DnsFailure,
    SslError,
    ConnectionRefused,
    Timeout,
    Parking(String),
    HttpError(u16),
}

impl std::fmt::Display for FilterReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterReason::DnsFailure => write!(f, "dns_failure"),
            FilterReason::SslError => write!(f, "ssl_error"),
            FilterReason::ConnectionRefused => write!(f, "connection_refused"),
            FilterReason::Timeout => write!(f, "timeout"),
            FilterReason::Parking(host) => write!(f, "parking:{}", host),
            FilterReason::HttpError(code) => write!(f, "http_{}", code),
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum PreCheckResult {
    Reachable {
        domain: String,
        status: u16,
    },
    Filtered {
        domain: String,
        reason: FilterReason,
    },
}

pub struct PreCheckSummary {
    pub total: u64,
    pub reachable: u64,
    pub dns_failures: u64,
    pub ssl_errors: u64,
    pub connection_refused: u64,
    pub timeouts: u64,
    pub parking: u64,
    pub http_errors: u64,
}

/// Perform a HEAD check on a single domain.
/// Tries HTTPS first, then HTTP. Uses `redirect: manual` to inspect redirect targets.
async fn head_check_single(client: &Client, domain: &str) -> PreCheckResult {
    for scheme in &["https", "http"] {
        let url = format!("{}://{}/", scheme, domain);

        match client.head(&url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();

                // Check for parking redirects (3xx with Location header)
                if resp.status().is_redirection() {
                    if let Some(location) = resp.headers().get("location") {
                        if let Ok(loc_str) = location.to_str() {
                            if let Some(parking_host) = detect_parking(domain, loc_str) {
                                return PreCheckResult::Filtered {
                                    domain: domain.to_string(),
                                    reason: FilterReason::Parking(parking_host),
                                };
                            }
                        }
                    }
                }

                // 4xx/5xx errors that indicate the domain is not useful
                if status == 403 || status == 404 || status == 410 || status == 451 || status >= 500
                {
                    // Still "reachable" — the server responded, it might work with
                    // a real GET or via Scrapling. Let it through.
                    return PreCheckResult::Reachable {
                        domain: domain.to_string(),
                        status,
                    };
                }

                // Any successful or redirect response = reachable
                return PreCheckResult::Reachable {
                    domain: domain.to_string(),
                    status,
                };
            }
            Err(e) => {
                debug!(domain, scheme, error = %e, "HEAD check failed");

                // On HTTPS failure, try HTTP before giving up
                if *scheme == "https" {
                    continue;
                }

                // Both schemes failed — classify the error
                return PreCheckResult::Filtered {
                    domain: domain.to_string(),
                    reason: classify_error(&e),
                };
            }
        }
    }

    // Should not reach here, but just in case
    PreCheckResult::Filtered {
        domain: domain.to_string(),
        reason: FilterReason::ConnectionRefused,
    }
}

fn classify_error(error: &reqwest::Error) -> FilterReason {
    let msg = error.to_string().to_lowercase();

    if error.is_timeout() {
        FilterReason::Timeout
    } else if msg.contains("dns") || msg.contains("resolve") || msg.contains("no such host") {
        FilterReason::DnsFailure
    } else if msg.contains("ssl") || msg.contains("tls") || msg.contains("certificate") {
        FilterReason::SslError
    } else if msg.contains("connection refused") || msg.contains("connection reset") {
        FilterReason::ConnectionRefused
    } else {
        // Default to connection_refused for other network errors
        FilterReason::ConnectionRefused
    }
}

/// Check if a redirect location points to a known parking/domain-sale host.
fn detect_parking(domain: &str, location: &str) -> Option<String> {
    // Extract host from Location header value
    let loc_lower = location.to_lowercase();
    let host = extract_host_from_url(&loc_lower).unwrap_or_default();

    if host.is_empty() {
        return None;
    }

    // Don't flag self-redirects (e.g., example.com -> www.example.com)
    if host == domain || host.ends_with(&format!(".{}", domain)) {
        return None;
    }

    for parking in PARKING_HOSTS {
        if host == *parking || host.ends_with(&format!(".{}", parking)) {
            return Some(parking.to_string());
        }
    }

    None
}

/// Extract host from a URL string without the `url` crate.
/// Handles "https://host/path" and "//host/path" patterns.
fn extract_host_from_url(url: &str) -> Option<String> {
    let after_scheme = if let Some(pos) = url.find("://") {
        &url[pos + 3..]
    } else if url.starts_with("//") {
        &url[2..]
    } else {
        // Relative URL — no host to extract
        return None;
    };

    // Take everything before the first '/' or '?' or ':'
    let host = after_scheme
        .split(|c: char| c == '/' || c == '?' || c == ':')
        .next()
        .unwrap_or("");

    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Run pre-check on all domains in the input file.
/// Writes reachable domains to output_file, optionally writes filtered domains to filtered_file.
pub async fn run_precheck(
    domains_file: &Path,
    output_file: &Path,
    filtered_file: Option<&Path>,
    concurrency: usize,
    timeout_secs: u64,
) -> anyhow::Result<PreCheckSummary> {
    let client = build_precheck_client(timeout_secs)?;
    let client = Arc::new(client);
    let sem = Arc::new(Semaphore::new(concurrency));

    // Count lines for progress
    let total = count_lines(domains_file).await?;
    info!(total, concurrency, timeout_secs, "Starting HEAD pre-check");

    let progress = CrawlProgress::new(total);

    // Atomic counters for summary
    let reachable_count = Arc::new(AtomicU64::new(0));
    let dns_failures = Arc::new(AtomicU64::new(0));
    let ssl_errors = Arc::new(AtomicU64::new(0));
    let conn_refused = Arc::new(AtomicU64::new(0));
    let timeouts = Arc::new(AtomicU64::new(0));
    let parking_count = Arc::new(AtomicU64::new(0));
    let http_errors = Arc::new(AtomicU64::new(0));

    // Channel for results
    let (tx, mut rx) = tokio::sync::mpsc::channel::<PreCheckResult>(concurrency * 2);

    // Writer task: flush reachable/filtered domains to files
    let output_path = output_file.to_path_buf();
    let filtered_path = filtered_file.map(|p| p.to_path_buf());
    let progress_writer = Arc::clone(&progress);
    let rc = Arc::clone(&reachable_count);
    let df = Arc::clone(&dns_failures);
    let se = Arc::clone(&ssl_errors);
    let cr = Arc::clone(&conn_refused);
    let to = Arc::clone(&timeouts);
    let pk = Arc::clone(&parking_count);
    let he = Arc::clone(&http_errors);

    let writer_task = tokio::spawn(async move {
        let out_file = tokio::fs::File::create(&output_path).await.unwrap();
        let mut out_writer = BufWriter::new(out_file);

        let mut filtered_writer = if let Some(ref fp) = filtered_path {
            let f = tokio::fs::File::create(fp).await.unwrap();
            Some(BufWriter::new(f))
        } else {
            None
        };

        let mut count: u64 = 0;

        while let Some(result) = rx.recv().await {
            count += 1;

            match result {
                PreCheckResult::Reachable { ref domain, .. } => {
                    rc.fetch_add(1, Ordering::Relaxed);
                    let _ = out_writer.write_all(domain.as_bytes()).await;
                    let _ = out_writer.write_all(b"\n").await;
                }
                PreCheckResult::Filtered {
                    ref domain,
                    ref reason,
                } => {
                    match reason {
                        FilterReason::DnsFailure => {
                            df.fetch_add(1, Ordering::Relaxed);
                        }
                        FilterReason::SslError => {
                            se.fetch_add(1, Ordering::Relaxed);
                        }
                        FilterReason::ConnectionRefused => {
                            cr.fetch_add(1, Ordering::Relaxed);
                        }
                        FilterReason::Timeout => {
                            to.fetch_add(1, Ordering::Relaxed);
                        }
                        FilterReason::Parking(_) => {
                            pk.fetch_add(1, Ordering::Relaxed);
                        }
                        FilterReason::HttpError(_) => {
                            he.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if let Some(ref mut fw) = filtered_writer {
                        let line = format!("{}\t{}\n", domain, reason);
                        let _ = fw.write_all(line.as_bytes()).await;
                    }
                }
            }

            if count % 500 == 0 {
                let _ = out_writer.flush().await;
                if let Some(ref mut fw) = filtered_writer {
                    let _ = fw.flush().await;
                }
                progress_writer.inc(500);
            }
        }

        // Flush remainder
        let remainder = count % 500;
        if remainder > 0 {
            progress_writer.inc(remainder);
        }
        let _ = out_writer.flush().await;
        if let Some(ref mut fw) = filtered_writer {
            let _ = fw.flush().await;
        }

        progress_writer.finish();
        count
    });

    // Stream domains from file and process concurrently
    let file = tokio::fs::File::open(domains_file).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let chunk_size = 100_000usize;
    let mut chunk: Vec<String> = Vec::with_capacity(chunk_size);

    loop {
        chunk.clear();
        for _ in 0..chunk_size {
            match lines.next_line().await? {
                Some(line) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        chunk.push(trimmed);
                    }
                }
                None => break,
            }
        }

        if chunk.is_empty() {
            break;
        }

        let domains_batch = std::mem::replace(&mut chunk, Vec::with_capacity(chunk_size));

        futures::stream::iter(domains_batch)
            .for_each_concurrent(concurrency, |domain| {
                let client = Arc::clone(&client);
                let sem = Arc::clone(&sem);
                let tx = tx.clone();

                async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    let result = head_check_single(&client, &domain).await;

                    if let Err(e) = tx.send(result).await {
                        warn!(domain = &domain, error = %e, "Failed to send precheck result");
                    }
                }
            })
            .await;
    }

    drop(tx);

    let processed = writer_task.await?;
    info!(processed, "Pre-check complete");

    let summary = PreCheckSummary {
        total: processed,
        reachable: reachable_count.load(Ordering::Relaxed),
        dns_failures: dns_failures.load(Ordering::Relaxed),
        ssl_errors: ssl_errors.load(Ordering::Relaxed),
        connection_refused: conn_refused.load(Ordering::Relaxed),
        timeouts: timeouts.load(Ordering::Relaxed),
        parking: parking_count.load(Ordering::Relaxed),
        http_errors: http_errors.load(Ordering::Relaxed),
    };

    info!(
        reachable = summary.reachable,
        dns_failures = summary.dns_failures,
        ssl_errors = summary.ssl_errors,
        connection_refused = summary.connection_refused,
        timeouts = summary.timeouts,
        parking = summary.parking,
        http_errors = summary.http_errors,
        "Pre-check summary"
    );

    Ok(summary)
}

fn build_precheck_client(timeout_secs: u64) -> anyhow::Result<Client> {
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(true)
        .user_agent(CHROME_UA)
        .build()?;
    Ok(client)
}

async fn count_lines(path: &Path) -> anyhow::Result<u64> {
    let file = tokio::fs::File::open(path).await?;
    let reader = BufReader::with_capacity(256 * 1024, file);
    let mut lines = reader.lines();
    let mut count: u64 = 0;
    while lines.next_line().await?.is_some() {
        count += 1;
    }
    Ok(count)
}
