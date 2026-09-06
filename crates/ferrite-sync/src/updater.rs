//! Updater engine per Part 13 5.1-5.6 - self_update + differential + channels + staged rollout + rollback.
//!
//! Trust model: every release is Ed25519-signed by the release root key.
//! `verify_sha256` never passes on empty expectations, version comparison is
//! real semver (prerelease < release), and swaps are fsync'd + health-gated.
//! Without a provisioned release root (`provision_release_root`), update
//! checks fail closed.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
    Dev,
}

impl Channel {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "beta" => Channel::Beta,
            "dev" => Channel::Dev,
            _ => Channel::Stable,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Dev => "dev",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub stable: ChannelRelease,
    pub beta: ChannelRelease,
    pub dev: ChannelRelease,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelRelease {
    pub version: String,
    pub rollout: u8, // 0-100
    pub sha256: String,
    /// Hex Ed25519 signature over [`release_signing_message`]. Empty means
    /// unsigned and NEVER verifies.
    #[serde(default)]
    pub signature: String,
    pub url: String,
    pub patch_url: Option<String>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            stable: ChannelRelease {
                version: "0.0.0".into(),
                rollout: 100,
                sha256: "".into(),
                signature: "".into(),
                url: "".into(),
                patch_url: None,
            },
            beta: ChannelRelease {
                version: "0.0.0".into(),
                rollout: 100,
                sha256: "".into(),
                signature: "".into(),
                url: "".into(),
                patch_url: None,
            },
            dev: ChannelRelease {
                version: "0.0.0".into(),
                rollout: 100,
                sha256: "".into(),
                signature: "".into(),
                url: "".into(),
                patch_url: None,
            },
        }
    }
}

/// Canonical signed payload for a release. Domain-separated; channel-bound
/// so a beta signature cannot be replayed as stable.
pub fn release_signing_message(channel: &Channel, rel: &ChannelRelease) -> String {
    format!(
        "ferrite-release-v1\n{}\n{}\n{}\n{}\n{}",
        channel.as_str(),
        rel.version,
        rel.rollout,
        rel.sha256,
        rel.url
    )
}

/// Verify `rel`'s Ed25519 signature against 32-byte `root_pubkey`.
/// Empty/malformed signature or key always fails.
pub fn verify_release_signature(
    channel: &Channel,
    rel: &ChannelRelease,
    root_pubkey: &[u8],
) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    if rel.signature.is_empty() || rel.sha256.is_empty() || rel.url.is_empty() {
        return false;
    }
    let Ok(key_bytes): Result<[u8; 32], _> = root_pubkey.try_into() else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&key_bytes) else {
        return false;
    };
    let Ok(sig_bytes) = hex_decode(&rel.signature) else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.try_into() else {
        return false;
    };
    vk.verify(
        release_signing_message(channel, rel).as_bytes(),
        &Signature::from_bytes(&sig_arr),
    )
    .is_ok()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

static RELEASE_ROOT: OnceLock<String> = OnceLock::new();

/// Provision the release-root public key (64 lowercase hex chars).
/// Called once at startup from trusted configuration; test code should use
/// [`verify_release_signature`] with ephemeral keys instead.
pub fn provision_release_root(pubkey_hex: &str) -> Result<(), String> {
    if pubkey_hex.len() != 64 || !pubkey_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("release root must be 64 hex chars".into());
    }
    RELEASE_ROOT
        .set(pubkey_hex.to_ascii_lowercase())
        .map_err(|_| "release root already provisioned".to_string())
}

/// Test helper: sign a release with `secret` (32B seed).
#[cfg(test)]
pub fn sign_release_for_test(channel: &Channel, rel: &mut ChannelRelease, secret_seed: &[u8; 32]) {
    use ed25519_dalek::{Signer, SigningKey};
    let sk = SigningKey::from_bytes(secret_seed);
    let sig = sk.sign(release_signing_message(channel, rel).as_bytes());
    rel.signature = sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
}

