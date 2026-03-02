use crate::config::FetcherConfig;
use crate::error::Result;
use crate::progress::CrawlProgress;
use crate::store::{load_domains_with_page_metadata, PageMetaRow};
use domain_core::{register_ngram_tokenizer, DomainSchema};
use std::collections::HashMap;
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::IndexRecordOption;
use tantivy::{Index, TantivyDocument, Term};
use tracing::{info, warn};

/// Import page metadata from SQLite into the Tantivy index.
///
/// Strategy: iterate over SQLite rows that have page_title (the smaller set, ~494K),
/// look each up in Tantivy, delete the old doc and re-add with page metadata fields.
pub async fn import_metadata(
    index_path: &Path,
    config: &FetcherConfig,
) -> Result<()> {
    info!(index = ?index_path, db = ?config.db_path, "Importing page metadata into Tantivy index");

    // 1. Open Tantivy index
    let index = Index::open_in_dir(index_path)?;
    register_ngram_tokenizer(&index);

    let schema = index.schema();
    let ds = DomainSchema::from_existing(&schema);

    // Verify page metadata fields exist in schema
    if ds.page_title.is_none() {
        warn!("page_title field not found in index schema. The index needs to be rebuilt with the new schema first.");
        warn!("Run a full re-index to add the new fields, then retry import.");
        return Ok(());
    }

    let page_title_field = ds.page_title.unwrap();
    let meta_desc_field = ds.meta_description.unwrap();
    let og_image_field = ds.og_image.unwrap();
    let snippet_field = ds.snippet.unwrap();
    let language_field = ds.language.unwrap();

    // 2. Load all domains with page metadata from SQLite
    info!("Loading domains with page metadata from SQLite...");
    let metadata_rows = load_domains_with_page_metadata(&config.db_path)?;

    let total = metadata_rows.len();
    info!(total, "Domains with page metadata loaded");

    if total == 0 {
        info!("No page metadata to import");
        return Ok(());
    }

    // Build a HashMap for fast lookup
    let metadata_map: HashMap<String, PageMetaRow> = metadata_rows.into_iter().collect();

    // 3. Set up Tantivy writer
    let heap_size = 500 * 1024 * 1024; // 500MB
    let mut writer = index.writer::<TantivyDocument>(heap_size)?;
    let reader = index.reader()?;
    let searcher = reader.searcher();

    let progress = CrawlProgress::new(total as u64);
    let mut enriched: u64 = 0;
    let mut not_found: u64 = 0;
    let commit_interval = config.import_commit_interval as u64;

    // 4. Process domains in order: look up each in Tantivy, enrich, re-add
    let domains: Vec<String> = metadata_map.keys().cloned().collect();

    for domain in &domains {
        let meta = &metadata_map[domain];

        // Look up domain in Tantivy
        let term = Term::from_field_text(ds.domain_exact, domain);
        let query = TermQuery::new(term.clone(), IndexRecordOption::Basic);

        let top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;

        if let Some((_score, doc_address)) = top_docs.first() {
            let doc: TantivyDocument = searcher.doc(*doc_address)?;

            // Delete old document
            writer.delete_term(term);

            // Clone and add page metadata fields
            let mut new_doc = doc;

            if let Some(ref title) = meta.page_title {
                new_doc.add_text(page_title_field, title);
            }
            if let Some(ref desc) = meta.meta_description {
                new_doc.add_text(meta_desc_field, desc);
            }
            if let Some(ref img) = meta.og_image {
                new_doc.add_text(og_image_field, img);
            }
            if let Some(ref snip) = meta.snippet {
                new_doc.add_text(snippet_field, snip);
            }
            if let Some(ref lang) = meta.language {
                new_doc.add_text(language_field, lang);
            }

            writer.add_document(new_doc)?;
            enriched += 1;
        } else {
            not_found += 1;
        }

        if (enriched + not_found) % 10_000 == 0 {
            progress.inc(10_000);
        }

        if enriched > 0 && enriched % commit_interval == 0 {
            info!(enriched, not_found, "Committing batch...");
            writer.commit()?;
        }
    }

    // Final commit
    writer.commit()?;
    progress.finish();

    info!(enriched, not_found, total, "Page metadata import complete");
    Ok(())
}
