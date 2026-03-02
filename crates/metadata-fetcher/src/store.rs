use crate::error::Result;
use crate::model::{DomainMetadataResult, PageMetadataResult};
use std::path::Path;
use tokio_rusqlite::Connection;
use tracing::info;

/// Lightweight row for importing page metadata into Tantivy.
/// Only the fields we actually store in the index.
#[derive(Debug, Clone)]
pub struct PageMetaRow {
    pub page_title: Option<String>,
    pub meta_description: Option<String>,
    pub og_image: Option<String>,
    pub snippet: Option<String>,
    pub language: Option<String>,
}

/// Load all domains with page metadata from SQLite (sync, for importer).
/// Returns (domain, PageMetaRow) pairs where page_title IS NOT NULL.
pub fn load_domains_with_page_metadata(
    db_path: &Path,
) -> std::result::Result<Vec<(String, PageMetaRow)>, rusqlite::Error> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA cache_size = -65536;")?;

    let mut stmt = conn.prepare(
        "SELECT domain, page_title, meta_description, og_image, snippet, language
         FROM page_metadata
         WHERE page_title IS NOT NULL",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            PageMetaRow {
                page_title: row.get(1)?,
                meta_description: row.get(2)?,
                og_image: row.get(3)?,
                snippet: row.get(4)?,
                language: row.get(5)?,
            },
        ))
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

#[derive(Clone)]
pub struct MetadataStore {
    conn: Connection,
}

