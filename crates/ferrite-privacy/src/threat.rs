//! Threat intel: hash lookup.

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

pub fn hash_domain(domain: &str) -> String {
    let normalized = domain.trim().to_lowercase();
    Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone)]
pub struct ThreatDb {
    inner: Arc<Mutex<Connection>>,
    // In-memory hash set mirrors SQLite so `contains` never touches the
    // database on the proxy hot path (no blocking Mutex+query per request).
    cache: Arc<RwLock<std::collections::HashSet<String>>>,
}

impl Default for ThreatDb {
    fn default() -> Self {
        Self::new_in_memory().expect("threat db default")
    }
}

impl ThreatDb {
    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS threats(hash TEXT PRIMARY KEY) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);",
        )
        .map_err(|e| format!("threat schema: {e}"))?;
        Ok(())
    }

    pub fn new_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("threat open mem: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            cache: Arc::new(RwLock::new(std::collections::HashSet::new())),
        })
    }

    pub fn new_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let p = path.as_ref();
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("threat mkdir: {e}"))?;
            }
        }
        let conn = Connection::open(p).map_err(|e| format!("threat open {}: {e}", p.display()))?;
        // WAL + synchronous NORMAL - durable enough for threat cache, faster.
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;");
        Self::init_schema(&conn)?;
        // Warm the in-memory mirror from existing rows (best-effort).
        let cached: std::collections::HashSet<String> = conn
            .prepare("SELECT hash FROM threats")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(0))
                    .and_then(|rows| rows.collect::<Result<_, _>>())
            })
            .map(|v: Vec<String>| v.into_iter().collect())
            .unwrap_or_default();
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            cache: Arc::new(RwLock::new(cached)),
        })
    }

    pub fn load_snapshot(&self, urls: &[&str]) -> Result<(), String> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| format!("lock poisoned: {e}"))?;
        conn.execute_batch("BEGIN IMMEDIATE; DELETE FROM threats;")
            .map_err(|e| format!("threat begin: {e}"))?;
        let mut ok = true;
        let mut err_msg = String::new();
        let mut hashes: Vec<String> = Vec::with_capacity(urls.len());
        {
            let mut stmt =
                match conn.prepare_cached("INSERT OR IGNORE INTO threats(hash) VALUES(?1)") {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = conn.execute_batch("ROLLBACK;");
                        return Err(format!("threat prepare: {e}"));
                    }
                };
            for url in urls {
                let host = normalize_host(url);
                let hash = hash_domain(&host);
                if let Err(e) = stmt.execute([hash.clone()]) {
                    ok = false;
                    err_msg = format!("threat insert: {e}");
                    break;
                }
                hashes.push(hash);
            }
        }
        if ok {
            conn.execute_batch("COMMIT;")
                .map_err(|e| format!("threat commit: {e}"))?;
            if let Ok(mut cache) = self.cache.write() {
                *cache = hashes.into_iter().collect();
            }
            Ok(())
        } else {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(err_msg)
        }
    }

    pub fn contains(&self, domain: &str) -> bool {
        let hash = hash_domain(&normalize_host(domain));
        if let Ok(cache) = self.cache.read() {
            if !cache.is_empty() {
                return cache.contains(&hash);
            }
        }
        // Fallback to SQLite only when the mirror is empty (fresh file DB
        // or poisoned cache path): preserves old behavior for tests.
        let Ok(conn) = self.inner.lock() else {
            return false;
        };
        let Ok(mut stmt) = conn.prepare_cached("SELECT 1 FROM threats WHERE hash=?1 LIMIT 1")
        else {
            return false;
        };
        stmt.exists([hash]).unwrap_or(false)
    }

    /// Suffix-aware check: `sub.evil.com` matches an `evil.com` entry.
    /// Normalizes input (URL → host) and walks parent domains.
    pub fn contains_with_suffixes(&self, domain: &str) -> bool {
        let host = normalize_host(domain).to_lowercase();
        if host.is_empty() {
            return false;
        }
        // Fast path: in-memory mirror.
        if let Ok(cache) = self.cache.read() {
            if !cache.is_empty() {
                let mut cur = host.as_str();
                loop {
                    if cache.contains(&hash_domain(cur)) {
                        return true;
                    }
                    match cur.find('.') {
                        Some(i) => cur = &cur[i + 1..],
                        None => return false,
                    }
                }
            }
        }
        // SQLite fallback mirrors the same walk.
        let Ok(conn) = self.inner.lock() else {
            return false;
        };
        let Ok(mut stmt) = conn.prepare_cached("SELECT 1 FROM threats WHERE hash=?1 LIMIT 1")
        else {
            return false;
        };
        let mut cur = host.as_str();
        loop {
            if stmt.exists([hash_domain(cur)]).unwrap_or(false) {
                return true;
            }
            match cur.find('.') {
                Some(i) => cur = &cur[i + 1..],
                None => return false,
            }
        }
    }

    pub fn len(&self) -> usize {
        let Ok(conn) = self.inner.lock() else {
            return 0;
        };
        conn.query_row("SELECT COUNT(*) FROM threats", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn normalize_host(input: &str) -> String {
    let trimmed = input.trim();
    if let Ok(url) = url::Url::parse(trimmed) {
        if let Some(host) = url.host_str() {
            return host.to_lowercase();
        }
    }
    if let Ok(url) = url::Url::parse(&format!("http://{trimmed}")) {
        if let Some(host) = url.host_str() {
            return host.to_lowercase();
        }
    }
    trimmed
        .split(&['/', '?', '#'][..])
        .next()
        .unwrap_or(trimmed)
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash_domain("Example.COM"), hash_domain("example.com"));
        assert_ne!(hash_domain("a.com"), hash_domain("b.com"));
    }

    #[test]
    fn load_and_contains() {
        let db = ThreatDb::new_in_memory().unwrap();
        db.load_snapshot(&["http://phishing.test/login", "https://malware.test/evil"])
            .unwrap();
        assert!(db.contains("phishing.test"));
        assert!(db.contains("PHISHING.TEST"));
        assert!(db.contains("malware.test"));
        assert!(!db.contains("example.com"));
        assert_eq!(db.len(), 2);
    }

    #[test]
    fn normalize_host_extracts() {
        assert_eq!(
            normalize_host("https://example.com/path?q=1"),
            "example.com"
        );
        assert_eq!(normalize_host("example.com/path"), "example.com");
        assert_eq!(normalize_host("sub.example.com"), "sub.example.com");
    }

    #[test]
    fn subdomain_matches_parent_entry() {
        let db = ThreatDb::new_in_memory().unwrap();
        db.load_snapshot(&["evil.com"]).unwrap();
        assert!(db.contains_with_suffixes("sub.evil.com"));
        assert!(db.contains_with_suffixes("deep.sub.evil.com"));
        assert!(db.contains_with_suffixes("EVIL.COM"));
        assert!(!db.contains_with_suffixes("not-evil.com"));
        assert!(!db.contains_with_suffixes("evil.com.evil.org"));
    }

    #[test]
    fn contains_normalizes_url_input() {
        let db = ThreatDb::new_in_memory().unwrap();
        db.load_snapshot(&["https://phishing.test/login?q=1"])
            .unwrap();
        assert!(db.contains("phishing.test"));
        assert!(db.contains("https://phishing.test/other"));
        assert!(db.contains_with_suffixes("https://sub.phishing.test/x"));
    }

    #[test]
    fn reload_replaces() {
        let db = ThreatDb::new_in_memory().unwrap();
        db.load_snapshot(&["a.test"]).unwrap();
        assert_eq!(db.len(), 1);
        db.load_snapshot(&["b.test", "c.test"]).unwrap();
        assert_eq!(db.len(), 2);
        assert!(!db.contains("a.test"));
    }
}
