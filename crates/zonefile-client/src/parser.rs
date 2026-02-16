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

// --- Detailed zonefile CSV parsing ---

/// A parsed line from a detailed zonefile CSV
///
/// Format: "domain";"dns_servers";"ip";"country";"web_server";"email";"(unused)";"phone";"seo_rank";
#[derive(Debug, Clone)]
pub struct DetailedLine {
    pub domain: String,
    pub dns_servers: String,
    pub ip: String,
    pub country: String,
    pub web_server: String,
    pub email: String,
    pub phone: String,
    pub seo_rank: String,
}

/// Parse a single detailed CSV line into its components.
/// Returns None for malformed or invalid lines.
fn parse_detailed_line(line: &str) -> Option<DetailedLine> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // Split by semicolon delimiter
    let parts: Vec<&str> = line.split(';').collect();

    // Need at least 9 fields (trailing semicolon may add a 10th empty one)
    if parts.len() < 9 {
        return None;
    }

    let strip_quotes = |s: &str| -> String {
        s.trim().trim_matches('"').to_string()
    };

    let domain = strip_quotes(parts[0]);

    // Basic domain validation
    if !domain.contains('.') || domain.len() > 253 || domain.is_empty() {
        return None;
    }

    Some(DetailedLine {
        domain,
        dns_servers: strip_quotes(parts[1]),
        ip: strip_quotes(parts[2]),
        country: strip_quotes(parts[3]),
        web_server: strip_quotes(parts[4]),
        email: strip_quotes(parts[5]),
        // parts[6] is the unused field
        phone: strip_quotes(parts[7]),
        seo_rank: strip_quotes(parts[8]),
    })
}

/// Stream of detailed domain records parsed from a CSV zonefile
pub struct DetailedDomainStream;

impl DetailedDomainStream {
    /// Create a stream of detailed records from a CSV file
    pub fn from_file(path: impl AsRef<Path>) -> impl Stream<Item = Result<DetailedLine>> {
        let path = path.as_ref().to_path_buf();

        try_stream! {
            let file = File::open(&path).await?;
            let reader = BufReader::with_capacity(1024 * 1024, file);
            let mut lines = reader.lines();
            let mut count: u64 = 0;

            while let Some(line) = lines.next_line().await? {
                if let Some(parsed) = parse_detailed_line(&line) {
                    count += 1;
                    if count % 10_000_000 == 0 {
                        debug!(count = count / 1_000_000, "Parsed {}M detailed records", count / 1_000_000);
                    }
                    yield parsed;
                }
            }

            debug!(total = count, "Finished parsing detailed file");
        }
    }

    /// Count records in a detailed CSV file
    pub async fn count_file(path: impl AsRef<Path>) -> Result<u64> {
        let file = File::open(path.as_ref()).await?;
        let reader = BufReader::with_capacity(1024 * 1024, file);
        let mut lines = reader.lines();
        let mut count: u64 = 0;

        while let Some(line) = lines.next_line().await? {
            if parse_detailed_line(&line).is_some() {
                count += 1;
            }
        }

        Ok(count)
    }
}

