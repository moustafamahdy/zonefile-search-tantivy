use crate::progress::IndexProgress;
use anyhow::Result;
use domain_core::{domain::should_filter_domain, Config, Domain, DomainSchema};
use futures::StreamExt;
use std::path::Path;
use tantivy::{Index, Term};
use tracing::{debug, info, warn};
use word_client::WordClient;
use zonefile_client::{parser::batch_stream, DomainStream, ZonefileDownloader, ZonefileType};

/// Run daily sync with download from API
pub async fn run_with_download(config: &Config, index_path: &Path) -> Result<()> {
    let downloader = ZonefileDownloader::new(
        &config.zonefile_api_url,
        &config.zonefile_token,
        std::env::temp_dir().join("zonefile-indexer"),
    )?;

    // Download both files
    info!("Downloading daily update file...");
    let adds_path = downloader.download(ZonefileType::DailyUpdate).await?;

    info!("Downloading daily remove file...");
    let removes_path = downloader.download(ZonefileType::DailyRemove).await?;

    run(config, Some(adds_path), Some(removes_path), index_path).await
}

/// Run daily sync from local files
pub async fn run(
    config: &Config,
    adds_path: Option<impl AsRef<Path>>,
    removes_path: Option<impl AsRef<Path>>,
    index_path: &Path,
) -> Result<()> {
    info!("Starting daily sync");

    // Open existing index
    let schema = DomainSchema::new();
    let index = Index::open_in_dir(index_path)?;
    let reader = index.reader()?;
    let initial_count = reader.searcher().num_docs();

    info!(documents = initial_count, "Current index size");

    let mut writer = index.writer(500 * 1024 * 1024)?; // 500MB heap for daily updates

    let word_client = WordClient::new(
        &config.word_splitter_url,
        &config.word_splitter_user,
        &config.word_splitter_pass,
        Some(config.word_batch_size),
        Some(4), // 4 parallel API requests
    )?;

    let mut total_deleted: u64 = 0;
    let mut total_added: u64 = 0;

    // Process removals first
    if let Some(removes_path) = removes_path {
        let removes_path = removes_path.as_ref();
        if removes_path.exists() {
            info!(path = ?removes_path, "Processing removals...");
            total_deleted = process_removals(&schema, &mut writer, removes_path).await?;
            info!(deleted = total_deleted, "Removals complete");
        }
    }

    // Process additions
    if let Some(adds_path) = adds_path {
        let adds_path = adds_path.as_ref();
        if adds_path.exists() {
            info!(path = ?adds_path, "Processing additions...");
            total_added = process_additions(config, &schema, &word_client, &mut writer, adds_path).await?;
            info!(added = total_added, "Additions complete");
        }
    }

    // Commit changes
    info!("Committing changes...");
    writer.commit()?;

    // Reload reader to get updated count
    let reader = index.reader()?;
    let final_count = reader.searcher().num_docs();

    info!(
        initial = initial_count,
        deleted = total_deleted,
        added = total_added,
        final_count = final_count,
        net_change = final_count as i64 - initial_count as i64,
        "Daily sync complete"
    );

    Ok(())
}

async fn process_removals(
    schema: &DomainSchema,
    writer: &mut tantivy::IndexWriter,
    removes_path: &Path,
) -> Result<u64> {
    let domain_stream = DomainStream::from_file(removes_path);
    let batched = batch_stream(domain_stream, 10_000); // Smaller batches for deletes

    futures::pin_mut!(batched);

    let mut progress = IndexProgress::spinner();
    let mut deleted: u64 = 0;

    while let Some(batch_result) = batched.next().await {
        let batch: Vec<String> = batch_result?;

        for raw_domain in batch {
            let domain = Domain::new(&raw_domain);

            match domain.normalize() {
                Ok(normalized) => {
                    // Delete by domain_exact term
                    let term = Term::from_field_text(schema.domain_exact, &normalized.domain_exact);
                    writer.delete_term(term);
                    deleted += 1;
                }
                Err(e) => {
                    debug!(domain = raw_domain, error = %e, "Failed to normalize for deletion");
                }
            }
        }

        progress.inc(deleted - progress.count());
    }

    progress.finish();
    Ok(deleted)
}

async fn process_additions(
    config: &Config,
    schema: &DomainSchema,
    word_client: &WordClient,
    writer: &mut tantivy::IndexWriter,
    adds_path: &Path,
) -> Result<u64> {
    let domain_stream = DomainStream::from_file(adds_path);
    let batched = batch_stream(domain_stream, config.word_batch_size);

    futures::pin_mut!(batched);

    let mut progress = IndexProgress::spinner();
    let mut added: u64 = 0;
    let mut filtered: u64 = 0;

    while let Some(batch_result) = batched.next().await {
        let batch: Vec<String> = batch_result?;
        let batch_size = batch.len();

        // Normalize and filter
        let mut valid_domains: Vec<domain_core::NormalizedDomain> = Vec::new();
        let mut labels_to_segment: Vec<String> = Vec::new();

        for raw_domain in &batch {
            let domain = Domain::new(raw_domain);

            match domain.normalize() {
                Ok(normalized) => {
                    if should_filter_domain(&normalized.label) {
                        filtered += 1;
                        continue;
                    }

                    labels_to_segment.push(normalized.label.clone());
                    valid_domains.push(normalized);
                }
                Err(e) => {
                    debug!(domain = raw_domain, error = %e, "Failed to normalize");
                }
            }
        }

        // Segment labels
        if !labels_to_segment.is_empty() {
            match word_client.segment_batch(labels_to_segment).await {
                Ok(segments) => {
                    for (normalized, (_, tokens)) in valid_domains.iter_mut().zip(segments.iter()) {
                        normalized.tokens = tokens.clone();
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Word segmentation failed, using empty tokens");
                }
            }
        }

        // Add to index
        for normalized in &valid_domains {
            // Delete existing document first (in case it's a re-add)
            let term = Term::from_field_text(schema.domain_exact, &normalized.domain_exact);
            writer.delete_term(term);

            // Add new document
            let doc = schema.to_document(normalized);
            writer.add_document(doc)?;
            added += 1;
        }

        progress.inc(batch_size as u64);
    }

    progress.finish();

    if filtered > 0 {
        info!(filtered = filtered, "Domains filtered during addition");
    }

    Ok(added)
}
