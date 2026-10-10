//! Temp file rendezvous for PDF bitmap transfer. Deleted on drop.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

/// Max bitmap bytes (64 MB).
pub const MAX_SHM_BYTES: usize = 64 * 1024 * 1024;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ShmHandle {
    pub path: PathBuf,
    pub size: usize,
}

impl ShmHandle {
    /// Fails on squatted names instead of truncating. Owner only perms.
    pub fn create(size: usize) -> std::io::Result<Self> {
        if size == 0 || size > MAX_SHM_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "shm size out of bounds",
            ));
        }
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ferrite-pdf-{}-{}-{}.shm",
            std::process::id(),
            nanos,
            nonce
        ));
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            opts.mode(0o600);
        }
        #[cfg(windows)]
        {
            opts.share_mode(0);
        }
        let file = opts.open(&path)?;
        file.set_len(size as u64)?;
        Ok(Self { path, size })
    }

    /// Accept only `.shm` names inside the temp dir.
    pub fn validate_path(raw: &str) -> Option<PathBuf> {
        if raw.is_empty() || raw.len() > 512 || raw.contains("..") {
            return None;
        }
        let p = Path::new(raw);
        if p.extension().and_then(|e| e.to_str()) != Some("shm") {
            return None;
        }
        let tmp = std::env::temp_dir();
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            tmp.join(p)
        };
        // Canonicalize the PARENT (file need not exist yet on create side;
        // on read side require containment).
        let parent = joined.parent()?;
        let canon_parent = parent.canonicalize().ok()?;
        let canon_tmp = tmp.canonicalize().unwrap_or(tmp);
        if canon_parent.starts_with(&canon_tmp) {
            Some(joined)
        } else {
            None
        }
    }

    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for ShmHandle {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_sizes() {
        assert!(ShmHandle::create(0).is_err());
        assert!(ShmHandle::create(MAX_SHM_BYTES + 1).is_err());
    }

    #[test]
    fn create_and_cleanup_roundtrip() {
        let h = ShmHandle::create(4096).unwrap();
        assert!(h.path.exists());
        let meta = std::fs::metadata(&h.path).unwrap();
        assert_eq!(meta.len(), 4096);
        let path = h.path.clone();
        drop(h);
        assert!(!path.exists());
    }

    #[test]
    fn validate_path_confines_to_temp() {
        assert!(ShmHandle::validate_path("").is_none());
        assert!(ShmHandle::validate_path("../evil.shm").is_none());
        assert!(ShmHandle::validate_path("/etc/passwd").is_none());
        assert!(ShmHandle::validate_path("out.bin").is_none());
        let ok = ShmHandle::validate_path("ferrite-pdf-1-2-3.shm");
        assert!(ok.is_some());
    }
}
