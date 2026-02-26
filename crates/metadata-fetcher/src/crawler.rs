use crate::config::FetcherConfig;
use crate::fetcher::process_domain;
use crate::model::DomainMetadataResult;
use crate::progress::CrawlProgress;
use crate::store::MetadataStore;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{info, warn};

/// Run the metadata crawl over a list of domains.
pub async fn run_crawl(
    domains: Vec<String>,
    config: &FetcherConfig,
    store: MetadataStore,
    resume: bool,
) -> anyhow::Result<()> {
    let client = build_client(config)?;
    let client = Arc::new(client);
    let sem = Arc::new(Semaphore::new(config.concurrency));
    let store = Arc::new(store);

    // Shuffle domains to distribute load across hosting providers
    let mut domains = domains;
    {
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        domains.shuffle(&mut rng);
    }

    let total = domains.len() as u64;
    info!(total, concurrency = config.concurrency, "Starting crawl");

    store.update_state(total, 0).await?;

    let progress = CrawlProgress::new(total);
    let max_sitemap_bytes = config.max_sitemap_bytes;

    // mpsc channel for batch-flushing results to SQLite
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DomainMetadataResult>(config.concurrency * 2);

    let store_flush = Arc::clone(&store);
    let progress_flush = Arc::clone(&progress);
    let flush_task = tokio::spawn(async move {
        let mut buffer: Vec<DomainMetadataResult> = Vec::with_capacity(1000);
        let mut done: u64 = 0;

        while let Some(result) = rx.recv().await {
            buffer.push(result);
            done += 1;

            if buffer.len() >= 500 {
                let len = buffer.len() as u64;
                let batch = std::mem::replace(&mut buffer, Vec::with_capacity(1000));
                if let Err(e) = store_flush.upsert_batch(batch).await {
                    warn!(error = %e, "Failed to flush batch to SQLite");
                }
                progress_flush.inc(len);

                if done % 10_000 == 0 {
                    if let Err(e) = store_flush.update_state(0, done).await {
                        warn!(error = %e, "Failed to update crawl state");
                    }
                }
            }
        }

        // Flush remainder
        if !buffer.is_empty() {
            let len = buffer.len() as u64;
            if let Err(e) = store_flush.upsert_batch(buffer).await {
                warn!(error = %e, "Failed to flush final batch");
            }
            progress_flush.inc(len);
        }

        progress_flush.finish();
        done
    });

    // Crawl all domains with bounded concurrency
    stream::iter(domains)
        .for_each_concurrent(config.concurrency, |domain| {
            let client = Arc::clone(&client);
            let sem = Arc::clone(&sem);
            let store_check = Arc::clone(&store);
            let tx = tx.clone();

            async move {
                // Resume: skip already-crawled domains
                if resume {
                    match store_check.is_done(&domain).await {
                        Ok(true) => return,
                        Ok(false) => {}
                        Err(e) => {
                            warn!(domain = &domain, error = %e, "Failed to check done status");
                        }
                    }
                }

                let _permit = sem.acquire().await.expect("semaphore closed");
                let result = process_domain(&client, &domain, max_sitemap_bytes).await;

                if let Err(e) = tx.send(result).await {
                    warn!(domain = &domain, error = %e, "Failed to send result");
                }
            }
        })
        .await;

    drop(tx);

    let done = flush_task.await?;
    info!(done, "Crawl complete");

    Ok(())
}

fn build_client(config: &FetcherConfig) -> anyhow::Result<Client> {
    let client = Client::builder()
        .timeout(Duration::from_secs(config.request_timeout_secs))
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .pool_max_idle_per_host(1)
        .gzip(true)
        .redirect(reqwest::redirect::Policy::limited(3))
        .danger_accept_invalid_certs(true)
        .user_agent("Mozilla/5.0 (compatible; SitemapBot/1.0)")
        .build()?;
    Ok(client)
}
