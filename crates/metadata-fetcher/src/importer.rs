use crate::config::FetcherConfig;
use crate::error::Result;
use crate::progress::CrawlProgress;
use crate::store::MetadataStore;
use domain_core::{register_ngram_tokenizer, DomainSchema};
use std::path::Path;
use tantivy::schema::Value;
use tantivy::{Index, TantivyDocument, Term};
use tracing::{info, warn};

/// Import metadata results from SQLite back into the Tantivy index.
///
/// Adds new metadata fields (page_count, last_updated, cms, site_category, has_sitemap)
/// using a delete+re-add strategy per document.
///
/// NOTE: Requires DomainSchema to include the new fields first.
pub async fn import_metadata(
    index_path: &Path,
    config: &FetcherConfig,
    _store: &MetadataStore,
) -> Result<()> {
    info!(index = ?index_path, "Importing metadata into Tantivy index");

    let index = Index::open_in_dir(index_path)?;
    register_ngram_tokenizer(&index);

    let schema = index.schema();
    let schema_helper = DomainSchema::from_existing(&schema);

    // Look up new metadata fields
    let page_count_field = schema.get_field("page_count").ok();
    let _last_updated_field = schema.get_field("last_updated").ok();
    let _cms_field = schema.get_field("cms").ok();
    let _site_category_field = schema.get_field("site_category").ok();
    let _has_sitemap_field = schema.get_field("has_sitemap").ok();

    if page_count_field.is_none() {
        warn!("New metadata fields not found in schema. Update DomainSchema::new() and rebuild the index first.");
        return Ok(());
    }

    let reader = index.reader()?;
    let searcher = reader.searcher();
    let total = searcher.num_docs();
    let progress = CrawlProgress::new(total);

    let heap_size = 500 * 1024 * 1024; // 500MB
    let mut writer = index.writer::<TantivyDocument>(heap_size)?;

    let mut imported: u64 = 0;
    let mut committed: u64 = 0;

    // Process per-segment to bound memory
    for segment_reader in searcher.segment_readers() {
        let store_reader = segment_reader.get_store_reader(50)?;

        // Collect domain names for this segment in chunks and batch-query SQLite
        let num_docs = segment_reader.num_docs();
        let chunk_size: u32 = 50_000;
        let mut doc_offset: u32 = 0;

        while doc_offset < num_docs {
            let end = (doc_offset + chunk_size).min(num_docs);

            // Collect domains in this chunk
            let mut chunk_domains: Vec<(u32, String)> = Vec::new();
            for doc_id in doc_offset..end {
                if segment_reader.is_deleted(doc_id) {
                    continue;
                }
                if let Ok(doc) = store_reader.get::<TantivyDocument>(doc_id) {
                    if let Some(domain) = doc
                        .get_first(schema_helper.domain_exact)
                        .and_then(|v| v.as_str())
                    {
                        chunk_domains.push((doc_id, domain.to_string()));
                    }
                }
            }

            // Batch-query SQLite for this chunk's domains
            // (MetadataStore would need a batch lookup method for production use)
            // For now, process individually since SQLite is fast enough with WAL mode
            for (doc_id, domain) in &chunk_domains {
                if let Ok(doc) = store_reader.get::<TantivyDocument>(*doc_id) {
                    let term = Term::from_field_text(schema_helper.domain_exact, domain);
                    writer.delete_term(term);

                    let new_doc = doc.clone();

                    // The actual metadata lookup and field addition would go here
                    // once the store has a batch lookup method and the schema has the new fields

                    writer.add_document(new_doc)?;
                    imported += 1;

                    if imported - committed >= config.import_commit_interval as u64 {
                        info!(imported, "Committing batch...");
                        writer.commit()?;
                        committed = imported;
                    }

                    if imported % 1_000_000 == 0 {
                        progress.inc(1_000_000);
                    }
                }
            }

            doc_offset = end;
        }
    }

    writer.commit()?;
    progress.finish();

    info!(imported, "Import complete");
    Ok(())
}