impl MetadataStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open(path).await?;

        conn.call(|conn| {
            Ok(conn.execute_batch(
                "
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA cache_size = -65536;

                CREATE TABLE IF NOT EXISTS results (
                    domain              TEXT PRIMARY KEY,
                    robots_found        INTEGER NOT NULL DEFAULT 0,
                    robots_status       INTEGER NOT NULL DEFAULT 0,
                    sitemap_url         TEXT,
                    sitemap_found       INTEGER NOT NULL DEFAULT 0,
                    sitemap_status      INTEGER,
                    url_count           INTEGER NOT NULL DEFAULT 0,
                    url_count_estimated INTEGER NOT NULL DEFAULT 0,
                    latest_lastmod      TEXT,
                    oldest_lastmod      TEXT,
                    is_sitemap_index    INTEGER NOT NULL DEFAULT 0,
                    child_sitemap_count INTEGER NOT NULL DEFAULT 0,
                    crawl_delay         REAL,
                    cms_hint            TEXT,
                    site_category       TEXT,
                    fetched_at          INTEGER NOT NULL,
                    error               TEXT
                );

                CREATE TABLE IF NOT EXISTS crawl_state (
                    id          INTEGER PRIMARY KEY CHECK (id = 1),
                    started_at  INTEGER NOT NULL,
                    updated_at  INTEGER NOT NULL,
                    total       INTEGER NOT NULL DEFAULT 0,
                    done        INTEGER NOT NULL DEFAULT 0
                );

                CREATE INDEX IF NOT EXISTS idx_fetched_at ON results(fetched_at);

                CREATE TABLE IF NOT EXISTS page_metadata (
                    domain           TEXT PRIMARY KEY,
                    http_status      INTEGER NOT NULL DEFAULT 0,
                    page_title       TEXT,
                    meta_description TEXT,
                    og_title         TEXT,
                    og_description   TEXT,
                    og_image         TEXT,
                    canonical_url    TEXT,
                    language         TEXT,
                    generator        TEXT,
                    snippet          TEXT,
                    content_type     TEXT,
                    fetched_at       INTEGER NOT NULL,
                    source           TEXT NOT NULL DEFAULT 'direct',
                    error            TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_page_fetched ON page_metadata(fetched_at);
                CREATE INDEX IF NOT EXISTS idx_page_source ON page_metadata(source);
            ",
            )?)
        })
        .await?;

        info!("SQLite store initialized");
        Ok(Self { conn })
    }

    pub async fn is_done(&self, domain: &str) -> Result<bool> {
        let domain = domain.to_string();
        let count: i64 = self
            .conn
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM results WHERE domain = ?1",
                    rusqlite::params![domain],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(count > 0)
    }

    pub async fn upsert_batch(&self, results: Vec<DomainMetadataResult>) -> Result<()> {
        self.conn
            .call(move |conn| {
                let tx = conn.unchecked_transaction()?;
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT OR REPLACE INTO results (
                        domain, robots_found, robots_status, sitemap_url,
                        sitemap_found, sitemap_status, url_count, url_count_estimated,
                        latest_lastmod, oldest_lastmod, is_sitemap_index, child_sitemap_count,
                        crawl_delay, cms_hint, site_category, fetched_at, error
                    ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                    )?;
                    for r in &results {
                        stmt.execute(rusqlite::params![
                            r.domain,
                            r.robots_found as i64,
                            r.robots_status as i64,
                            r.sitemap_url,
                            r.sitemap_found as i64,
                            r.sitemap_status.map(|s| s as i64),
                            r.url_count as i64,
                            r.url_count_estimated as i64,
                            r.latest_lastmod,
                            r.oldest_lastmod,
                            r.is_sitemap_index as i64,
                            r.child_sitemap_count as i64,
                            r.crawl_delay.map(|d| d as f64),
                            r.cms_hint,
                            r.site_category,
                            r.fetched_at,
                            r.error,
                        ])?;
                    }
                }
                Ok(tx.commit()?)
            })
            .await?;
        Ok(())
    }

    pub async fn update_state(&self, total: u64, done: u64) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO crawl_state (id, started_at, updated_at, total, done)
                 VALUES (1, ?1, ?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                     updated_at = excluded.updated_at,
                     total = excluded.total,
                     done = excluded.done",
                    rusqlite::params![now, total as i64, done as i64],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn read_stats(&self) -> Result<CrawlStats> {
        let stats = self
            .conn
            .call(|conn| {
                let total_results: i64 = conn
                    .query_row("SELECT COUNT(*) FROM results", [], |r| r.get(0))
                    .unwrap_or(0);
                let robots_found: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM results WHERE robots_found = 1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let sitemap_found: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM results WHERE sitemap_found = 1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let errors: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM results WHERE error IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let state: Option<(i64, i64, i64)> = conn
                    .query_row(
                        "SELECT total, done, updated_at FROM crawl_state WHERE id = 1",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .ok();

                let mut cms_counts: Vec<(String, i64)> = Vec::new();
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT cms_hint, COUNT(*) as cnt FROM results WHERE cms_hint IS NOT NULL GROUP BY cms_hint ORDER BY cnt DESC",
                ) {
                    if let Ok(rows) = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))) {
                        for row in rows.flatten() {
                            cms_counts.push(row);
                        }
                    }
                }

                Ok(CrawlStats {
                    total_results,
                    robots_found,
                    sitemap_found,
                    errors,
                    crawl_total: state.map(|s| s.0).unwrap_or(0),
                    crawl_done: state.map(|s| s.1).unwrap_or(0),
                    last_updated: state.map(|s| s.2).unwrap_or(0),
                    cms_counts,
                })
            })
            .await?;
        Ok(stats)
    }

    pub async fn count_done(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .call(|conn| {
                Ok(conn
                    .query_row("SELECT COUNT(*) FROM results", [], |r| r.get(0))?)
            })
            .await?;
        Ok(count as u64)
    }

    // --- Page metadata methods ---

    pub async fn is_page_done(&self, domain: &str) -> Result<bool> {
        let domain = domain.to_string();
        let count: i64 = self
            .conn
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM page_metadata WHERE domain = ?1",
                    rusqlite::params![domain],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(count > 0)
    }

    pub async fn upsert_page_batch(&self, results: Vec<PageMetadataResult>) -> Result<()> {
        self.conn
            .call(move |conn| {
                let tx = conn.unchecked_transaction()?;
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT OR REPLACE INTO page_metadata (
                        domain, http_status, page_title, meta_description,
                        og_title, og_description, og_image, canonical_url,
                        language, generator, snippet, content_type,
                        fetched_at, source, error
                    ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    )?;
                    for r in &results {
                        stmt.execute(rusqlite::params![
                            r.domain,
                            r.http_status as i64,
                            r.page_title,
                            r.meta_description,
                            r.og_title,
                            r.og_description,
                            r.og_image,
                            r.canonical_url,
                            r.language,
                            r.generator,
                            r.snippet,
                            r.content_type,
                            r.fetched_at,
                            r.source,
                            r.error,
                        ])?;
                    }
                }
                Ok(tx.commit()?)
            })
            .await?;
        Ok(())
    }

    pub async fn read_page_stats(&self) -> Result<PageCrawlStats> {
        let stats = self
            .conn
            .call(|conn| {
                let total: i64 = conn
                    .query_row("SELECT COUNT(*) FROM page_metadata", [], |r| r.get(0))
                    .unwrap_or(0);
                let with_title: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM page_metadata WHERE page_title IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let with_description: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM page_metadata WHERE meta_description IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let with_og: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM page_metadata WHERE og_title IS NOT NULL OR og_description IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let with_snippet: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM page_metadata WHERE snippet IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let errors: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM page_metadata WHERE error IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);

                let mut lang_counts: Vec<(String, i64)> = Vec::new();
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT language, COUNT(*) as cnt FROM page_metadata WHERE language IS NOT NULL GROUP BY language ORDER BY cnt DESC LIMIT 20",
                ) {
                    if let Ok(rows) = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))) {
                        for row in rows.flatten() {
                            lang_counts.push(row);
                        }
                    }
                }

                let mut source_counts: Vec<(String, i64)> = Vec::new();
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT source, COUNT(*) as cnt FROM page_metadata GROUP BY source ORDER BY cnt DESC",
                ) {
                    if let Ok(rows) = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))) {
                        for row in rows.flatten() {
                            source_counts.push(row);
                        }
                    }
                }

                Ok(PageCrawlStats {
                    total,
                    with_title,
                    with_description,
                    with_og,
                    with_snippet,
                    errors,
                    lang_counts,
                    source_counts,
                })
            })
            .await?;
        Ok(stats)
    }

    pub async fn count_pages_done(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .call(|conn| {
                Ok(conn
                    .query_row("SELECT COUNT(*) FROM page_metadata", [], |r| r.get(0))?)
            })
            .await?;
        Ok(count as u64)
    }
}

#[derive(Debug)]
pub struct PageCrawlStats {
    pub total: i64,
    pub with_title: i64,
    pub with_description: i64,
    pub with_og: i64,
    pub with_snippet: i64,
    pub errors: i64,
    pub lang_counts: Vec<(String, i64)>,
    pub source_counts: Vec<(String, i64)>,
}

#[derive(Debug)]
pub struct CrawlStats {
    pub total_results: i64,
    pub robots_found: i64,
    pub sitemap_found: i64,
    pub errors: i64,
    pub crawl_total: i64,
    pub crawl_done: i64,
    pub last_updated: i64,
    pub cms_counts: Vec<(String, i64)>,
}
