mod downloader;
mod error;
pub mod parser;

pub use downloader::{ZonefileDownloader, ZonefileType};
pub use error::{Error, Result};
pub use parser::DomainStream;