/// Batch detailed records from a stream into chunks
pub fn batch_stream_detailed<S>(
    stream: S,
    batch_size: usize,
) -> impl Stream<Item = Result<Vec<DetailedLine>>>
where
    S: Stream<Item = Result<DetailedLine>>,
{
    use futures::StreamExt;

    try_stream! {
        let mut batch = Vec::with_capacity(batch_size);

        futures::pin_mut!(stream);

        while let Some(item) = stream.next().await {
            let record = item?;
            batch.push(record);

            if batch.len() >= batch_size {
                yield std::mem::replace(&mut batch, Vec::with_capacity(batch_size));
            }
        }

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

    #[test]
    fn test_parse_detailed_line_valid() {
        let line = r#""example.com";"ns1.example.com,ns2.example.com";"93.184.216.34";"us";"nginx";"admin@example.com";"";""+1234567890";"42";"#;
        let parsed = parse_detailed_line(line).unwrap();

        assert_eq!(parsed.domain, "example.com");
        assert_eq!(parsed.dns_servers, "ns1.example.com,ns2.example.com");
        assert_eq!(parsed.ip, "93.184.216.34");
        assert_eq!(parsed.country, "us");
        assert_eq!(parsed.web_server, "nginx");
        assert_eq!(parsed.email, "admin@example.com");
        assert_eq!(parsed.phone, "+1234567890");
        assert_eq!(parsed.seo_rank, "42");
    }

    #[test]
    fn test_parse_detailed_line_empty_fields() {
        let line = r#""nodata.net";"";"";"";"";"";"";"";"";"#;
        let parsed = parse_detailed_line(line).unwrap();

        assert_eq!(parsed.domain, "nodata.net");
        assert_eq!(parsed.dns_servers, "");
        assert_eq!(parsed.ip, "");
        assert_eq!(parsed.country, "");
        assert_eq!(parsed.web_server, "");
        assert_eq!(parsed.email, "");
        assert_eq!(parsed.phone, "");
        assert_eq!(parsed.seo_rank, "");
    }

    #[test]
    fn test_parse_detailed_line_malformed() {
        // Too few fields
        assert!(parse_detailed_line(r#""example.com";"ns1""#).is_none());

        // Empty line
        assert!(parse_detailed_line("").is_none());

        // Comment
        assert!(parse_detailed_line("# comment").is_none());

        // No dot in domain
        assert!(parse_detailed_line(r#""nodot";"";"";"";"";"";"";"";"";"#).is_none());
    }

    #[test]
    fn test_parse_detailed_line_real_data() {
        // Actual line from domains-monitor.com sample
        let line = r#""05amarquitectura.com";"ns1.cdmon.net,ns3.cdmon.net,ns4.cdmondns-01.org,ns5.cdmondns-01.com,ns2.cdmon.net";"34.77.10.20";"be";"apache";"arquitectura@05am.com";"";""+34972013234";"";""#;
        let parsed = parse_detailed_line(line).unwrap();

        assert_eq!(parsed.domain, "05amarquitectura.com");
        assert_eq!(parsed.country, "be");
        assert_eq!(parsed.web_server, "apache");
        assert_eq!(parsed.email, "arquitectura@05am.com");
        assert_eq!(parsed.phone, "+34972013234");
    }

    #[tokio::test]
    async fn test_detailed_domain_stream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.csv");

        let mut file = tokio::fs::File::create(&path).await.unwrap();
        file.write_all(
            br#""example.com";"ns1.example.com";"1.2.3.4";"us";"nginx";"";"";"";"";"
"test.de";"ns1.test.de";"5.6.7.8";"de";"apache";"";"";"";"";"
"#,
        )
        .await
        .unwrap();
        file.flush().await.unwrap();

        let stream = DetailedDomainStream::from_file(&path);
        futures::pin_mut!(stream);

        let mut records = Vec::new();
        while let Some(result) = stream.next().await {
            records.push(result.unwrap());
        }

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].domain, "example.com");
        assert_eq!(records[0].country, "us");
        assert_eq!(records[1].domain, "test.de");
        assert_eq!(records[1].country, "de");
    }

    #[tokio::test]
    async fn test_detailed_count_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("count.csv");

        let mut file = tokio::fs::File::create(&path).await.unwrap();
        file.write_all(
            br#""a.com";"";"";"";"";"";"";"";"";"
"b.net";"";"";"";"";"";"";"";"";"
"c.org";"";"";"";"";"";"";"";"";"
"#,
        )
        .await
        .unwrap();
        file.flush().await.unwrap();

        let count = DetailedDomainStream::count_file(&path).await.unwrap();
        assert_eq!(count, 3);
    }
}
