use crate::progress::IndexProgress;
use anyhow::Result;
use domain_core::{
    domain::should_filter_domain, register_ngram_tokenizer, Config, DetailedRecord, Domain,
    DomainSchema,
};
use futures::StreamExt;
use serde::Serialize;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::schema::Value;
use tantivy::{Index, Searcher, Term};
use tracing::{debug, info, warn};
use word_client::WordClient;
use zonefile_client::{
    batch_stream, batch_stream_detailed, DetailedDomainStream, DomainStream, ZonefileDownloader,
    ZonefileType,
};

#[derive(Debug, Serialize)]
struct NsChange {
    domain: String,
    old_ns: String,
    new_ns: String,
    timestamp: String,
}

/// Run daily sync with download from API.
///
/// `prefer_detailed` is the operator preference (CLI flag / `DETAILED_MODE`
/// env var). The actual mode is resolved against the plan via
/// [`crate::mode::resolve_mode_via_downloader`] before any download starts —
/// see the dual-mode design doc for the full decision matrix.
pub async fn run_with_download(
    config: &Config,
    index_path: &Path,
    prefer_detailed: bool,
    no_word_segment: bool,
    notify_ns_changes: bool,
) -> Result<()> {
    let downloader = ZonefileDownloader::new(
        &config.zonefile_api_url,
        &config.zonefile_token,
        std::env::temp_dir().join("zonefile-indexer"),
    )?;

    // Resolve mode against the live plan. May log [MODE] info/warn lines.
    let detailed = crate::mode::resolve_mode_via_downloader(prefer_detailed, &downloader).await?;

    // Download adds file (detailed or plain) using the resolved mode.
    let adds_type = if detailed {
        ZonefileType::DetailedDailyUpdate
    } else {
        ZonefileType::DailyUpdate
    };

    info!("Downloading daily update file...");
    let adds_path = downloader.download(adds_type).await?;

    // Removals are always plain domain lists (no detailed version exists)
    info!("Downloading daily remove file...");
    let removes_path = downloader.download(ZonefileType::DailyRemove).await?;

    run(
        config,
        Some(adds_path),
        Some(removes_path),
        index_path,
        detailed,
        no_word_segment,
        notify_ns_changes,
    )
    .await
}

/// Run daily sync from local files
pub async fn run(
    config: &Config,
    adds_path: Option<impl AsRef<Path>>,
    removes_path: Option<impl AsRef<Path>>,
    index_path: &Path,
    detailed: bool,
    no_word_segment: bool,
    notify_ns_changes: bool,
) -> Result<()> {
    info!("Starting daily sync");

    // Open existing index
    let schema = DomainSchema::new();
    let index = Index::open_in_dir(index_path)?;
    register_ngram_tokenizer(&index);
    let reader = index.reader()?;
    let initial_count = reader.searcher().num_docs();

    info!(documents = initial_count, "Current index size");

    let mut writer = index.writer(500 * 1024 * 1024)?; // 500MB heap for daily updates

    let word_client = if no_word_segment {
        info!("Word segmentation disabled (--no-word-segment)");
        None
    } else {
        Some(WordClient::new(
            &config.word_splitter_url,
            &config.word_splitter_user,
            &config.word_splitter_pass,
            Some(config.word_batch_size),
            Some(4),
        )?)
    };

    // Prepare searcher for NS change detection (before any writes).
    // Note: `detailed` here is the *resolved* mode (from `resolve_mode`), not the
    // raw operator preference — so on standard plan this gate is always false
    // and NS-change detection silently no-ops, which is the intended dormant
    // behavior described in the dual-mode design doc §6.4.
    let searcher = if notify_ns_changes && detailed {
        info!("NS change detection enabled");
        Some(reader.searcher())
    } else {
        None
    };

    let mut total_deleted: u64 = 0;
    let mut total_added: u64 = 0;

    // Process removals first (always plain format)
    if let Some(removes_path) = removes_path {
        let removes_path = removes_path.as_ref();
        if removes_path.exists() {
            info!(path = ?removes_path, "Processing removals...");
            total_deleted = process_removals(&schema, &mut writer, removes_path).await?;
            info!(deleted = total_deleted, "Removals complete");
        }
    }

    // Process additions (detailed or plain)
    let mut ns_changes: Vec<NsChange> = Vec::new();
    if let Some(adds_path) = adds_path {
        let adds_path = adds_path.as_ref();
        if adds_path.exists() {
            info!(path = ?adds_path, detailed = detailed, "Processing additions...");
            total_added = process_additions(
                config,
                &schema,
                &word_client,
                &mut writer,
                adds_path,
                detailed,
                searcher.as_ref(),
                &mut ns_changes,
            )
            .await?;

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

    // POST NS changes to namemaxi-sync if any detected
    if !ns_changes.is_empty() {
        info!(count = ns_changes.len(), "NS changes detected, posting...");
        post_ns_changes(&ns_changes).await;
    }

    Ok(())
}

/// Look up the current dns_servers for a domain in the existing index
fn lookup_current_ns(
    searcher: &Searcher,
    schema: &DomainSchema,
    domain_exact: &str,
) -> Option<String> {
    let term = Term::from_field_text(schema.domain_exact, domain_exact);
    let query = TermQuery::new(term, IndexRecordOption::Basic);

    let top_docs = searcher.search(&query, &TopDocs::with_limit(1)).ok()?;
    let (_, doc_address) = top_docs.into_iter().next()?;
    let doc: tantivy::TantivyDocument = searcher.doc(doc_address).ok()?;

    let dns_field = schema.dns_servers?;
    doc.get_first(dns_field)
        .and_then(|v| v.as_str().map(|s| s.to_string()))
}

/// POST NS changes to the namemaxi-sync endpoint
async fn post_ns_changes(changes: &[NsChange]) {
    let endpoint =
        std::env::var("NS_CHANGE_ENDPOINT").unwrap_or_default();
    let secret = std::env::var("NS_CHANGE_SECRET").unwrap_or_default();

    if endpoint.is_empty() {
        info!(
            count = changes.len(),
            "NS_CHANGE_ENDPOINT not set, skipping POST (changes detected but not sent)"
        );
        return;
    }

    let client = reqwest::Client::new();

    // Send in batches of 500
    for (i, chunk) in changes.chunks(500).enumerate() {
        let body = serde_json::json!({ "changes": chunk });

        match client
            .post(&endpoint)
            .header("x-ns-sync-secret", &secret)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    info!(batch = i + 1, count = chunk.len(), "NS changes posted successfully");
                } else {
                    warn!(
                        batch = i + 1,
                        status = %resp.status(),
                        "NS change POST returned non-success status"
                    );
                }
            }
            Err(e) => {
                warn!(batch = i + 1, error = %e, "Failed to POST NS changes (continuing)");
            }
        }
    }
}

