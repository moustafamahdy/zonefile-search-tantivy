use crate::error::Result;
use async_stream::try_stream;
use futures::Stream;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::debug;

/// Stream of domains parsed from a zonefile
pub struct DomainStream;

impl DomainStream {
    /// Create a stream of domains from a file path
    ///
    /// Reads the file line by line and yields valid domain strings.
    /// Filters out:
    /// - Empty lines
    /// - Comment lines (starting with #)
    /// - Lines with invalid domain format
    pub fn from_file(path: impl AsRef<Path>) -> impl Stream<Item = Result<String>> {
        let path = path.as_ref().to_path_buf();

        try_stream! {
            let file = File::open(&path).await?;
            let reader = BufReader::with_capacity(1024 * 1024, file); // 1MB buffer
            let mut lines = reader.lines();
            let mut count: u64 = 0;

            while let Some(line) = lines.next_line().await? {
                let line = line.trim();

                // Skip empty lines and comments
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                // Basic validation: must contain at least one dot
                if !line.contains('.') {
                    continue;
                }

                // Skip lines that are too long (DNS label limit is 253 total)
                if line.len() > 253 {
                    continue;
                }

                count += 1;

                // Log progress every 10M domains
                if count % 10_000_000 == 0 {
                    debug!(count = count / 1_000_000, "Parsed {}M domains", count / 1_000_000);
                }

                yield line.to_string();
            }

            debug!(total = count, "Finished parsing file");
        }
    }

    /// Create a stream of domains from raw bytes (for in-memory ZIP content)
    pub fn from_bytes(data: Vec<u8>) -> impl Stream<Item = Result<String>> {
        try_stream! {
            let cursor = std::io::Cursor::new(data);
            let reader = std::io::BufReader::new(cursor);

            use std::io::BufRead;
            for line in reader.lines() {
                let line = line?;
                let line = line.trim();

                // Skip empty lines and comments
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                // Basic validation
                if !line.contains('.') || line.len() > 253 {
                    continue;
                }

                yield line.to_string();
            }
        }
    }

    /// Count domains in a file without fully parsing
    pub async fn count_file(path: impl AsRef<Path>) -> Result<u64> {
        let file = File::open(path.as_ref()).await?;
        let reader = BufReader::with_capacity(1024 * 1024, file);
        let mut lines = reader.lines();
        let mut count: u64 = 0;

        while let Some(line) = lines.next_line().await? {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') && line.contains('.') {
                count += 1;
            }
        }

        Ok(count)
    }
}

/// Batch domains from a stream into chunks
pub fn batch_stream<S>(
    stream: S,
    batch_size: usize,
) -> impl Stream<Item = Result<Vec<String>>>
where
    S: Stream<Item = Result<String>>,
{
    use futures::StreamExt;

    try_stream! {
        let mut batch = Vec::with_capacity(batch_size);

        futures::pin_mut!(stream);

        while let Some(item) = stream.next().await {
            let domain = item?;
            batch.push(domain);

            if batch.len() >= batch_size {
                yield std::mem::replace(&mut batch, Vec::with_capacity(batch_size));
            }
        }

        // Yield remaining items
        if !batch.is_empty() {
            yield batch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn test_from_bytes() {
        let data = b"example.com\ntest.net\n\n# comment\ninvalid\n".to_vec();

        let stream = DomainStream::from_bytes(data);
        futures::pin_mut!(stream);

        let mut domains = Vec::new();
        while let Some(result) = stream.next().await {
            domains.push(result.unwrap());
        }

        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0], "example.com");
        assert_eq!(domains[1], "test.net");
    }

    #[tokio::test]
    async fn test_batch_stream() {
        let data = b"a.com\nb.com\nc.com\nd.com\ne.com\n".to_vec();
        let stream = DomainStream::from_bytes(data);
        let batched = batch_stream(stream, 2);

        futures::pin_mut!(batched);

        let mut batches = Vec::new();
        while let Some(result) = batched.next().await {
            batches.push(result.unwrap());
        }

        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0], vec!["a.com", "b.com"]);
        assert_eq!(batches[1], vec!["c.com", "d.com"]);
        assert_eq!(batches[2], vec!["e.com"]);
    }
}
