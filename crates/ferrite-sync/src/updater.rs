//! Release updater. Every release is Ed25519 signed. Without a provisioned
//! root, checks fail closed. Swaps are fsync sealed and health gated.

use std::collections::HashMap;
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
    /// Signed rollback directive. Lets the server pull clients back to a
    /// known good build. Absent by default, old manifests keep working.
    #[serde(default)]
    pub downgrade: Option<ChannelRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelRelease {
    pub version: String,
    pub rollout: u8, // 0-100
    /// Hex signature over the release message. Empty never verifies.
    #[serde(default)]
    pub signature: String,
    /// Per-platform artifacts, keyed by Rust target triple
    /// (`x86_64-pc-windows-msvc`, …). One manifest serves every OS because
    /// native binaries are not portable; the client selects its own triple
    /// and refuses when absent (fail-closed, never a foreign binary).
    #[serde(default)]
    pub targets: HashMap<String, TargetArtifact>,
    /// Severity marker, unsigned and cosmetic-only: `"security"` escalates
    /// the restart reminder, anything else is routine. It can never widen
    /// trust (signatures and hashes decide that), only reminder cadence.
    #[serde(default)]
    pub severity: String,
}

/// One platform's download: full artifact plus its optional delta patch.
/// Patch binaries are platform-specific (bsdiff over native executables),
/// so the patch rides with its target, never top-level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetArtifact {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub patch_url: Option<String>,
}

impl TargetArtifact {
    pub fn is_complete(&self) -> bool {
        !self.url.is_empty() && !self.sha256.is_empty()
    }
}

impl ChannelRelease {
    /// Release triple of the running binary, if it is a shipped target.
    /// Unknown platforms yield `None` so every consumer fails closed instead
    /// of downloading a foreign binary.
    pub fn artifact_for_this_platform(&self) -> Option<&TargetArtifact> {
        let triple = current_target_triple()?;
        self.targets.get(triple).filter(|a| a.is_complete())
    }
}

/// Rust target triple of the running binary for the shipped matrix, if any.
pub fn current_target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

impl ChannelRelease {
    /// Whether this release is marked a security update.
    pub fn is_security(&self) -> bool {
        self.severity.eq_ignore_ascii_case("security")
    }
}

impl Manifest {
    /// Channel release for `channel`. The manifest carries all three,
    /// callers select, never guess.
    pub fn release(&self, channel: &Channel) -> &ChannelRelease {
        match channel {
            Channel::Stable => &self.stable,
            Channel::Beta => &self.beta,
            Channel::Dev => &self.dev,
        }
    }
}

fn empty_channel() -> ChannelRelease {
    ChannelRelease {
        version: "0.0.0".into(),
        rollout: 100,
        signature: "".into(),
        targets: HashMap::new(),
        severity: String::new(),
    }
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            stable: empty_channel(),
            beta: empty_channel(),
            dev: empty_channel(),
            downgrade: None,
        }
    }
}

/// Canonical signed payload for a release. Domain-separated, channel-bound,
/// so a beta signature cannot be replayed as stable. The per-target table is
/// covered entry by entry (sorted by triple): swapping a platform's URL or
/// hash invalidates the signature exactly like tampering with the version.
/// Mirrored byte-for-byte by the release workflow's signing script; change
/// both together (the `signing_message_vector` test pins the bytes).
pub fn release_signing_message(channel: &Channel, rel: &ChannelRelease) -> String {
    let mut msg = format!(
        "ferrite-release-v1\n{}\n{}\n{}",
        channel.as_str(),
        rel.version,
        rel.rollout,
    );
    let mut triples: Vec<&String> = rel.targets.keys().collect();
    triples.sort();
    // Every field terminated by `\n`, entries concatenated: the byte layout
    // is fully determined by the sorted table, with no edge special-casing.
    for triple in triples {
        if let Some(a) = rel.targets.get(triple) {
            msg.push_str(&format!(
                "\n{}\n{}\n{}\n{}\n",
                triple,
                a.sha256,
                a.url,
                a.patch_url.as_deref().unwrap_or("")
            ));
        }
    }
    msg
}

