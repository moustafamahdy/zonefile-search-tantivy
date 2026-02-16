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
    /// Full zonefile (all domains, plain list)
    Full,
    /// Daily added domains (plain list)
    DailyUpdate,
    /// Daily removed domains (plain list)
    DailyRemove,
    /// Full detailed zonefile (CSV with metadata)
    DetailedFull,
    /// Daily added domains with detailed metadata (CSV)
    DetailedDailyUpdate,
}

impl ZonefileType {
    /// Short label for logging and temp file naming
    fn endpoint(&self) -> &'static str {
        match self {
            ZonefileType::Full => "full",
            ZonefileType::DailyUpdate => "dailyupdate",
            ZonefileType::DailyRemove => "dailyremove",
            ZonefileType::DetailedFull => "detailed-full",
            ZonefileType::DetailedDailyUpdate => "detailed-update",
        }
    }

    /// URL path segment for the API request
    fn url_path(&self) -> &'static str {
        match self {
            ZonefileType::Full => "get/full/list/zip",
            ZonefileType::DailyUpdate => "get/dailyupdate/list/zip",
            ZonefileType::DailyRemove => "get/dailyremove/list/zip",
            ZonefileType::DetailedFull => "get-detailed/full/list/zip",
            ZonefileType::DetailedDailyUpdate => "get-detailed/full-update/list/zip",
        }
    }

    /// Whether this is a detailed (CSV) zonefile type
    pub fn is_detailed(&self) -> bool {
        matches!(
            self,
            ZonefileType::DetailedFull | ZonefileType::DetailedDailyUpdate
        )
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
    /// Downloads a ZIP file from the API, extracts the data file, and returns its path.
    pub async fn download(&self, zonefile_type: ZonefileType) -> Result<PathBuf> {
        let endpoint = zonefile_type.endpoint();
        let url = format!(
            "{}/{}/{}",
            self.base_url, self.token, zonefile_type.url_path()
        );

        info!(endpoint = endpoint, "Downloading zonefile");

        // Download ZIP to temp file
        let zip_path = self.download_dir.join(format!("{}.zip", endpoint));
        self.download_file(&url, &zip_path).await?;

        // Extract data file from ZIP
        let ext = if zonefile_type.is_detailed() {
            "csv"
        } else {
            "txt"
        };
        let extracted_path = self.download_dir.join(format!("{}.{}", endpoint, ext));
        self.extract_data_file(&zip_path, &extracted_path, zonefile_type)
            .await?;

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

    /// Extract data file from a ZIP archive
    ///
    /// Supports both .txt files (plain zonefiles) and .csv files (detailed zonefiles).
    async fn extract_data_file(
        &self,
        zip_path: &Path,
        output_path: &Path,
        zonefile_type: ZonefileType,
    ) -> Result<()> {
        use async_zip::tokio::read::fs::ZipFileReader;
        use tokio_util::compat::FuturesAsyncReadCompatExt;

        let reader = ZipFileReader::new(zip_path)
            .await
            .map_err(|e| Error::Zip(e.to_string()))?;

        // Find the data file in the archive
        let entries = reader.file().entries();
        let mut domains_txt_idx = None;
        let mut csv_fallback_idx = None;
        let mut txt_fallback_idx = None;

        for (idx, entry) in entries.iter().enumerate() {
            let filename = entry
                .filename()
                .as_str()
                .map_err(|e| Error::Zip(e.to_string()))?;

            // Skip readme files
            if filename.to_lowercase().contains("readme") {
                continue;
            }

            // Priority 1: domains.txt (plain mode classic)
            if filename == "domains.txt" || filename.ends_with("/domains.txt") {
                domains_txt_idx = Some(idx);
            }

            // Priority 2: any .csv file (detailed mode)
            if filename.ends_with(".csv") && csv_fallback_idx.is_none() {
                csv_fallback_idx = Some(idx);
            }

            // Priority 3: any .txt file (fallback for daily updates with different names)
            if filename.ends_with(".txt") && txt_fallback_idx.is_none() {
                txt_fallback_idx = Some(idx);
            }
        }

        // Choose the best match based on zonefile type
        let idx = if zonefile_type.is_detailed() {
            // For detailed types, prefer CSV files
            csv_fallback_idx
                .or(domains_txt_idx)
                .or(txt_fallback_idx)
        } else {
            // For plain types, prefer domains.txt then any .txt
            domains_txt_idx
                .or(txt_fallback_idx)
                .or(csv_fallback_idx)
        }
        .ok_or_else(|| {
            Error::InvalidZonefile("No .txt or .csv file found in archive".to_string())
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
            "Extracted data file"
        );

        Ok(())
    }

    /// Download directly to memory (for smaller files like daily updates)
    pub async fn download_to_memory(&self, zonefile_type: ZonefileType) -> Result<Vec<u8>> {
        let endpoint = zonefile_type.endpoint();
        let url = format!(
            "{}/{}/{}",
            self.base_url, self.token, zonefile_type.url_path()
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
        assert_eq!(ZonefileType::DetailedFull.endpoint(), "detailed-full");
        assert_eq!(
            ZonefileType::DetailedDailyUpdate.endpoint(),
            "detailed-update"
        );
    }

    #[test]
    fn test_zonefile_type_url_path() {
        assert_eq!(ZonefileType::Full.url_path(), "get/full/list/zip");
        assert_eq!(
            ZonefileType::DetailedFull.url_path(),
            "get-detailed/full/list/zip"
        );
        assert_eq!(
            ZonefileType::DetailedDailyUpdate.url_path(),
            "get-detailed/full-update/list/zip"
        );
    }

    #[test]
    fn test_zonefile_type_is_detailed() {
        assert!(!ZonefileType::Full.is_detailed());
        assert!(!ZonefileType::DailyUpdate.is_detailed());
        assert!(!ZonefileType::DailyRemove.is_detailed());
        assert!(ZonefileType::DetailedFull.is_detailed());
        assert!(ZonefileType::DetailedDailyUpdate.is_detailed());
    }
}