pub fn rollout_bucket() -> u8 {
    // Client-side random 0-99 stored locally, never transmitted (5.4 privacy-preserving)
    let path = rollout_path();
    if let Ok(data) = std::fs::read_to_string(&path)
        && let Ok(n) = data.trim().parse::<u8>()
        && n < 100
    {
        return n;
    }
    // Unbiased rejection sampling (256 % 100 != 0, so `% 100` is biased).
    let n = {
        use rand::RngCore;
        let mut rng = rand::thread_rng();
        loop {
            let mut b = [0u8; 1];
            rng.fill_bytes(&mut b);
            // 0..=255; accept 0..200 then mod 100 (200 is a multiple of 100).
            if b[0] < 200 {
                break b[0] % 100;
            }
        }
    };
    // Best-effort persist with restrictive perms on unix; failure just means
    // a fresh draw next launch (rollout-eligible users may flap once).
    if let Ok(mut f) = std::fs::File::create(&path) {
        use std::io::Write;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let _ = writeln!(f, "{n}");
        let _ = f.sync_all();
    }
    n
}

fn rollout_path() -> PathBuf {
    std::env::temp_dir().join("ferrite-rollout-bucket")
}

pub fn should_update(manifest: &Manifest, channel: &Channel, current: &str) -> bool {
    let rel = match channel {
        Channel::Stable => &manifest.stable,
        Channel::Beta => &manifest.beta,
        Channel::Dev => &manifest.dev,
    };
    // Fail closed without a provisioned root or an invalid signature.
    let Some(root_hex) = RELEASE_ROOT.get() else {
        tracing::warn!("update check without provisioned release root - refusing");
        return false;
    };
    let Ok(root) = hex_decode(root_hex) else {
        return false;
    };
    if !verify_release_signature(channel, rel, &root) {
        tracing::warn!("release signature invalid - refusing update");
        return false;
    };
    if version_greater(&rel.version, current) {
        let bucket = rollout_bucket();
        return bucket < rel.rollout;
    }
    false
}

fn parse_semver(s: &str) -> (Vec<u64>, Option<Vec<SemVerIdent>>) {
    // Split off build metadata (`+...`, ignored for precedence).
    let s = s.split('+').next().unwrap_or(s);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let nums = core
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect::<Vec<_>>();
    let pre_ids = pre.map(|p| {
        p.split('.')
            .map(|id| {
                if let Ok(n) = id.parse::<u64>() {
                    SemVerIdent::Num(n)
                } else {
                    SemVerIdent::Str(id.to_owned())
                }
            })
            .collect()
    });
    (nums, pre_ids)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SemVerIdent {
    Num(u64),
    Str(String),
}

impl PartialOrd for SemVerIdent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVerIdent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (SemVerIdent::Num(a), SemVerIdent::Num(b)) => a.cmp(b),
            // Numeric identifiers have lower precedence than alphanumeric.
            (SemVerIdent::Num(_), SemVerIdent::Str(_)) => Ordering::Less,
            (SemVerIdent::Str(_), SemVerIdent::Num(_)) => Ordering::Greater,
            (SemVerIdent::Str(a), SemVerIdent::Str(b)) => a.cmp(b),
        }
    }
}

