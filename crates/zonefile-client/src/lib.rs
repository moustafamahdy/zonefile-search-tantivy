mod downloader;
mod error;
pub mod parser;

pub use downloader::{ProbeResult, ZonefileDownloader, ZonefileType};
pub use error::{Error, Result};
pub use parser::{
    batch_stream, batch_stream_detailed, DetailedDomainStream, DetailedLine, DomainStream,
};
