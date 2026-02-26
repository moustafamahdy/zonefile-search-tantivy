use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod crawler;
mod error;
mod exporter;
mod fetcher;
mod importer;
mod model;
mod progress;
mod robots;
mod sitemap;
mod store;

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
            let store = MetadataStore::open(&config.db_path).await?;
            importer::import_metadata(&config.index_path, &config, &store).await?;
        }
    }

    Ok(())
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
