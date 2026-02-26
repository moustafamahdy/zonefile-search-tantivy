use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Tokio-SQLite error: {0}")]
    TokioSqlite(#[from] tokio_rusqlite::Error),

    #[error("XML parse error: {0}")]
    Xml(String),

    #[error("Tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Export error: {0}")]
    Export(String),
}

pub type Result<T> = std::result::Result<T, Error>;
