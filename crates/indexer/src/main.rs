use anyhow::Result;
use clap::{Parser, Subcommand};
use domain_core::Config;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod daily;
mod full;
mod progress;

#[derive(Parser)]
#[command(name = "domain-indexer")]
#[command(about = "Domain search indexer for Tantivy", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build a full index from a zonefile
    Full {
        /// Path to the input zonefile (domains.txt)
        #[arg(short, long)]
        input: Option<PathBuf>,

        /// Download the zonefile from API instead of using local file
        #[arg(long)]
        download: bool,

        /// Path to the output index directory
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// IndexWriter heap size in GB
        #[arg(long, default_value = "4")]
        heap_gb: usize,

        /// Commit interval (number of documents)
        #[arg(long, default_value = "1000000")]
        commit_interval: usize,
    },

    /// Apply daily incremental updates (adds and deletes)
    Daily {
        /// Path to added domains file
        #[arg(long)]
        adds: Option<PathBuf>,

        /// Path to removed domains file
        #[arg(long)]
        removes: Option<PathBuf>,

        /// Download daily files from API instead of using local files
        #[arg(long)]
        download: bool,

        /// Path to the existing index directory
        #[arg(short, long)]
        index: Option<PathBuf>,
    },

    /// Show index statistics
    Stats {
        /// Path to the index directory
        #[arg(short, long)]
        index: Option<PathBuf>,
    },

    /// Optimize/merge index segments
    Optimize {
        /// Path to the index directory
        #[arg(short, long)]
        index: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let config = Config::from_env()?;

    match cli.command {
        Commands::Full {
            input,
            download,
            output,
            heap_gb,
            commit_interval,
        } => {
            let output_path = output.unwrap_or_else(|| config.index_path.clone());
            let heap_size = heap_gb * 1024 * 1024 * 1024;

            if download {
                info!("Downloading full zonefile from API...");
                full::run_with_download(&config, &output_path, heap_size, commit_interval).await?;
            } else {
                let input_path = input.ok_or_else(|| {
                    anyhow::anyhow!("--input is required when not using --download")
                })?;
                info!(input = ?input_path, output = ?output_path, "Building full index");
                full::run(&config, &input_path, &output_path, heap_size, commit_interval).await?;
            }
        }

        Commands::Daily {
            adds,
            removes,
            download,
            index,
        } => {
            let index_path = index.unwrap_or_else(|| config.index_path.clone());

            if download {
                info!("Downloading daily updates from API...");
                daily::run_with_download(&config, &index_path).await?;
            } else {
                info!(index = ?index_path, "Applying daily updates");
                daily::run(&config, adds, removes, &index_path).await?;
            }
        }

        Commands::Stats { index } => {
            let index_path = index.unwrap_or_else(|| config.index_path.clone());
            show_stats(&index_path)?;
        }

        Commands::Optimize { index } => {
            let index_path = index.unwrap_or_else(|| config.index_path.clone());
            optimize_index(&index_path)?;
        }
    }

    Ok(())
}

fn show_stats(index_path: &PathBuf) -> Result<()> {
    use domain_core::DomainSchema;
    use tantivy::Index;

    let schema = DomainSchema::new();
    let index = Index::open_in_dir(index_path)?;
    let reader = index.reader()?;
    let searcher = reader.searcher();

    let num_docs = searcher.num_docs();
    let num_segments = searcher.segment_readers().len();

    info!(documents = num_docs, segments = num_segments, "Index statistics");

    // Show space usage
    let mut total_size: u64 = 0;
    for entry in std::fs::read_dir(index_path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total_size += entry.metadata()?.len();
        }
    }

    info!(
        size_gb = total_size as f64 / 1024.0 / 1024.0 / 1024.0,
        "Index size"
    );

    Ok(())
}

fn optimize_index(index_path: &PathBuf) -> Result<()> {
    use tantivy::{Index, TantivyDocument};

    info!("Optimizing index...");

    let index = Index::open_in_dir(index_path)?;
    let mut writer = index.writer::<TantivyDocument>(500 * 1024 * 1024)?; // 500MB heap

    // Commit to finalize any pending merges
    writer.commit()?;

    info!("Index optimization complete");

    Ok(())
}
