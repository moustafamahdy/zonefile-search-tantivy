use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod commoncrawl;
mod config;
mod crawler;
mod error;
mod exporter;
mod fetcher;
mod html_parser;
mod importer;
mod model;
mod page_crawler;
mod page_fetcher;
mod progress;
mod robots;
mod sitemap;
mod store;
// mod wat_parser; // Available for WAT-based enrichment if needed

use config::FetcherConfig;
use store::MetadataStore;

#[derive(Parser)]
#[command(name = "domain-metadata")]
#[command(about = "Crawl robots.txt and sitemaps for all indexed domains", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Crawl robots.txt and sitemaps for all domains
    Crawl {
        /// Path to pre-exported domain list (one domain per line)
        #[arg(long)]
        domains_file: Option<PathBuf>,

        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,

        /// Resume from previous crawl (skip already-done domains)
        #[arg(long, default_value = "true")]
        resume: bool,

        /// Maximum concurrent requests
        #[arg(long)]
        concurrency: Option<usize>,

        /// Path to the Tantivy index (for domain export if no domains-file given)
        #[arg(short, long)]
        index: Option<PathBuf>,
    },

    /// Export all domain names from the Tantivy index to a file
    ExportDomains {
        /// Output file path (one domain per line)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Path to the Tantivy index
        #[arg(short, long)]
        index: Option<PathBuf>,
    },

    /// Show crawl progress and statistics
    Stats {
        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Import crawl results into the Tantivy index
    Import {
        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,

        /// Path to the Tantivy index
        #[arg(short, long)]
        index: Option<PathBuf>,
    },

    /// Fetch page metadata (title, description, OG tags) for all domains
    FetchPages {
        /// Path to pre-exported domain list (one domain per line)
        #[arg(long)]
        domains_file: Option<PathBuf>,

        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,

        /// Resume from previous crawl (skip already-done domains)
        #[arg(long, default_value = "true")]
        resume: bool,

        /// Maximum concurrent requests
        #[arg(long)]
        concurrency: Option<usize>,
    },

    /// Show page metadata crawl statistics
    PageStats {
        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Enrich failed domains with metadata from Common Crawl WAT files
    EnrichCc {
        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,

        /// Path to pre-exported domain list (one domain per line)
        #[arg(long)]
        domains_file: Option<PathBuf>,

        /// Common Crawl index to query (e.g., CC-MAIN-2025-08)
        #[arg(long, alias = "cc-index")]
        index: Option<String>,

        /// Concurrent WAT S3 fetch requests
        #[arg(long)]
        concurrency: Option<usize>,

        /// Delay between CDX API requests in milliseconds
        #[arg(long)]
        cdx_delay_ms: Option<u64>,

        /// Only process domains that got an HTTP response (skip connection failures)
        #[arg(long, default_value = "true")]
        http_only: bool,

        /// Also include all connection-failed domains (overrides --http-only)
        #[arg(long)]
        all: bool,
    },

    /// Export domains that returned HTTP 403 (for Scrapling)
    ExportBlocked {
        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,

        /// Output file path (one domain per line)
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Import metadata from a JSONL file into the SQLite database
    ImportJsonl {
        /// Path to the JSONL input file
        #[arg(short, long)]
        input: PathBuf,

        /// Path to SQLite results database
        #[arg(long)]
        db: Option<PathBuf>,

        /// Source label for imported records (e.g., "scrapling")
        #[arg(long, default_value = "jsonl")]
        source: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let mut config = FetcherConfig::from_env()?;

    match cli.command {
        Commands::Crawl {
            domains_file,
            db,
            resume,
            concurrency,
            index,
        } => {
            if let Some(db) = db {
                config.db_path = db;
            }
            if let Some(c) = concurrency {
                config.concurrency = c;
            }
            if let Some(idx) = index {
                config.index_path = idx;
            }

            let store = MetadataStore::open(&config.db_path).await?;

            // Load domains from file or export from index
            let domains = if let Some(ref file) = domains_file {
                info!(path = ?file, "Loading domains from file");
                load_domains_from_file(file).await?
            } else {
                info!("Exporting domains from Tantivy index");
                let export_path = config.db_path.with_extension("domains.txt");
                exporter::export_domains(&config.index_path, &export_path).await?;
                load_domains_from_file(&export_path).await?
            };

            info!(count = domains.len(), "Domains loaded");
            crawler::run_crawl(domains, &config, store, resume).await?;
        }

        Commands::ExportDomains { output, index } => {
            if let Some(idx) = index {
                config.index_path = idx;
            }
            let output_path =
                output.unwrap_or_else(|| config.index_path.join("../domains_export.txt"));
            exporter::export_domains(&config.index_path, &output_path).await?;
        }

        Commands::Stats { db } => {
            if let Some(db) = db {
                config.db_path = db;
            }
            let store = MetadataStore::open(&config.db_path).await?;
            let stats = store.read_stats().await?;

            println!("=== Crawl Statistics ===");
            println!("Total results:   {}", stats.total_results);
            println!("robots.txt found: {} ({:.1}%)",
                stats.robots_found,
                if stats.total_results > 0 { stats.robots_found as f64 / stats.total_results as f64 * 100.0 } else { 0.0 }
            );
            println!("Sitemaps found:  {} ({:.1}%)",
                stats.sitemap_found,
                if stats.total_results > 0 { stats.sitemap_found as f64 / stats.total_results as f64 * 100.0 } else { 0.0 }
            );
            println!("Errors:          {}", stats.errors);
            println!("Crawl progress:  {}/{}", stats.crawl_done, stats.crawl_total);
            if stats.last_updated > 0 {
                println!("Last updated:    {}", stats.last_updated);
            }
            if !stats.cms_counts.is_empty() {
                println!("\n=== CMS Distribution ===");
                for (cms, count) in &stats.cms_counts {
                    println!("  {:<15} {}", cms, count);
                }
            }
        }

        Commands::Import { db, index } => {
            if let Some(db) = db {
                config.db_path = db;
            }
            if let Some(idx) = index {
                config.index_path = idx;
            }
            importer::import_metadata(&config.index_path, &config).await?;
        }

        Commands::FetchPages {
            domains_file,
            db,
            resume,
            concurrency,
        } => {
            if let Some(db) = db {
                config.db_path = db;
            }
            if let Some(c) = concurrency {
                config.page_concurrency = c;
            }

            let store = MetadataStore::open(&config.db_path).await?;

            let file_path = if let Some(ref file) = domains_file {
                file.clone()
            } else {
                info!("Exporting domains from Tantivy index");
                let export_path = config.db_path.with_extension("domains.txt");
                exporter::export_domains(&config.index_path, &export_path).await?;
                export_path
            };

            info!(path = ?file_path, "Streaming domains from file");
            page_crawler::run_page_crawl(&file_path, &config, store, resume).await?;
        }

        Commands::EnrichCc {
            db,
            domains_file,
            index,
            concurrency,
            cdx_delay_ms,
            http_only,
            all,
        } => {
            if let Some(db) = db {
                config.db_path = db;
            }
            if let Some(idx) = index {
                config.cc_index = idx;
            }
            if let Some(c) = concurrency {
                config.cc_wat_concurrency = c;
            }
            if let Some(d) = cdx_delay_ms {
                config.cc_cdx_delay_ms = d;
            }
            if all {
                config.cc_http_only = false;
            } else {
                config.cc_http_only = http_only;
            }

            let store = MetadataStore::open(&config.db_path).await?;
            commoncrawl::run_commoncrawl_enrichment(
                &config,
                store,
                domains_file.as_deref(),
            ).await?;
        }

        Commands::ExportBlocked { db, output } => {
            if let Some(db) = db {
                config.db_path = db;
            }
            let store = MetadataStore::open(&config.db_path).await?;
            let domains = store.export_blocked_domains().await?;
            info!(count = domains.len(), "Exporting blocked domains");
            let content = domains.join("\n");
            tokio::fs::write(&output, content).await?;
            info!(path = ?output, count = domains.len(), "Blocked domains exported");
        }

        Commands::ImportJsonl { input, db, source } => {
            if let Some(db) = db {
                config.db_path = db;
            }

            let store = MetadataStore::open(&config.db_path).await?;
            commoncrawl::import_jsonl(&input, &store, Some(&source)).await?;
        }

        Commands::PageStats { db } => {
            if let Some(db) = db {
                config.db_path = db;
            }
            let store = MetadataStore::open(&config.db_path).await?;
            let stats = store.read_page_stats().await?;

            println!("=== Page Metadata Statistics ===");
            println!("Total fetched:     {}", stats.total);
            println!("With title:        {} ({:.1}%)",
                stats.with_title,
                pct(stats.with_title, stats.total),
            );
            println!("With description:  {} ({:.1}%)",
                stats.with_description,
                pct(stats.with_description, stats.total),
            );
            println!("With OG tags:      {} ({:.1}%)",
                stats.with_og,
                pct(stats.with_og, stats.total),
            );
            println!("With snippet:      {} ({:.1}%)",
                stats.with_snippet,
                pct(stats.with_snippet, stats.total),
            );
            println!("Errors:            {} ({:.1}%)",
                stats.errors,
                pct(stats.errors, stats.total),
            );
            if !stats.source_counts.is_empty() {
                println!("\n=== Sources ===");
                for (source, count) in &stats.source_counts {
                    println!("  {:<15} {}", source, count);
                }
            }
            if !stats.lang_counts.is_empty() {
                println!("\n=== Top Languages ===");
                for (lang, count) in &stats.lang_counts {
                    println!("  {:<10} {}", lang, count);
                }
            }
        }
    }

    Ok(())
}

fn pct(part: i64, total: i64) -> f64 {
    if total > 0 { part as f64 / total as f64 * 100.0 } else { 0.0 }
}

async fn load_domains_from_file(path: &PathBuf) -> Result<Vec<String>> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let file = tokio::fs::File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut domains = Vec::new();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim().to_string();
        if !trimmed.is_empty() {
            domains.push(trimmed);
        }
    }

    Ok(domains)
}