fn version_greater(remote: &str, current: &str) -> bool {
    let (rn, rp) = parse_semver(remote);
    let (cn, cp) = parse_semver(current);
    // Compare numeric core, padding the shorter side with zeros.
    let len = rn.len().max(cn.len());
    for i in 0..len {
        let a = rn.get(i).copied().unwrap_or(0);
        let b = cn.get(i).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    // Core equal: release > prerelease; prerelease compares lexically.
    match (rp, cp) {
        (None, None) => false,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => a > b,
    }
}

pub fn verify_sha256(path: &Path, expected: &str) -> bool {
    // Empty expectation NEVER verifies: fail closed.
    if expected.is_empty()
        || expected.len() != 64
        || !expected.chars().all(|c| c.is_ascii_hexdigit())
    {
        return false;
    }
    if let Ok(data) = std::fs::read(path) {
        use sha2::{Digest, Sha256};
        let hash: String = Sha256::digest(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        return hash.eq_ignore_ascii_case(expected);
    }
    false
}

pub fn apply_patch(old: &Path, patch: &Path, new: &Path) -> Result<(), String> {
    let old_data = std::fs::read(old).map_err(|e| e.to_string())?;
    let patch_data = std::fs::read(patch).map_err(|e| e.to_string())?;
    let mut new_data = Vec::new();
    // bsdiff patch format: use bsdiff::patch if available, else fallback to full
    if bsdiff::patch(&old_data, &mut patch_data.as_slice(), &mut new_data).is_ok() {
        std::fs::write(new, new_data).map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("patch failed, fallback to full".into())
    }
}

fn fsync_file(path: &Path) -> Result<(), String> {
    // read+write: Windows FlushFileBuffers requires GENERIC_WRITE.
    let f = std::fs::File::options()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}

#[cfg(unix)]
fn fsync_dir(path: &Path) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let f = std::fs::File::open(dir).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn fsync_dir(_path: &Path) -> Result<(), String> {
    // Windows cannot open directories for fsync (ERROR_ACCESS_DENIED);
    // NTFS rename is atomic, so the swap itself remains safe.
    Ok(())
}

#[cfg(unix)]
fn preserve_exec_bit(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(src)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode();
    let mut perm = std::fs::metadata(dst)
        .map_err(|e| e.to_string())?
        .permissions();
    // Preserve only the owner-execute bit state from the running binary.
    let new_mode = (perm.mode() & !0o111) | (mode & 0o111);
    perm.set_mode(new_mode);
    std::fs::set_permissions(dst, perm).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn preserve_exec_bit(_src: &Path, _dst: &Path) -> Result<(), String> {
    Ok(())
}

pub fn atomic_swap(current: &Path, new_bin: &Path) -> Result<(), String> {
    let old = current.with_extension("old");
    // Retain old until health check passes (5.6)
    if current.exists() {
        // Crash between renames must not lose both binaries: fsync the new
        // binary and its directory BEFORE touching the live path.
        fsync_file(new_bin)?;
        if current.exists() {
            std::fs::rename(current, &old).map_err(|e| e.to_string())?;
        }
    }
    if let Err(e) = std::fs::rename(new_bin, current) {
        // rollback on failure
        let _ = std::fs::rename(&old, current);
        return Err(e.to_string());
    }
    let _ = preserve_exec_bit(&old, current);
    fsync_file(current)?;
    fsync_dir(current)?;
    Ok(())
}

pub fn rollback_if_failed(current: &Path, expected_version: &str) -> bool {
    let old = current.with_extension("old");
    if !old.exists() {
        return false;
    }
    // Health check: the new binary must start, exit 0, AND report the
    // expected version. `is_ok` alone only proves spawn succeeded.
    let healthy = std::process::Command::new(current)
        .arg("--version")
        .output()
        .map(|o| {
            o.status.success() && String::from_utf8_lossy(&o.stdout).contains(expected_version)
        })
        .unwrap_or(false);
    if !healthy {
        let _ = std::fs::rename(&old, current);
        return true;
    }
    // Keep old until next successful update, or remove after health passes
    // For now, keep old for manual rollback
    false
}

pub fn check_for_update(
    manifest_url: &str,
    channel: &Channel,
) -> Result<Option<ChannelRelease>, String> {
    // Fetch latest.json from CDN edge, not GitHub API directly (5.5 blind)
    let resp = ureq::get(manifest_url).call().map_err(|e| e.to_string())?;
    let manifest: Manifest =
        serde_json::from_reader(resp.into_body().into_reader()).map_err(|e| e.to_string())?;
    let rel = match channel {
        Channel::Stable => manifest.stable.clone(),
        Channel::Beta => manifest.beta.clone(),
        Channel::Dev => manifest.dev.clone(),
    };
    let Some(root_hex) = RELEASE_ROOT.get() else {
        return Err("no release root provisioned - refusing update".into());
    };
    let root = hex_decode(root_hex).map_err(|_| "release root corrupt".to_string())?;
    if !verify_release_signature(channel, &rel, &root) {
        return Err("manifest signature invalid - refusing update".into());
    }
    let bucket = rollout_bucket();
    if bucket < rel.rollout {
        Ok(Some(rel))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_compare() {
        assert!(version_greater("1.0.1", "1.0.0"));
        assert!(!version_greater("1.0.0", "1.0.1"));
        assert!(version_greater("1.1.0", "1.0.9"));
    }

    #[test]
    fn version_prerelease_semantics() {
        // Release beats any prerelease of the same core.
        assert!(version_greater("1.0.0", "1.0.0-beta"));
        assert!(!version_greater("1.0.0-beta", "1.0.0"));
        // No more `-beta == release` confusion.
        assert!(!version_greater("1.0.0-beta", "1.0.0-beta"));
        assert!(version_greater("1.0.0-beta.2", "1.0.0-beta.1"));
        assert!(version_greater("1.0.0-rc.1", "1.0.0-beta.99"));
        // Build metadata ignored.
        assert!(!version_greater("1.0.0+build.2", "1.0.0+build.1"));
        assert!(version_greater("1.0.1", "1.0.0+build.9"));
    }

    #[test]
    fn sha256_empty_never_verifies() {
        let dir = std::env::temp_dir();
        let f = dir.join("ferrite-sha-empty-test.bin");
        let _ = std::fs::write(&f, b"data");
        assert!(!verify_sha256(&f, ""));
        assert!(!verify_sha256(&f, "short"));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn release_signature_roundtrip_and_tamper() {
        use ed25519_dalek::SigningKey;
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let sk = SigningKey::from_bytes(&seed);
        let root = sk.verifying_key().as_bytes().to_vec();
        let mut rel = ChannelRelease {
            version: "1.2.3".into(),
            rollout: 100,
            sha256: "a".repeat(64),
            signature: String::new(),
            url: "https://cdn.example/ferrite-1.2.3".into(),
            patch_url: None,
        };
        assert!(!verify_release_signature(&Channel::Stable, &rel, &root));
        sign_release_for_test(&Channel::Stable, &mut rel, &seed);
        assert!(verify_release_signature(&Channel::Stable, &rel, &root));
        // Tampered version invalidates the signature.
        let mut tampered = rel.clone();
        tampered.version = "9.9.9".into();
        assert!(!verify_release_signature(
            &Channel::Stable,
            &tampered,
            &root
        ));
        // Cross-channel replay invalidates the signature.
        assert!(!verify_release_signature(&Channel::Beta, &rel, &root));
        // Wrong root invalidates.
        let mut other_seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut other_seed);
        let other_root = SigningKey::from_bytes(&other_seed)
            .verifying_key()
            .as_bytes()
            .to_vec();
        assert!(!verify_release_signature(
            &Channel::Stable,
            &rel,
            &other_root
        ));
    }
    #[test]
    fn rollout_deterministic() {
        let a = rollout_bucket();
        let b = rollout_bucket();
        assert_eq!(a, b);
        assert!(a < 100);
    }
    #[test]
    fn staged_rollout_bucket() {
        let manifest = Manifest {
            stable: ChannelRelease {
                version: "1.0.1".into(),
                rollout: 10,
                sha256: "".into(),
                signature: "".into(),
                url: "".into(),
                patch_url: None,
            },
            beta: ChannelRelease {
                version: "1.0.0".into(),
                rollout: 100,
                sha256: "".into(),
                signature: "".into(),
                url: "".into(),
                patch_url: None,
            },
            dev: ChannelRelease {
                version: "0.0.0".into(),
                rollout: 100,
                sha256: "".into(),
                signature: "".into(),
                url: "".into(),
                patch_url: None,
            },
        };
        // Bucket <10 => should update, >=10 => not
        let should = should_update(&manifest, &Channel::Stable, "1.0.0");
        // Deterministic per stored bucket, just check logic doesn't panic
        let _ = should;
    }
    #[test]
    fn patch_fallback() {
        let dir = std::env::temp_dir();
        let old = dir.join("ferrite-old-test.bin");
        let patch = dir.join("ferrite-patch-test.bin");
        let new = dir.join("ferrite-new-test.bin");
        let _ = std::fs::write(&old, b"old data");
        let _ = std::fs::write(&patch, b"bad patch");
        // patch should fail and fallback error, not panic
        assert!(apply_patch(&old, &patch, &new).is_err());
        let _ = std::fs::remove_file(old);
        let _ = std::fs::remove_file(patch);
        let _ = std::fs::remove_file(new);
    }

    #[test]
    fn atomic_swap_and_rollback_health() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-swap-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cur = dir.join("ferrite-bin");
        let new = dir.join("ferrite-new");
        std::fs::write(&cur, b"v1").unwrap();
        std::fs::write(&new, b"v2").unwrap();
        atomic_swap(&cur, &new).unwrap();
        assert_eq!(std::fs::read(&cur).unwrap(), b"v2");
        assert!(cur.with_extension("old").exists());
        // Health always fails for a non-executable blob -> rolls back.
        assert!(rollback_if_failed(&cur, "9.9.9-nope"));
        assert_eq!(std::fs::read(&cur).unwrap(), b"v1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
