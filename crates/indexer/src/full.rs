use crate::progress::IndexProgress;
use anyhow::Result;
use domain_core::{domain::should_filter_domain, Config, Domain, DomainSchema};
use futures::StreamExt;
use std::path::Path;
use tantivy::Index;
use tracing::{debug, info, warn};
use word_client::WordClient;
use zonefile_client::{parser::batch_stream, DomainStream, ZonefileDownloader, ZonefileType};

/// Run full indexing with download from API
pub async fn run_with_download(
    config: &Config,
    output_path: &Path,
    heap_size: usize,
    commit_interval: usize,
) -> Result<()> {
    // Download the zonefile
    let downloader = ZonefileDownloader::new(
        &config.zonefile_api_url,
        &config.zonefile_token,
        std::env::temp_dir().join("zonefile-indexer"),
    )?;

    let input_path = downloader.download(ZonefileType::Full).await?;

    run(config, &input_path, output_path, heap_size, commit_interval).await
}

/// Run full indexing from a local file
pub async fn run(
    config: &Config,
    input_path: &Path,
    output_path: &Path,
    heap_size: usize,
    commit_interval: usize,
) -> Result<()> {
    info!("Starting full index build");
    info!(input = ?input_path, output = ?output_path);
    info!(heap_mb = heap_size / 1024 / 1024, commit_interval = commit_interval);

    // Count total domains for progress
    info!("Counting domains in file...");
    let total_count = DomainStream::count_file(input_path).await?;
    info!(total = total_count, "Total domains to index");

    // Create Tantivy index
    std::fs::create_dir_all(output_path)?;
    let schema = DomainSchema::new();
    let index = Index::create_in_dir(output_path, schema.schema.clone())?;
    let mut writer = index.writer(heap_size)?;

    // Create word client with parallel requests
    let word_client = WordClient::new(
        &config.word_splitter_url,
        &config.word_splitter_user,
        &config.word_splitter_pass,
        Some(config.word_batch_size),
        Some(4), // 4 parallel API requests
    )?;

    // Set up progress tracking
    let mut progress = IndexProgress::new(total_count);

    // Process domains in batches
    let domain_stream = DomainStream::from_file(input_path);
    let batched_stream = batch_stream(domain_stream, config.word_batch_size);

    futures::pin_mut!(batched_stream);

    let mut indexed_count: u64 = 0;
    let mut filtered_count: u64 = 0;
    let mut error_count: u64 = 0;
    let mut last_commit: u64 = 0;

    while let Some(batch_result) = batched_stream.next().await {
        let batch: Vec<String> = batch_result?;
        let batch_size = batch.len();

        // Normalize and filter domains
        let mut valid_domains: Vec<(String, domain_core::NormalizedDomain)> = Vec::new();
        let mut labels_to_segment: Vec<String> = Vec::new();

        for raw_domain in &batch {
            let domain = Domain::new(raw_domain);

            match domain.normalize() {
                Ok(normalized) => {
                    // Apply filtering rules
                    if should_filter_domain(&normalized.label) {
                        filtered_count += 1;
                        continue;
                    }

                    labels_to_segment.push(normalized.label.clone());
                    valid_domains.push((raw_domain.clone(), normalized));
                }
                Err(e) => {
                    debug!(domain = raw_domain, error = %e, "Failed to normalize domain");
                    error_count += 1;
                }
            }
        }

        // Segment labels in batch
        if !labels_to_segment.is_empty() {
            match word_client.segment_batch(labels_to_segment).await {
                Ok(segments) => {
                    // Match segments with domains by index
                    for (i, (_, tokens)) in segments.iter().enumerate() {
                        if i < valid_domains.len() {
                            valid_domains[i].1.tokens = tokens.clone();
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Word segmentation failed for batch, using empty tokens");
                    // Continue without tokens - domains will still be searchable by exact match
                }
            }
        }

        // Add documents to index
        for (_, normalized) in &valid_domains {
            let doc = schema.to_document(normalized);
            writer.add_document(doc)?;
            indexed_count += 1;
        }

        // Commit periodically
        if indexed_count - last_commit >= commit_interval as u64 {
            info!(indexed = indexed_count, "Committing checkpoint...");
            writer.commit()?;
            last_commit = indexed_count;
        }

        progress.inc(batch_size as u64);
    }

    // Final commit
    info!("Final commit...");
    writer.commit()?;

    progress.finish();

    info!(
        indexed = indexed_count,
        filtered = filtered_count,
        errors = error_count,
        "Indexing complete"
    );

    // Show final index size
    let mut total_size: u64 = 0;
    for entry in std::fs::read_dir(output_path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total_size += entry.metadata()?.len();
        }
    }
    info!(size_gb = total_size as f64 / 1024.0 / 1024.0 / 1024.0, "Index size");

    Ok(())
}