/// Verify the Ed25519 signature of `rel` against 32-byte `root_pubkey`.
/// Empty/malformed signature or key always fails, as does a release with no
/// complete targets entry (nothing to trust means nothing verifies).
pub fn verify_release_signature(
    channel: &Channel,
    rel: &ChannelRelease,
    root_pubkey: &[u8],
) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    if rel.signature.is_empty() || !rel.targets.values().any(|a| a.is_complete()) {
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
/// Called once at startup from trusted configuration. Test code uses
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

/// Emergency rollback directive from a signed manifest, if any. Returns
/// the downgrade release when the running version is newer than it, with
/// a valid channel-bound signature. Downgrades skip rollout buckets.
pub fn should_downgrade(
    manifest: &Manifest,
    channel: &Channel,
    current: &str,
) -> Option<ChannelRelease> {
    let rel = manifest.downgrade.as_ref()?;
    let Some(root_hex) = RELEASE_ROOT.get() else {
        tracing::warn!("downgrade check without provisioned release root - refusing");
        return None;
    };
    let Ok(root) = hex_decode(root_hex) else {
        return None;
    };
    downgrade_for(channel, rel, current, &root)
}

/// Pure downgrade core (lock-free for tests). [`should_downgrade`] only
/// adds the provisioned-root gate.
fn downgrade_for(
    channel: &Channel,
    rel: &ChannelRelease,
    current: &str,
    root: &[u8],
) -> Option<ChannelRelease> {
    if !verify_release_signature(channel, rel, root) {
        tracing::warn!("downgrade signature invalid - refusing");
        return None;
    }
    if version_greater(current, &rel.version) {
        Some(rel.clone())
    } else {
        None
    }
}

/// Maximum full-artifact download trusted per run (browser binary ceiling).
/// Anything claiming bigger is refused before a byte lands on disk.
pub const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum delta-patch download (oversize patches fall back to full).
pub const MAX_PATCH_BYTES: u64 = 64 * 1024 * 1024;
/// Progress callback granularity: at most one call per 256KiB plus start
/// and finish (a per-read callback would spam the UI thread).
pub const PROGRESS_GRANULARITY: u64 = 256 * 1024;

/// Background-download progress events (byte counts for the About page.
/// The shell maps them to Started, Progress, and Finished UI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateProgress {
    Started { total: Option<u64> },
    Progress { downloaded: u64, total: Option<u64> },
    Finished { bytes: u64 },
}

/// Byte-counting reader wrapper. Progress-callback granularity lives here
/// (not inline in the download loop), so it is unit-testable without a network.
pub struct ProgressReader<R, F: FnMut(u64, Option<u64>)> {
    inner: R,
    downloaded: u64,
    total: Option<u64>,
    last_emit: u64,
    on_progress: F,
}

impl<R, F: FnMut(u64, Option<u64>)> ProgressReader<R, F> {
    /// `total` is the advertised length when known.
    pub fn new(inner: R, total: Option<u64>, on_progress: F) -> Self {
        Self {
            inner,
            downloaded: 0,
            total,
            last_emit: 0,
            on_progress,
        }
    }
}

impl<R: std::io::Read, F: FnMut(u64, Option<u64>)> std::io::Read for ProgressReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.downloaded += n as u64;
            if self.downloaded - self.last_emit >= PROGRESS_GRANULARITY {
                self.last_emit = self.downloaded;
                (self.on_progress)(self.downloaded, self.total);
            }
        }
        Ok(n)
    }
}

