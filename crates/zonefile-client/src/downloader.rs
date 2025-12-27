use crate::error::{Error, Result};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

/// Type of zonefile to download
#[derive(Debug, Clone, Copy)]
pub enum ZonefileType {
    /// Full zonefile (all domains)
    Full,
    /// Daily added domains
    DailyUpdate,
    /// Daily removed domains
    DailyRemove,
}

impl ZonefileType {
    fn endpoint(&self) -> &'static str {
        match self {
            ZonefileType::Full => "full",
            ZonefileType::DailyUpdate => "dailyupdate",
            ZonefileType::DailyRemove => "dailyremove",
        }
    }
}

/// Client for downloading zonefiles from domains-monitor.com
pub struct ZonefileDownloader {
    client: Client,
    base_url: String,
    token: String,
    download_dir: PathBuf,
}

impl ZonefileDownloader {
    /// Create a new ZonefileDownloader
    ///
    /// # Arguments
    /// * `base_url` - API base URL (e.g., "https://domains-monitor.com/api/v1")
    /// * `token` - API authentication token
    /// * `download_dir` - Directory for temporary downloads
    pub fn new(
        base_url: impl Into<String>,
        token: impl Into<String>,
        download_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(3600)) // 1 hour timeout for large downloads
            .connect_timeout(Duration::from_secs(30))
            .build()?;

        let download_dir = download_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&download_dir)?;

        Ok(Self {
            client,
            base_url: base_url.into(),
            token: token.into(),
            download_dir,
        })
    }

    /// Download a zonefile and return the path to the extracted file
    ///
    /// Downloads a ZIP file from the API, extracts domains.txt, and returns its path.
    pub async fn download(&self, zonefile_type: ZonefileType) -> Result<PathBuf> {
        let endpoint = zonefile_type.endpoint();
        let url = format!(
            "{}/{}/get/{}/list/zip",
            self.base_url, self.token, endpoint
        );

        info!(endpoint = endpoint, "Downloading zonefile");

        // Download ZIP to temp file
        let zip_path = self.download_dir.join(format!("{}.zip", endpoint));
        self.download_file(&url, &zip_path).await?;

        // Extract domains.txt from ZIP
        let extracted_path = self.download_dir.join(format!("{}.txt", endpoint));
        self.extract_domains_txt(&zip_path, &extracted_path).await?;

        // Clean up ZIP file
        if let Err(e) = tokio::fs::remove_file(&zip_path).await {
            debug!(error = %e, "Failed to remove ZIP file");
        }

        info!(path = ?extracted_path, "Zonefile extracted successfully");
        Ok(extracted_path)
    }

    /// Download a file from URL to disk with progress tracking
    async fn download_file(&self, url: &str, path: &Path) -> Result<()> {
        let response = self.client.get(url).send().await?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::DownloadFailed {
                status: status.as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        let total_size = response.content_length().unwrap_or(0);
        info!(
            size_mb = total_size / 1024 / 1024,
            "Starting download"
        );

        let mut file = File::create(path).await?;
        let mut stream = response.bytes_stream();
        let mut downloaded: u64 = 0;
        let mut last_log: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk: Bytes = chunk?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;

            // Log progress every 100MB
            if downloaded - last_log > 100 * 1024 * 1024 {
                let pct = if total_size > 0 {
                    (downloaded as f64 / total_size as f64 * 100.0) as u32
                } else {
                    0
                };
                info!(
                    downloaded_mb = downloaded / 1024 / 1024,
                    percent = pct,
                    "Download progress"
                );
                last_log = downloaded;
            }
        }

        file.flush().await?;
        info!(downloaded_mb = downloaded / 1024 / 1024, "Download complete");

        Ok(())
    }

    /// Extract domains.txt from a ZIP file
    async fn extract_domains_txt(&self, zip_path: &Path, output_path: &Path) -> Result<()> {
        use async_zip::tokio::read::fs::ZipFileReader;
        use tokio_util::compat::FuturesAsyncReadCompatExt;

        let reader = ZipFileReader::new(zip_path)
            .await
            .map_err(|e| Error::Zip(e.to_string()))?;

        // Find domains.txt in the archive
        let entries = reader.file().entries();
        let mut domains_idx = None;

        for (idx, entry) in entries.iter().enumerate() {
            let filename = entry
                .filename()
                .as_str()
                .map_err(|e| Error::Zip(e.to_string()))?;
            if filename == "domains.txt" || filename.ends_with("/domains.txt") {
                domains_idx = Some(idx);
                break;
            }
        }

        let idx = domains_idx.ok_or_else(|| {
            Error::InvalidZonefile("domains.txt not found in archive".to_string())
        })?;

        // Extract the file
        let entry_reader = reader
            .reader_with_entry(idx)
            .await
            .map_err(|e| Error::Zip(e.to_string()))?;

        // Convert futures::io::AsyncRead to tokio::io::AsyncRead
        let mut compat_reader = entry_reader.compat();
        let mut output_file = File::create(output_path).await?;
        tokio::io::copy(&mut compat_reader, &mut output_file).await?;
        output_file.flush().await?;

        let size = tokio::fs::metadata(output_path).await?.len();
        info!(
            size_mb = size / 1024 / 1024,
            "Extracted domains.txt"
        );

        Ok(())
    }

    /// Download directly to memory (for smaller files like daily updates)
    pub async fn download_to_memory(&self, zonefile_type: ZonefileType) -> Result<Vec<u8>> {
        let endpoint = zonefile_type.endpoint();
        let url = format!(
            "{}/{}/get/{}/list/zip",
            self.base_url, self.token, endpoint
        );

        debug!(endpoint = endpoint, "Downloading zonefile to memory");

        let response = self.client.get(&url).send().await?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::DownloadFailed {
                status: status.as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zonefile_type_endpoint() {
        assert_eq!(ZonefileType::Full.endpoint(), "full");
        assert_eq!(ZonefileType::DailyUpdate.endpoint(), "dailyupdate");
        assert_eq!(ZonefileType::DailyRemove.endpoint(), "dailyremove");
    }
}
