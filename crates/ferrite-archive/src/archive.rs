//! Stored snapshots with FTS search and LRU retention. Served rows are
//! always sanitized. Decompress is capped at 10 MB per snapshot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use crate::error::{ArchiveError, ArchiveResult};
use crate::sanitize::{extract_text, sanitize};
use crate::storage::Storage;

pub type SnapshotId = i64;

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: SnapshotId,
    pub url: String,
    pub timestamp: i64,
    pub headers: String,
    pub html: String,
    pub is_pinned: bool,
}

#[derive(Debug, Clone)]
pub struct ArchiveConfig {
    pub path: String,
    pub max_bytes: i64,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            path: "ferrite-archive.db".to_string(),
            max_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

/// Clone shares the same store and index.
#[derive(Debug, Clone)]
pub struct Archive {
    storage: Storage,
    pub(crate) config: ArchiveConfig,
    time_machine: Arc<DashMap<String, Vec<(String, i64)>>>,
    /// Failed retention evictions (disk-full, lock errors). Surfaced, never
    /// swallowed: a stuck evictor grows the DB without bound, and the only
    /// symptom would otherwise be a full disk months later.
    evict_errors: Arc<std::sync::atomic::AtomicU64>,
}

impl Archive {
    /// File store under `key` (the device master key; borrowed, never
    /// retained; the open consumes what it needs and the caller zeroizes
    /// its copy). Empty keys open plaintext: only the boot gate may pass
    /// one, and only for explicitly chosen ephemeral stores.
    pub fn open(config: ArchiveConfig, key: &[u8]) -> ArchiveResult<Self> {
        let storage = Storage::open(&config.path, key)?;
        Ok(Self {
            storage,
            config,
            time_machine: Arc::new(DashMap::new()),
            evict_errors: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    pub fn open_in_memory() -> ArchiveResult<Self> {
        let storage = Storage::open_in_memory()?;
        Ok(Self {
            storage,
            config: ArchiveConfig {
                path: ":memory:".to_string(),
                max_bytes: 2 * 1024 * 1024 * 1024,
            },
            time_machine: Arc::new(DashMap::new()),
            evict_errors: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Whether this archive persists to disk (in-memory stores skip
    /// rotation: there is nothing keyed to rekey).
    pub fn is_persistent(&self) -> bool {
        self.config.path != ":memory:"
    }

    /// Re-encrypt under `new_key`. See [`Storage::rekey`] for the contract.
    pub fn rekey(&self, new_key: &[u8]) -> ArchiveResult<()> {
        self.storage.rekey(self.is_persistent(), new_key)
    }

    /// Reclaim free pages. See [`Storage::reclaim_space`] for the contract.
    pub fn reclaim_space(&self) -> ArchiveResult<usize> {
        self.storage.reclaim_space()
    }

    /// Sanitize, compress (level 3), store, then evict.
    pub fn ingest(&self, url: &str, headers: &str, html: &[u8]) -> ArchiveResult<SnapshotId> {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_owned()));
        let html_str = String::from_utf8_lossy(html).into_owned();
        let sanitized = sanitize(&html_str, host.as_deref());
        let text = extract_text(&sanitized);
        let zstd_blob = zstd::bulk::compress(sanitized.as_bytes(), 3)
            .map_err(|e| ArchiveError::Compression(e.to_string()))?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size = zstd_blob.len() as i64;
        let headers_owned = headers.to_owned();
        let id = self.storage.insert_snapshot(
            url,
            timestamp,
            &headers_owned,
            &zstd_blob,
            size,
            false,
            &text,
        )?;
        self.time_machine
            .entry(url.to_owned())
            .or_default()
            .push((url.to_owned(), timestamp));
        if let Some(mut entry) = self.time_machine.get_mut(url) {
            if entry.len() > 50 {
                let len = entry.len();
                entry.drain(0..len - 50);
            }
        }
        // Bounded hint: 50 per URL, 5000 URLs.
        if self.time_machine.len() > 5000 {
            let excess: Vec<String> = self
                .time_machine
                .iter()
                .take(self.time_machine.len() - 5000)
                .map(|e| e.key().clone())
                .collect();
            for k in excess {
                self.time_machine.remove(&k);
            }
        }
        if let Err(e) = self.storage.evict_lru(self.config.max_bytes) {
            self.evict_errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(error = %e, "archive retention eviction failed");
        }
        Ok(id)
    }

    /// Lifetime count of failed retention evictions (diagnostics surface).
    pub fn evict_errors(&self) -> u64 {
        self.evict_errors.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Store without sanitizing or indexing. Invisible to reads.
    pub fn ingest_unsanitized(
        &self,
        url: &str,
        headers: &str,
        html: &[u8],
    ) -> ArchiveResult<SnapshotId> {
        let html_str = String::from_utf8_lossy(html).into_owned();
        let zstd_blob = zstd::bulk::compress(html_str.as_bytes(), 3)
            .map_err(|e| ArchiveError::Compression(e.to_string()))?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size = zstd_blob.len() as i64;
        self.storage
            .insert_unsanitized(url, timestamp, headers, &zstd_blob, size)
    }

    pub fn get_latest(&self, url: &str) -> ArchiveResult<Option<Snapshot>> {
        let Some((id, blob, headers, ts, is_pinned)) = self.storage.get_latest(url)? else {
            return Ok(None);
        };
        let html_bytes = zstd::bulk::decompress(&blob, 10 * 1024 * 1024)
            .map_err(|e| ArchiveError::Compression(e.to_string()))?;
        let html = String::from_utf8_lossy(&html_bytes).into_owned();
        Ok(Some(Snapshot {
            id,
            url: url.to_owned(),
            timestamp: ts,
            headers,
            html,
            is_pinned,
        }))
    }

    /// Fallback when the exact URL missed. Literal substring match.
    pub fn get_fuzzy(&self, domain: &str) -> ArchiveResult<Option<Snapshot>> {
        let Some((id, url, blob, headers, timestamp)) = self.storage.query_fuzzy_domain(domain)?
        else {
            return Ok(None);
        };
        let html_bytes = zstd::bulk::decompress(&blob, 10 * 1024 * 1024)
            .map_err(|e| ArchiveError::Compression(e.to_string()))?;
        let html = String::from_utf8_lossy(&html_bytes).into_owned();
        let is_pinned = self.storage.is_pinned(id).unwrap_or(false);
        Ok(Some(Snapshot {
            id,
            url,
            timestamp,
            headers,
            html,
            is_pinned,
        }))
    }

    /// Phrase search. Operators are literals. Limit clamped to 100.
    pub fn search(&self, query: &str, limit: usize) -> ArchiveResult<Vec<(String, String)>> {
        self.storage.search_fts(query, limit)
    }

    pub fn list_for_url(&self, url: &str) -> ArchiveResult<Vec<(i64, i64)>> {
        self.storage.list_for_url(url)
    }

    pub fn pin(&self, id: i64, pinned: bool) -> ArchiveResult<()> {
        self.storage.set_pinned(id, pinned)
    }

    pub fn delete(&self, id: i64) -> ArchiveResult<()> {
        self.storage.delete_snapshot(id)
    }

    pub fn is_pinned(&self, id: i64) -> ArchiveResult<bool> {
        self.storage.is_pinned(id)
    }

    pub fn export(&self, id: i64) -> ArchiveResult<(String, Vec<u8>)> {
        let (url, blob) = self.storage.export_snapshot(id)?;
        let html = zstd::bulk::decompress(&blob, 10 * 1024 * 1024)
            .map_err(|e| ArchiveError::Compression(e.to_string()))?;
        Ok((url, html))
    }

    pub fn total_size(&self) -> ArchiveResult<i64> {
        self.storage.total_size()
    }

    pub fn time_machine_for(&self, domain: &str) -> Vec<(String, i64)> {
        self.time_machine
            .get(domain)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    pub fn time_machine_all(&self) -> HashMap<String, Vec<(String, i64)>> {
        let mut out = HashMap::new();
        for entry in self.time_machine.iter() {
            out.insert(entry.key().clone(), entry.value().clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_and_retrieve() {
        let arch = Archive::open_in_memory().unwrap();
        let html = b"<html><body><p>Hello</p><script>alert(1)</script></body></html>";
        let id = arch
            .ingest("https://example.com/a", "content-type: text/html", html)
            .unwrap();
        assert!(id > 0);
        let snap = arch.get_latest("https://example.com/a").unwrap().unwrap();
        assert!(snap.html.contains("Hello"));
        assert!(!snap.html.contains("script"));
    }

    #[test]
    fn unsanitized_not_in_latest() {
        let arch = Archive::open_in_memory().unwrap();
        let html = b"<script>evil</script>";
        arch.ingest_unsanitized("https://example.com/b", "", html)
            .unwrap();
        assert!(arch.get_latest("https://example.com/b").unwrap().is_none());
    }

    #[test]
    fn fts_search() {
        let arch = Archive::open_in_memory().unwrap();
        arch.ingest(
            "https://example.com/c",
            "",
            b"<p>carbon capture article about climate</p>",
        )
        .unwrap();
        arch.ingest("https://example.com/d", "", b"<p>unrelated cooking</p>")
            .unwrap();
        let res = arch.search("carbon capture", 5).unwrap();
        assert_eq!(res.len(), 1);
        assert!(res[0].0.contains("example.com/c"));
    }

    #[test]
    fn pin_prevents_evict() {
        let arch = Archive::open_in_memory().unwrap();
        let cfg = ArchiveConfig {
            path: ":memory:".to_string(),
            max_bytes: 100,
        };
        let mut small = arch.clone();
        small.config = cfg;
        // ingest large to trigger evict
        small.ingest("https://a.com/1", "", &[b'x'; 200]).unwrap();
        let id = small.ingest("https://a.com/2", "", &[b'y'; 200]).unwrap();
        small.pin(id, true).unwrap();
        // next ingest should evict non-pinned only
        small.ingest("https://a.com/3", "", &[b'z'; 200]).unwrap();
        // pinned still exists
        assert!(small.storage.list_for_url("https://a.com/2").unwrap().len() == 1);
    }

    #[test]
    fn evict_counter_starts_at_zero_and_clones_share_it() {
        let arch = Archive::open_in_memory().unwrap();
        assert_eq!(arch.evict_errors(), 0);
        arch.ingest("https://example.com/e", "", b"<p>fine</p>")
            .unwrap();
        assert_eq!(arch.evict_errors(), 0, "healthy evictions never count");
        // Clones share the store, index, and the error counter.
        assert_eq!(arch.clone().evict_errors(), 0);
    }

    #[test]
    fn get_latest_returns_real_pinned_value() {
        let arch = Archive::open_in_memory().unwrap();
        let id = arch
            .ingest("https://example.com/pinned", "", b"<p>hi</p>")
            .unwrap();
        assert!(
            !arch
                .get_latest("https://example.com/pinned")
                .unwrap()
                .unwrap()
                .is_pinned
        );
        arch.pin(id, true).unwrap();
        assert!(
            arch.get_latest("https://example.com/pinned")
                .unwrap()
                .unwrap()
                .is_pinned
        );
        arch.pin(id, false).unwrap();
        assert!(
            !arch
                .get_latest("https://example.com/pinned")
                .unwrap()
                .unwrap()
                .is_pinned
        );
    }
}