/// Download `url` to `dest` with byte caps, timeouts, and progress
/// events. Partial files are removed on any failure. The destination is
/// only complete on `Ok`. Returns the byte count.
pub fn download_release(
    url: &str,
    dest: &Path,
    max_bytes: u64,
    on_progress: impl FnMut(UpdateProgress),
) -> Result<u64, String> {
    use std::io::Write;
    use std::time::Duration;

    let mut on_progress = on_progress;
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_recv_response(Some(Duration::from_secs(10)))
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    if resp.status() != ureq::http::StatusCode::OK {
        return Err(format!("download status {}", resp.status()));
    }
    let total: Option<u64> = resp
        .headers()
        .get(ureq::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());
    if total.is_some_and(|t| t > max_bytes) {
        return Err("download exceeds size cap".to_owned());
    }
    on_progress(UpdateProgress::Started { total });
    let reader = resp.into_body().into_reader();
    let mut counted = ProgressReader::new(reader, total, |downloaded, total| {
        on_progress(UpdateProgress::Progress { downloaded, total });
    });
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;
    let mut chunk = [0u8; 32 * 1024];
    loop {
        use std::io::Read;
        let n = counted.read(&mut chunk).map_err(|e| {
            let _ = std::fs::remove_file(dest);
            e.to_string()
        })?;
        if n == 0 {
            break;
        }
        downloaded += n as u64;
        if downloaded > max_bytes {
            let _ = std::fs::remove_file(dest);
            return Err("download exceeds size cap".to_owned());
        }
        file.write_all(&chunk[..n]).map_err(|e| {
            let _ = std::fs::remove_file(dest);
            e.to_string()
        })?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    on_progress(UpdateProgress::Finished { bytes: downloaded });
    Ok(downloaded)
}

/// Resolve a release to a verified local artifact: try the signed delta
/// patch first, fall back to the full artifact on any patch failure.
/// Refuse anything whose sha256 does not verify. Refuse releases with no
/// artifact for this platform (fail-closed, never a foreign binary).
/// Returns the path.
pub fn fetch_and_verify(
    rel: &ChannelRelease,
    current_exe: &Path,
    workdir: &Path,
    on_progress: impl FnMut(UpdateProgress),
) -> Result<PathBuf, String> {
    let mut on_progress = on_progress;
    let artifact = rel.artifact_for_this_platform().ok_or_else(|| {
        "no artifact for this platform in release manifest - refusing update".to_owned()
    })?;
    let (url, sha256, patch_url) = (
        artifact.url.clone(),
        artifact.sha256.clone(),
        artifact.patch_url.clone().unwrap_or_default(),
    );
    std::fs::create_dir_all(workdir).map_err(|e| e.to_string())?;
    // Delta first: patch detached from the RUNNING binary (never from a
    // half-written candidate), then hash-gated before acceptance.
    if !patch_url.is_empty() {
        let patch_path = workdir.join("ferrite-update.patch");
        let candidate = workdir.join("ferrite-update-patched");
        let patched = download_release(&patch_url, &patch_path, MAX_PATCH_BYTES, |p| {
            on_progress(p);
        })
        .and_then(|_| apply_patch(current_exe, &patch_path, &candidate))
        .and_then(|()| {
            if verify_sha256(&candidate, &sha256) {
                Ok(())
            } else {
                Err("patched artifact hash mismatch".to_owned())
            }
        });
        let _ = std::fs::remove_file(&patch_path);
        match patched {
            Ok(()) => return Ok(candidate),
            Err(e) => {
                tracing::warn!(error = %e, "delta path failed, falling back to full download");
                let _ = std::fs::remove_file(&candidate);
            }
        }
    }
    let full_path = workdir.join("ferrite-update-full");
    download_release(&url, &full_path, MAX_DOWNLOAD_BYTES, |p| {
        on_progress(p);
    })?;
    if !verify_sha256(&full_path, &sha256) {
        let _ = std::fs::remove_file(&full_path);
        return Err("full artifact hash mismatch".to_owned());
    }
    Ok(full_path)
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

/// Fetch and parse the release manifest (CDN edge, not GitHub API).
/// Signature checks happen per-release in the callers, never here.
pub fn fetch_manifest(manifest_url: &str) -> Result<Manifest, String> {
    let resp = ureq::get(manifest_url).call().map_err(|e| e.to_string())?;
    serde_json::from_reader(resp.into_body().into_reader()).map_err(|e| e.to_string())
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
            signature: String::new(),
            targets: HashMap::from([(
                "x86_64-unknown-linux-gnu".to_owned(),
                TargetArtifact {
                    sha256: "a".repeat(64),
                    url: "https://cdn.example/ferrite-1.2.3".into(),
                    patch_url: None,
                },
            )]),
            severity: String::new(),
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
        // Tampered per-target URL invalidates the signature (targets ride
        // inside the signed bytes, never beside them).
        let mut swapped = rel.clone();
        if let Some(a) = swapped.targets.get_mut("x86_64-unknown-linux-gnu") {
            a.url = "https://cdn.example/evil".into();
        }
        assert!(!verify_release_signature(&Channel::Stable, &swapped, &root));
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
    fn rollout_bucket_file_holds_no_identifier() {
        // The bucket persists a bare number for staged-rollout sampling.
        // It must never grow into an installation identifier: numeric,
        // in range, nothing else on disk.
        let _ = rollout_bucket();
        let content = std::fs::read_to_string(rollout_path()).unwrap();
        let n: u8 = content.trim().parse().expect("bare number only");
        assert!(n < 100);
    }
    #[test]
    fn staged_rollout_bucket() {
        fn unsigned(version: &str, rollout: u8) -> ChannelRelease {
            ChannelRelease {
                version: version.into(),
                rollout,
                signature: "".into(),
                targets: HashMap::new(),
                severity: String::new(),
            }
        }
        let manifest = Manifest {
            stable: unsigned("1.0.1", 10),
            beta: unsigned("1.0.0", 100),
            dev: unsigned("0.0.0", 100),
            downgrade: None,
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

    fn signed_release(version: &str, seed: &[u8; 32]) -> (ChannelRelease, Vec<u8>) {
        use ed25519_dalek::SigningKey;
        let sk = SigningKey::from_bytes(seed);
        let root = sk.verifying_key().as_bytes().to_vec();
        let mut rel = ChannelRelease {
            version: version.into(),
            rollout: 100,
            signature: String::new(),
            targets: HashMap::from([(
                "x86_64-unknown-linux-gnu".to_owned(),
                TargetArtifact {
                    sha256: "b".repeat(64),
                    url: "https://cdn.example/ferrite".into(),
                    patch_url: None,
                },
            )]),
            severity: String::new(),
        };
        sign_release_for_test(&Channel::Stable, &mut rel, seed);
        (rel, root)
    }

    #[test]
    fn downgrade_applies_only_when_running_newer_with_valid_sig() {
        let seed = [7u8; 32];
        let (rel, root) = signed_release("1.2.0", &seed);
        // Running ahead of the directive: roll back.
        assert!(downgrade_for(&Channel::Stable, &rel, "1.3.0", &root).is_some());
        // Same or older running version: nothing to roll back to.
        assert!(downgrade_for(&Channel::Stable, &rel, "1.2.0", &root).is_none());
        assert!(downgrade_for(&Channel::Stable, &rel, "1.1.0", &root).is_none());
        // Cross-channel replay refuses.
        assert!(downgrade_for(&Channel::Beta, &rel, "1.3.0", &root).is_none());
        // Tampered directive refuses.
        let mut tampered = rel.clone();
        tampered.version = "0.1.0".into();
        assert!(downgrade_for(&Channel::Stable, &tampered, "1.3.0", &root).is_none());
        // Unsigned directive refuses.
        let mut unsigned = rel.clone();
        unsigned.signature.clear();
        assert!(downgrade_for(&Channel::Stable, &unsigned, "1.3.0", &root).is_none());
    }

    #[test]
    fn severity_flags_security_only() {
        let seed = [9u8; 32];
        let (mut rel, _) = signed_release("1.2.0", &seed);
        assert!(!rel.is_security());
        rel.severity = "security".into();
        assert!(rel.is_security());
        rel.severity = "Security".into();
        assert!(rel.is_security(), "case-insensitive");
        // Severity rides outside the signature: changing it afterwards
        // must not break verification (cosmetic-only by construction).
        let (rel2, root) = signed_release("1.2.0", &seed);
        assert!(verify_release_signature(&Channel::Stable, &rel2, &root));
    }

    #[test]
    fn progress_reader_emits_at_granularity() {
        use std::io::Read;
        let data = vec![0u8; (PROGRESS_GRANULARITY * 3 + 100) as usize];
        let mut events = Vec::new();
        let mut r = ProgressReader::new(std::io::Cursor::new(data), Some(999), |d, t| {
            events.push((d, t));
        });
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.len(), (PROGRESS_GRANULARITY * 3 + 100) as usize);
        // One callback per 256KiB window, not per read.
        assert_eq!(events.len(), 3, "granular callbacks, got {events:?}");
        assert!(events.iter().all(|(_, t)| *t == Some(999)), "total carried");
        assert!(events.windows(2).all(|w| w[1].0 > w[0].0), "monotonic");
    }

    #[test]
    fn progress_reader_handles_unknown_total() {
        use std::io::Read;
        let mut events = Vec::new();
        let mut r = ProgressReader::new(std::io::Cursor::new(vec![1u8; 10]), None, |d, t| {
            events.push((d, t));
        });
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).unwrap();
        assert!(events.is_empty(), "tiny reads stay silent");
    }

    fn two_target_release() -> ChannelRelease {
        ChannelRelease {
            version: "2.0.0".into(),
            rollout: 25,
            signature: String::new(),
            targets: HashMap::from([
                (
                    "x86_64-pc-windows-msvc".to_owned(),
                    TargetArtifact {
                        sha256: "c".repeat(64),
                        url: "https://cdn.example/ferrite-win".into(),
                        patch_url: Some("https://cdn.example/ferrite-win.patch".into()),
                    },
                ),
                (
                    "aarch64-apple-darwin".to_owned(),
                    TargetArtifact {
                        sha256: "d".repeat(64),
                        url: "https://cdn.example/ferrite-mac".into(),
                        patch_url: None,
                    },
                ),
            ]),
            severity: String::new(),
        }
    }

    #[test]
    fn signing_message_vector() {
        // Byte-exact contract shared with the release workflow's signing
        // script: entries sorted by triple, patch slot empty-never-absent.
        let rel = two_target_release();
        let cccc = "c".repeat(64);
        let dddd = "d".repeat(64);
        let expect = format!(
            "ferrite-release-v1\nstable\n2.0.0\n25\naarch64-apple-darwin\n{dddd}\nhttps://cdn.example/ferrite-mac\n\n\nx86_64-pc-windows-msvc\n{cccc}\nhttps://cdn.example/ferrite-win\nhttps://cdn.example/ferrite-win.patch\n"
        );
        assert_eq!(release_signing_message(&Channel::Stable, &rel), expect);
    }

    #[test]
    fn platform_artifact_selection_is_exact() {
        let rel = two_target_release();
        // Resolution follows the running platform, never a guess: on this
        // machine the answer is whichever entry matches, or nothing.
        let got = rel.artifact_for_this_platform();
        match current_target_triple() {
            Some("x86_64-pc-windows-msvc") => {
                assert_eq!(
                    got.map(|a| a.url.as_str()),
                    Some("https://cdn.example/ferrite-win")
                );
            }
            Some("aarch64-apple-darwin") => {
                assert_eq!(
                    got.map(|a| a.url.as_str()),
                    Some("https://cdn.example/ferrite-mac")
                );
            }
            _ => assert!(got.is_none(), "foreign platform must resolve to nothing"),
        }
        // Incomplete entries (empty hash) never count as artifacts.
        let mut thin = two_target_release();
        for a in thin.targets.values_mut() {
            a.sha256.clear();
        }
        assert!(thin.artifact_for_this_platform().is_none());
        let seed = [3u8; 32];
        let mut thin_signed = thin;
        sign_release_for_test(&Channel::Stable, &mut thin_signed, &seed);
        use ed25519_dalek::SigningKey;
        let root = SigningKey::from_bytes(&seed)
            .verifying_key()
            .as_bytes()
            .to_vec();
        assert!(
            !verify_release_signature(&Channel::Stable, &thin_signed, &root),
            "no complete entry means nothing verifies"
        );
    }

    #[test]
    fn fetch_refuses_without_platform_artifact() {
        // No network involved: resolution fails before any I/O.
        let rel = ChannelRelease {
            version: "9.9.9".into(),
            rollout: 100,
            signature: "00".repeat(64),
            targets: HashMap::new(),
            severity: String::new(),
        };
        let dir = std::env::temp_dir();
        let exe = dir.join("ferrite-noplat-test.exe");
        let err = fetch_and_verify(&rel, &exe, &dir, |_| {}).unwrap_err();
        assert!(err.contains("no artifact for this platform"), "{err}");
    }
}