async fn process_removals(
    schema: &DomainSchema,
    writer: &mut tantivy::IndexWriter,
    removes_path: &Path,
) -> Result<u64> {
    let domain_stream = DomainStream::from_file(removes_path);
    let batched = batch_stream(domain_stream, 10_000);

    futures::pin_mut!(batched);

    let mut progress = IndexProgress::spinner();
    let mut deleted: u64 = 0;

    while let Some(batch_result) = batched.next().await {
        let batch: Vec<String> = batch_result?;

        for raw_domain in batch {
            let domain = Domain::new(&raw_domain);

            match domain.normalize() {
                Ok(normalized) => {
                    let term =
                        Term::from_field_text(schema.domain_exact, &normalized.domain_exact);
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
    word_client: &Option<WordClient>,
    writer: &mut tantivy::IndexWriter,
    adds_path: &Path,
    detailed: bool,
    searcher: Option<&Searcher>,
    ns_changes: &mut Vec<NsChange>,
) -> Result<u64> {
    let mut progress = IndexProgress::spinner();
    let mut added: u64 = 0;
    let mut filtered: u64 = 0;
    let timestamp = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("{}", now)
    };

    if detailed {
        // DETAILED MODE: parse CSV and attach metadata
        let record_stream = DetailedDomainStream::from_file(adds_path);
        let batched = batch_stream_detailed(record_stream, config.word_batch_size);
        futures::pin_mut!(batched);

        while let Some(batch_result) = batched.next().await {
            let batch = batch_result?;
            let batch_size = batch.len();

            let mut valid_domains: Vec<domain_core::NormalizedDomain> = Vec::new();
            let mut labels_to_segment: Vec<String> = Vec::new();
            // Track records alongside valid_domains for NS comparison
            let mut valid_records: Vec<&zonefile_client::parser::DetailedLine> = Vec::new();

            for record in &batch {
                let domain = Domain::new(&record.domain);

                match domain.normalize() {
                    Ok(normalized) => {
                        if should_filter_domain(&normalized.label) {
                            filtered += 1;
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
                        valid_records.push(record);
                    }
                    Err(e) => {
                        debug!(domain = &record.domain, error = %e, "Failed to normalize");
                    }
                }
            }

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
                            warn!(error = %e, "Word segmentation failed");
                        }
                    }
                }
            }

            for (idx, normalized) in valid_domains.iter().enumerate() {
                // NS change detection: compare old vs new before overwriting
                if let Some(ref s) = searcher {
                    let new_ns = &valid_records[idx].dns_servers;
                    if !new_ns.is_empty() {
                        if let Some(old_ns) =
                            lookup_current_ns(s, schema, &normalized.domain_exact)
                        {
                            if !old_ns.is_empty() && old_ns != *new_ns {
                                ns_changes.push(NsChange {
                                    domain: normalized.domain_exact.clone(),
                                    old_ns,
                                    new_ns: new_ns.clone(),
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                    }
                }

                let term =
                    Term::from_field_text(schema.domain_exact, &normalized.domain_exact);
                writer.delete_term(term);

                let doc = schema.to_document(normalized);
                writer.add_document(doc)?;
                added += 1;
            }

            progress.inc(batch_size as u64);
        }
    } else {
        // PLAIN MODE: domain-per-line format
        let domain_stream = DomainStream::from_file(adds_path);
        let batched = batch_stream(domain_stream, config.word_batch_size);
        futures::pin_mut!(batched);

        while let Some(batch_result) = batched.next().await {
            let batch: Vec<String> = batch_result?;
            let batch_size = batch.len();

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
                            warn!(error = %e, "Word segmentation failed, using empty tokens");
                        }
                    }
                }
            }

            for normalized in &valid_domains {
                let term =
                    Term::from_field_text(schema.domain_exact, &normalized.domain_exact);
                writer.delete_term(term);

                let doc = schema.to_document(normalized);
                writer.add_document(doc)?;
                added += 1;
            }

            progress.inc(batch_size as u64);
        }
    }

    progress.finish();

    if filtered > 0 {
        info!(filtered = filtered, "Domains filtered during addition");
    }

    Ok(added)
}
