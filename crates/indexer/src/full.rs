use crate::progress::IndexProgress;
use anyhow::Result;
use domain_core::{
    domain::should_filter_domain, register_ngram_tokenizer, Config, DetailedRecord, Domain,
    DomainSchema,
};
use futures::StreamExt;
use std::path::Path;
use tantivy::Index;
use tracing::{debug, info, warn};
use word_client::WordClient;
use zonefile_client::{
    batch_stream, batch_stream_detailed, DetailedDomainStream, DomainStream, ZonefileDownloader,
    ZonefileType,
};

/// Run full indexing with download from API.
///
/// `prefer_detailed` is the operator preference (CLI flag / `DETAILED_MODE`
/// env var). The actual mode is resolved against the plan via
/// [`crate::mode::resolve_mode_via_downloader`] before download begins.
pub async fn run_with_download(
    config: &Config,
    output_path: &Path,
    heap_size: usize,
    commit_interval: usize,
    prefer_detailed: bool,
    no_word_segment: bool,
) -> Result<()> {
    let downloader = ZonefileDownloader::new(
        &config.zonefile_api_url,
        &config.zonefile_token,
        std::env::temp_dir().join("zonefile-indexer"),
    )?;

    // Resolve mode against the live plan. May log [MODE] info/warn lines.
    let detailed = crate::mode::resolve_mode_via_downloader(prefer_detailed, &downloader).await?;

    let zonefile_type = if detailed {
        ZonefileType::DetailedFull
    } else {
        ZonefileType::Full
    };

    let input_path = downloader.download(zonefile_type).await?;

    run(config, &input_path, output_path, heap_size, commit_interval, detailed, no_word_segment).await
}

