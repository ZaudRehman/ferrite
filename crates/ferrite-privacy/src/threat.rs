//! Threat intel: hash lookup.

use std::path::Path;
use std::sync::{Arc, Mutex};

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
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
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
                if let Err(e) = stmt.execute([hash]) {
                    ok = false;
                    err_msg = format!("threat insert: {e}");
                    break;
                }
            }
        }
        if ok {
            conn.execute_batch("COMMIT;")
                .map_err(|e| format!("threat commit: {e}"))?;
            Ok(())
        } else {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(err_msg)
        }
    }

    pub fn contains(&self, domain: &str) -> bool {
        let hash = hash_domain(domain);
        let Ok(conn) = self.inner.lock() else {
            return false;
        };
        let Ok(mut stmt) = conn.prepare_cached("SELECT 1 FROM threats WHERE hash=?1 LIMIT 1")
        else {
            return false;
        };
        stmt.exists([hash]).unwrap_or(false)
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
    fn reload_replaces() {
        let db = ThreatDb::new_in_memory().unwrap();
        db.load_snapshot(&["a.test"]).unwrap();
        assert_eq!(db.len(), 1);
        db.load_snapshot(&["b.test", "c.test"]).unwrap();
        assert_eq!(db.len(), 2);
        assert!(!db.contains("a.test"));
    }
}
