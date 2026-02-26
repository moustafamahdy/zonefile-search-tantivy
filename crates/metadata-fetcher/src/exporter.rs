use crate::error::Result;
use crate::progress::CrawlProgress;
use domain_core::{register_ngram_tokenizer, DomainSchema};
use std::path::Path;
use tantivy::schema::Value;
use tantivy::{Index, TantivyDocument};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tracing::info;

/// Export all domain_exact values from the Tantivy index to a plain text file.
/// One domain per line. Returns the total number of domains exported.
pub async fn export_domains(index_path: &Path, output_path: &Path) -> Result<u64> {
    info!(index = ?index_path, output = ?output_path, "Exporting domains from index");

    let index = Index::open_in_dir(index_path)?;
    register_ngram_tokenizer(&index);
    let schema_helper = DomainSchema::from_existing(&index.schema());
    let domain_field = schema_helper.domain_exact;

    let reader = index.reader()?;
    let searcher = reader.searcher();
    let total_docs = searcher.num_docs();

    info!(total = total_docs, "Total documents in index");

    let file = File::create(output_path).await?;
    let mut writer = BufWriter::new(file);
    let progress = CrawlProgress::new(total_docs);

    let mut exported: u64 = 0;

    for segment_reader in searcher.segment_readers() {
        let store_reader = segment_reader.get_store_reader(50)?;

        for doc_id in 0..segment_reader.num_docs() {
            if segment_reader.is_deleted(doc_id) {
                continue;
            }

            match store_reader.get::<TantivyDocument>(doc_id) {
                Ok(doc) => {
                    if let Some(domain) = doc
                        .get_first(domain_field)
                        .and_then(|v| v.as_str())
                    {
                        writer.write_all(domain.as_bytes()).await?;
                        writer.write_all(b"\n").await?;
                        exported += 1;

                        if exported % 1_000_000 == 0 {
                            progress.inc(1_000_000);
                            writer.flush().await?;
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(doc_id, error = %e, "Failed to read doc");
                }
            }
        }
    }

    writer.flush().await?;
    progress.finish();

    info!(exported, path = ?output_path, "Export complete");
    Ok(exported)
}