/// Run full indexing from a local file
pub async fn run(
    config: &Config,
    input_path: &Path,
    output_path: &Path,
    heap_size: usize,
    commit_interval: usize,
    detailed: bool,
    no_word_segment: bool,
) -> Result<()> {
    info!("Starting full index build");
    info!(input = ?input_path, output = ?output_path, detailed = detailed);
    info!(heap_mb = heap_size / 1024 / 1024, commit_interval = commit_interval);

    // Count total domains for progress
    info!("Counting domains in file...");
    let total_count = if detailed {
        DetailedDomainStream::count_file(input_path).await?
    } else {
        DomainStream::count_file(input_path).await?
    };
    info!(total = total_count, "Total domains to index");

    // Create Tantivy index
    std::fs::create_dir_all(output_path)?;
    let schema = DomainSchema::new();
    let index = Index::create_in_dir(output_path, schema.schema.clone())?;
    register_ngram_tokenizer(&index);
    let mut writer = index.writer(heap_size)?;

    // Create word client with parallel requests (only if word segmentation is enabled)
    let word_client = if no_word_segment {
        info!("Word segmentation disabled (--no-word-segment)");
        None
    } else {
        Some(WordClient::new(
            &config.word_splitter_url,
            &config.word_splitter_user,
            &config.word_splitter_pass,
            Some(config.word_batch_size),
            Some(4), // 4 parallel API requests
        )?)
    };

    let mut progress = IndexProgress::new(total_count);
    let mut indexed_count: u64 = 0;
    let mut filtered_count: u64 = 0;
    let mut error_count: u64 = 0;
    let mut last_commit: u64 = 0;

    if detailed {
        // DETAILED MODE: parse CSV and attach metadata
        let record_stream = DetailedDomainStream::from_file(input_path);
        let batched_stream = batch_stream_detailed(record_stream, config.word_batch_size);
        futures::pin_mut!(batched_stream);

        while let Some(batch_result) = batched_stream.next().await {
            let batch = batch_result?;
            let batch_size = batch.len();

            let mut valid_domains: Vec<domain_core::NormalizedDomain> = Vec::new();
            let mut labels_to_segment: Vec<String> = Vec::new();

            for record in &batch {
                let domain = Domain::new(&record.domain);

                match domain.normalize() {
                    Ok(normalized) => {
                        if should_filter_domain(&normalized.label) {
                            filtered_count += 1;
                            continue;
                        }

                        let detail = DetailedRecord {
                            dns_servers: record.dns_servers.clone(),
                            ip: record.ip.clone(),
                            country: record.country.clone(),
                            web_server: record.web_server.clone(),
                            email: record.email.clone(),
                            phone: record.phone.clone(),
                            seo_rank: record.seo_rank.clone(),
                        };

                        labels_to_segment.push(normalized.label.clone());
                        valid_domains.push(normalized.with_detailed(detail));
                    }
                    Err(e) => {
                        debug!(domain = &record.domain, error = %e, "Failed to normalize");
                        error_count += 1;
                    }
                }
            }

            // Word segmentation (optional)
            if let Some(ref wc) = word_client {
                if !labels_to_segment.is_empty() {
                    match wc.segment_batch(labels_to_segment).await {
                        Ok(segments) => {
                            let token_map: std::collections::HashMap<&str, (&Vec<String>, &Vec<String>)> =
                                segments.iter().map(|(label, seg, kw)| (label.as_str(), (seg, kw))).collect();
                            for normalized in valid_domains.iter_mut() {
                                if let Some((seg, kw)) = token_map.get(normalized.label.as_str()) {
                                    let mut tokens = (*seg).clone();
                                    for keyword in kw.iter() {
                                        if !tokens.contains(keyword) {
                                            tokens.push(keyword.clone());
                                        }
                                    }
                                    normalized.tokens = tokens;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "Word segmentation failed for batch");
                        }
                    }
                }
            }

            for normalized in &valid_domains {
                let doc = schema.to_document(normalized);
                writer.add_document(doc)?;
                indexed_count += 1;
            }

            if indexed_count - last_commit >= commit_interval as u64 {
                info!(indexed = indexed_count, "Committing checkpoint...");
                writer.commit()?;
                last_commit = indexed_count;
            }

            progress.inc(batch_size as u64);
        }
    } else {
        // PLAIN MODE: existing domain-per-line format
        let domain_stream = DomainStream::from_file(input_path);
        let batched_stream = batch_stream(domain_stream, config.word_batch_size);
        futures::pin_mut!(batched_stream);

        while let Some(batch_result) = batched_stream.next().await {
            let batch: Vec<String> = batch_result?;
            let batch_size = batch.len();

            let mut valid_domains: Vec<(String, domain_core::NormalizedDomain)> = Vec::new();
            let mut labels_to_segment: Vec<String> = Vec::new();

            for raw_domain in &batch {
                let domain = Domain::new(raw_domain);

                match domain.normalize() {
                    Ok(normalized) => {
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

            if let Some(ref wc) = word_client {
                if !labels_to_segment.is_empty() {
                    match wc.segment_batch(labels_to_segment).await {
                        Ok(segments) => {
                            let token_map: std::collections::HashMap<&str, (&Vec<String>, &Vec<String>)> =
                                segments.iter().map(|(label, seg, kw)| (label.as_str(), (seg, kw))).collect();
                            for (_, normalized) in valid_domains.iter_mut() {
                                if let Some((seg, kw)) = token_map.get(normalized.label.as_str()) {
                                    let mut tokens = (*seg).clone();
                                    for keyword in kw.iter() {
                                        if !tokens.contains(keyword) {
                                            tokens.push(keyword.clone());
                                        }
                                    }
                                    normalized.tokens = tokens;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "Word segmentation failed for batch, using empty tokens");
                        }
                    }
                }
            }

            for (_, normalized) in &valid_domains {
                let doc = schema.to_document(normalized);
                writer.add_document(doc)?;
                indexed_count += 1;
            }

            if indexed_count - last_commit >= commit_interval as u64 {
                info!(indexed = indexed_count, "Committing checkpoint...");
                writer.commit()?;
                last_commit = indexed_count;
            }

            progress.inc(batch_size as u64);
        }
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
