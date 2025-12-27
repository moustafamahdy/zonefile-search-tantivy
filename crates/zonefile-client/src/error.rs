use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP error: {0}")]
    Zip(String),

    #[error("Download failed: {status} - {message}")]
    DownloadFailed { status: u16, message: String },

    #[error("Invalid zonefile: {0}")]
    InvalidZonefile(String),
}

pub type Result<T> = std::result::Result<T, Error>;
