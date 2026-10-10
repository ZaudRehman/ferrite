//! Self update lifecycle: staged apply at boot, background check, restart
//! surfacing. Verification cannot be disabled and lives in `ferrite_sync`.
//! This module owns scheduling and user surfacing only.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ferrite_sync::updater::{
    Channel, ChannelRelease, UpdateProgress, atomic_swap, fetch_and_verify, fetch_manifest,
    provision_release_root, rollback_if_failed, should_downgrade, should_update,
    verify_release_signature, verify_sha256,
};

/// Hourly check cadence. Manual checks bypass it.
pub const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(3600);
/// First check delay after boot (staggered, non-blocking): day-one CVEs
/// must not wait out the hourly throttle on short sessions. The tick
/// still owns the cadence; this only backdates the initial timestamp.
pub const FIRST_UPDATE_CHECK_DELAY: Duration = Duration::from_secs(45);
/// Restart reminder cadence for staged security updates.
pub const SECURITY_REMINDER_INTERVAL: Duration = Duration::from_secs(1800);

/// Post-update marker file: `{from,to}` versions, consumed once at the
/// next boot to open About with an update banner. Plain JSON (versions
/// only, no secrets); validated on read, deleted on consume.
pub const UPDATED_MARKER_NAME: &str = "ferrite-updated.json";

/// Version shape for the update marker: dotted/dashed alphanumerics,
/// capped (a hostile file must not inject markup into the banner).
fn clean_version(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty()
        || t.len() > 32
        || !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+')
    {
        return None;
    }
    Some(t.to_owned())
}

/// Record an applied update for the next boot's banner. Best-effort:
/// failures only log (the update itself already landed).
pub fn note_updated_at_boot(from: &str, to: &str) {
    note_updated_in(&crate::profiles::data_root(), from, to);
}

pub(crate) fn note_updated_in(dir: &std::path::Path, from: &str, to: &str) {
    let (Some(f), Some(t)) = (clean_version(from), clean_version(to)) else {
        return;
    };
    if f == t {
        return;
    }
    let path = dir.join(UPDATED_MARKER_NAME);
    let doc = serde_json::json!({"from": f, "to": t}).to_string();
    if let Err(e) = std::fs::write(&path, doc) {
        tracing::debug!(error = %e, "update marker unwritable");
    }
}

/// Consume the post-update marker: validated `(from, to)`, file deleted
/// whether valid or not (one-shot by construction; a corrupt marker must
/// not re-fire every boot).
pub fn take_updated_marker() -> Option<(String, String)> {
    take_updated_in(&crate::profiles::data_root())
}

pub(crate) fn take_updated_in(dir: &std::path::Path) -> Option<(String, String)> {
    let path = dir.join(UPDATED_MARKER_NAME);
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let from = v
        .get("from")
        .and_then(|x| x.as_str())
        .and_then(clean_version)?;
    let to = v
        .get("to")
        .and_then(|x| x.as_str())
        .and_then(clean_version)?;
    if from == to {
        return None;
    }
    Some((from, to))
}

/// Manifest URL override.
pub const UPDATE_URL_ENV: &str = "FERRITE_UPDATE_URL";
/// Channel override. Default is stable.
pub const UPDATE_CHANNEL_ENV: &str = "FERRITE_CHANNEL";
/// Release root key (64 hex). Without it checks fail closed.
pub const RELEASE_ROOT_ENV: &str = "FERRITE_RELEASE_ROOT";
const DEFAULT_MANIFEST_URL: &str = "https://ferrite.browser/latest.json";
// Compile-time stable root, baked by release builds
// (`FERRITE_RELEASE_ROOT_BAKED` at compile time). Lets installed builds
// verify updates with no operator env step; env still overrides per boot
// (beta/dev testing, rotation). Empty in dev builds: fail-closed as before.
/// Staged artifact on disk (private shape, re-verified at boot).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StagedSidecar {
    version: String,
    channel: String,
    security: bool,
    sha256: String,
    signature: String,
    url: String,
}

/// Worker-to-tick events (bounded texture: progress carries counters, the
/// staged path travels only on completion).
#[derive(Debug)]
pub enum UpdateEvent {
    /// One progress tick from the downloader.
    Progress { downloaded: u64, total: Option<u64> },
    /// Cycle finished: staged and verified, up-to-date, or failed with a
    /// short reason. `manual` echoes whether the About button triggered it
    /// (failures toast only for manual checks, background failures log).
    Done {
        manual: bool,
        result: Result<UpdateOutcome, String>,
    },
}

#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// Staged, verified, awaiting restart.
    Staged { version: String, security: bool },
    /// Nothing newer (or rollout bucket missed): no action.
    UpToDate,
    /// Signed emergency rollback staged (older known-good build).
    RolledBack { version: String },
}

/// Restart-pending UI state (About hook + reminder scheduling).
#[derive(Debug, Default)]
pub enum UpdatePhase {
    /// No information yet (or checks disabled without a root).
    #[default]
    Idle,
    /// Worker running (manual or scheduled).
    Checking,
    Downloading {
        downloaded: u64,
        total: Option<u64>,
    },
    /// Verified artifact staged. Restart applies it at boot.
    Ready {
        version: String,
        security: bool,
        reminded_at: Instant,
    },
    /// Last cycle failed (short reason kept for the About page).
    Failed(String),
    UpToDate,
}

/// Staging directory for downloads and the sidecar. Temp-scoped, and the
/// sidecar re-verifies at boot, so a foreign file here cannot promote.
pub fn stage_dir() -> PathBuf {
    std::env::temp_dir().join("ferrite-update")
}

fn sidecar_path() -> PathBuf {
    stage_dir().join("ferrite-update-pending.json")
}

/// Manifest URL from env override or the release default.
pub fn manifest_url() -> String {
    match std::env::var(UPDATE_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().to_owned(),
        _ => DEFAULT_MANIFEST_URL.to_owned(),
    }
}

/// Release channel from env override (unknown values fall back to stable,
/// never to a dev channel by typo).
pub fn update_channel() -> Channel {
    match std::env::var(UPDATE_CHANNEL_ENV)
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .as_str()
    {
        "beta" => Channel::Beta,
        "dev" => Channel::Dev,
        _ => Channel::Stable,
    }
}

/// Where a release root comes from. Pure precedence for tests: explicit
/// env beats the bake, empty counts as unset on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootSource {
    Env(String),
    Baked(String),
    Missing,
}

fn resolve_root_source(env_val: Option<&str>, baked: Option<&str>) -> RootSource {
    match env_val.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => RootSource::Env(v.to_owned()),
        None => match baked.map(str::trim).filter(|s| !s.is_empty()) {
            Some(v) => RootSource::Baked(v.to_owned()),
            None => RootSource::Missing,
        },
    }
}

/// Provision the release root from the operator env var, falling back to
/// the baked-in stable root (release builds only). Returns `Ok(true)` when
/// checks are armed, `Ok(false)` when fail-closed. Precedence: explicit env
/// first (rotation/testing wins over the bake), then the bake. An invalid
/// env value is an error, never a silent fallthrough to the bake. The key
/// never touches the network.
pub fn provision_root_from_env() -> Result<bool, String> {
    match resolve_root_source(
        std::env::var(RELEASE_ROOT_ENV).ok().as_deref(),
        option_env!("FERRITE_RELEASE_ROOT_BAKED"),
    ) {
        RootSource::Env(key) | RootSource::Baked(key) => provision_release_root(key.trim())
            .map(|()| true)
            .map_err(|e| format!("release root invalid (check env or rebuild): {e}")),
        RootSource::Missing => Ok(false),
    }
}

/// One full background cycle: check, then delta-first fetch, then stage.
/// Runs on a worker thread. Posts progress and exactly one [`UpdateEvent::Done`].
pub fn run_update_cycle(
    manifest_url: &str,
    channel: &Channel,
    current_version: &str,
    current_exe: &std::path::Path,
    tx: std::sync::mpsc::Sender<UpdateEvent>,
    manual: bool,
) {
    let progress = |p: UpdateProgress| {
        if let UpdateProgress::Progress { downloaded, total } = p {
            let _ = tx.send(UpdateEvent::Progress { downloaded, total });
        }
    };
    let done = |result: Result<UpdateOutcome, String>| {
        let _ = tx.send(UpdateEvent::Done { manual, result });
    };
    // Upgrade path first; downgrade directive only when no upgrade pending.
    // The manifest is fetched once and judged through both signed gates
    // (both fail closed without a provisioned root).
    let manifest = match fetch_manifest(manifest_url) {
        Ok(m) => m,
        Err(e) => {
            done(Err(short_reason(&e)));
            return;
        }
    };
    let rel: Option<(ChannelRelease, bool)> = if should_update(&manifest, channel, current_version)
    {
        Some((manifest.release(channel).clone(), false))
    } else {
        should_downgrade(&manifest, channel, current_version).map(|rel| (rel, true))
    };
    let Some((rel, is_downgrade)) = rel else {
        done(Ok(UpdateOutcome::UpToDate));
        return;
    };
    // Redundant safety alongside the manifest gates: never stage our own
    // version (downgrade of an equal version is a no-op refused here).
    if rel.version == current_version {
        done(Ok(UpdateOutcome::UpToDate));
        return;
    }
    let dir = stage_dir();
    match fetch_and_verify(&rel, current_exe, &dir, progress) {
        Ok(staged) => match write_sidecar(channel, &rel, &staged) {
            Ok(()) => {
                if is_downgrade {
                    done(Ok(UpdateOutcome::RolledBack {
                        version: rel.version.clone(),
                    }));
                } else {
                    done(Ok(UpdateOutcome::Staged {
                        version: rel.version.clone(),
                        security: rel.is_security(),
                    }));
                }
            }
            Err(e) => done(Err(e)),
        },
        Err(e) => done(Err(short_reason(&e))),
    }
}

/// Cap an arbitrary failure string for toasts and the About page (paths
/// and URLs must never reach user-visible copy unbounded).
fn short_reason(e: &str) -> String {
    const CAP: usize = 160;
    let flat: String = e.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= CAP {
        flat
    } else {
        format!("{}...", &flat[..CAP])
    }
}

fn write_sidecar(
    channel: &Channel,
    rel: &ChannelRelease,
    fetched: &std::path::Path,
) -> Result<(), String> {
    // The sidecar blesses the resolved platform artifact, not the whole
    // manifest: boot-time re-verification covers exactly these bytes.
    let artifact = rel.artifact_for_this_platform().ok_or_else(|| {
        "no artifact for this platform in release manifest - refusing update".to_owned()
    })?;
    let car = StagedSidecar {
        version: rel.version.clone(),
        channel: channel.as_str().to_owned(),
        security: rel.is_security(),
        sha256: artifact.sha256.clone(),
        signature: rel.signature.clone(),
        url: artifact.url.clone(),
    };
    let json = serde_json::to_string_pretty(&car).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(stage_dir()).map_err(|e| e.to_string())?;
    // The staged binary lives at a fixed name per version: the sidecar
    // describes, never locates, so a foreign sidecar cannot bless an
    // arbitrary path.
    let dest = stage_dir().join(staged_file_name(&rel.version));
    std::fs::rename(fetched, &dest).map_err(|e| e.to_string())?;
    let path = sidecar_path();
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)) {
        // No orphan staged binary without its describing sidecar.
        let _ = std::fs::remove_file(&dest);
        return Err(e.to_string());
    }
    Ok(())
}

/// Boot-time root for staged-apply verification. Same precedence as
/// [`provision_root_from_env`]: explicit env first, then the baked
/// release root. Missing on both sides means fail closed.
fn staged_root_hex() -> Option<String> {
    match resolve_root_source(
        std::env::var(RELEASE_ROOT_ENV).ok().as_deref(),
        option_env!("FERRITE_RELEASE_ROOT_BAKED"),
    ) {
        RootSource::Env(key) | RootSource::Baked(key) => Some(key),
        RootSource::Missing => None,
    }
}

/// Boot-time staged apply: re-verify the sidecar signature against the
/// provisioned root, hash the staged file, swap, health-check, roll back
/// on failure. Returns the applied version when one landed.
pub fn apply_staged_at_boot(current_exe: &std::path::Path) -> Option<String> {
    let data = std::fs::read_to_string(sidecar_path()).ok()?;
    let car: StagedSidecar = serde_json::from_str(&data).ok()?;
    // Basic sanity before touching crypto-shaped values.
    if car.version.is_empty() || car.version.len() > 32 {
        return None;
    }
    let channel = Channel::from_str(&car.channel);
    // Rebuild a single-target release around the blessed bytes: the
    // signature gate below covers exactly what was fetched, on this
    // platform. Unknown platforms fail closed (nothing was fetchable there).
    let Some(triple) = ferrite_sync::updater::current_target_triple() else {
        tracing::warn!("staged update on unknown platform - discarding");
        discard_staged();
        return None;
    };
    let rel = ChannelRelease {
        version: car.version.clone(),
        rollout: 100,
        signature: car.signature.clone(),
        targets: [(
            triple.to_owned(),
            ferrite_sync::updater::TargetArtifact {
                url: car.url.clone(),
                sha256: car.sha256.clone(),
                patch_url: None,
            },
        )]
        .into_iter()
        .collect(),
        severity: String::new(),
    };
    // Fail closed without a root, exactly like the live check path.
    // Same precedence as provisioning (explicit env, then the baked
    // release root): a build that stages via the bake must also apply
    // via the bake, or staged updates would never land.
    let root_hex = staged_root_hex()?;
    let root = hex_bytes(&root_hex)?;
    if !verify_release_signature(&channel, &rel, &root) {
        tracing::warn!("staged update signature invalid - discarding");
        discard_staged();
        return None;
    }
    // The staged binary is the only file the sidecar may bless: same
    // directory, fixed name per kind, never a sidecar-supplied path.
    let staged = stage_dir().join(staged_file_name(&car.version));
    if !verify_sha256(&staged, &car.sha256) {
        tracing::warn!("staged artifact hash mismatch - discarding");
        discard_staged();
        return None;
    }
    if atomic_swap(current_exe, &staged).is_err() {
        return None;
    }
    if rollback_if_failed(current_exe, &car.version) {
        tracing::warn!(version = %car.version, "staged update failed health - rolled back");
        discard_staged();
        return None;
    }
    discard_staged();
    tracing::info!(version = %car.version, "staged update applied");
    Some(car.version)
}

/// Fixed staged-binary name per version (no path flows from the sidecar).
fn staged_file_name(version: &str) -> String {
    let safe: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
        .take(32)
        .collect();
    format!("ferrite-staged-{safe}.bin")
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn discard_staged() {
    let _ = std::fs::remove_file(sidecar_path());
    if let Ok(entries) = std::fs::read_dir(stage_dir()) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Reminder copy for a staged update. Security updates escalate ("until
/// applied"). Routine updates state the fact once.
pub fn staged_toast(version: &str, security: bool) -> String {
    if security {
        format!("Security update {version} downloaded. Restart Ferrite to apply it.")
    } else {
        format!("Update {version} downloaded. Restart Ferrite to apply it.")
    }
}

/// Previous-release binary retained by [`ferrite_sync::updater::atomic_swap`]
/// (same `<exe>.old` naming, so both sides agree without sharing code).
pub fn previous_binary(current_exe: &std::path::Path) -> PathBuf {
    current_exe.with_extension("old")
}

/// Whether a previous release is retained for rollback.
pub fn has_previous_version(current_exe: &std::path::Path) -> bool {
    previous_binary(current_exe).exists()
}

/// Relaunch marker: written by the restore-page rollback action, consumed
/// by the graceful close path and then by `run()`. Content is the
/// rolled-back-to version (capped). Absent or unreadable means no relaunch.
pub fn relaunch_marker_path(dir: &std::path::Path) -> PathBuf {
    dir.join("ferrite-relaunch")
}

/// Record a requested relaunch into `dir` (the working directory in
/// production, a temp dir in tests).
pub fn write_relaunch_marker(dir: &std::path::Path, version: &str) {
    let version: String = version.chars().take(32).collect();
    let _ = std::fs::write(relaunch_marker_path(dir), version);
}

/// Take a pending relaunch request, if any (consumes the marker so one
/// request relaunches exactly once).
pub fn take_relaunch_marker(dir: &std::path::Path) -> Option<String> {
    let path = relaunch_marker_path(dir);
    let version = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let version = version.trim().to_owned();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Roll back to the retained previous release: swap it over the current
/// binary, health-check the restored image, put the current build back on
/// any failure. Returns the restored version. 20s health-check ceiling.
pub fn rollback_to_previous(current_exe: &std::path::Path) -> Result<String, String> {
    rollback_to_previous_with(current_exe, health_version)
}

/// Core with injectable health check (unit tests stub the runner, the
/// file moves are real and run in temp dirs).
fn rollback_to_previous_with(
    current_exe: &std::path::Path,
    health: impl Fn(&std::path::Path) -> Option<String>,
) -> Result<String, String> {
    let prev = previous_binary(current_exe);
    if !prev.exists() {
        return Err("no previous version retained".to_owned());
    }
    let bad = current_exe.with_extension("bad");
    let _ = std::fs::remove_file(&bad);
    std::fs::rename(current_exe, &bad).map_err(|e| e.to_string())?;
    if std::fs::rename(&prev, current_exe).is_err() {
        let _ = std::fs::rename(&bad, current_exe);
        return Err("rollback swap failed".to_owned());
    }
    match health(current_exe) {
        Some(version) => {
            let _ = std::fs::remove_file(&bad);
            Ok(version)
        }
        None => {
            // Restored image unhealthy: put the caller's build back so a
            // failed rollback never bricks the install. The single token
            // is consumed either way (documented single generation).
            let _ = std::fs::rename(&bad, current_exe);
            Err("previous version failed its health check".to_owned())
        }
    }
}

/// Health probe shared with the updater own gate: `--version` exits 0
/// and its first stdout line names the version, capped. The binary is
/// ours, but the output is still bounded before it reaches UI.
fn health_version(exe: &std::path::Path) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let exe = exe.to_owned();
    let _ = std::thread::Builder::new()
        .name("ferrite-rollback-health".to_owned())
        .spawn(move || {
            let _ = tx.send(
                std::process::Command::new(&exe)
                    .arg("--version")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .next()
                            .map(|l| l.trim().chars().take(32).collect::<String>())
                    })
                    .filter(|v| !v.is_empty()),
            );
        });
    rx.recv_timeout(Duration::from_secs(20)).ok().flatten()
}

/// About-page state snapshot for `__ferriteUpdate`. Shape is fixed and
/// additive: `{phase, version?, security?, downloaded?, total?, message?}`.
/// Versions and messages are already bounded at their sources.
pub fn phase_json(phase: &UpdatePhase) -> serde_json::Value {
    match phase {
        UpdatePhase::Idle => serde_json::json!({"phase": "idle"}),
        UpdatePhase::Checking => serde_json::json!({"phase": "checking"}),
        UpdatePhase::Downloading { downloaded, total } => {
            serde_json::json!({"phase": "downloading", "downloaded": downloaded, "total": total})
        }
        UpdatePhase::Ready {
            version,
            security,
            reminded_at: _,
        } => serde_json::json!({"phase": "ready", "version": version, "security": security}),
        UpdatePhase::Failed(message) => serde_json::json!({"phase": "failed", "message": message}),
        UpdatePhase::UpToDate => serde_json::json!({"phase": "uptodate"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_override_falls_back_to_stable() {
        assert_eq!(update_channel(), Channel::Stable);
    }

    #[test]
    fn update_marker_round_trips_and_self_cleans() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-update-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Round trip, then gone (one-shot).
        note_updated_in(&dir, "0.1.0", "0.2.0");
        assert_eq!(
            take_updated_in(&dir),
            Some(("0.1.0".to_owned(), "0.2.0".to_owned()))
        );
        assert_eq!(take_updated_in(&dir), None);
        // Same-version and garbage never arm.
        note_updated_in(&dir, "0.2.0", "0.2.0");
        assert_eq!(take_updated_in(&dir), None);
        note_updated_in(&dir, "<script>", "0.2.0");
        assert_eq!(take_updated_in(&dir), None);
        std::fs::write(dir.join(UPDATED_MARKER_NAME), b"{nope}").unwrap();
        assert_eq!(take_updated_in(&dir), None);
        assert!(!dir.join(UPDATED_MARKER_NAME).exists(), "corrupt deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_check_delay_beats_hourly_throttle() {
        assert!(FIRST_UPDATE_CHECK_DELAY < UPDATE_CHECK_INTERVAL);
        assert_eq!(FIRST_UPDATE_CHECK_DELAY, Duration::from_secs(45));
    }

    #[test]
    fn short_reason_caps_and_flattens() {
        assert_eq!(short_reason("  a  b\n c "), "a b c");
        let long = "x".repeat(500);
        let short = short_reason(&long);
        assert!(short.len() <= 163, "capped with ellipsis");
        assert!(short.ends_with("..."));
    }

    #[test]
    fn staged_file_name_never_carries_a_path() {
        assert_eq!(staged_file_name("1.2.3"), "ferrite-staged-1.2.3.bin");
        assert_eq!(
            staged_file_name("../../evil"),
            "ferrite-staged-....evil.bin"
        );
        assert!(!staged_file_name("1.0").contains('/'));
        assert!(!staged_file_name("1.0").contains('\\'));
    }

    #[test]
    fn hex_bytes_validates_release_root_shape() {
        assert!(hex_bytes(&"ab".repeat(32)).is_some());
        assert!(hex_bytes("short").is_none());
        assert!(hex_bytes(&"zz".repeat(32)).is_none());
    }

    #[test]
    fn root_source_env_beats_bake_empty_is_missing() {
        assert_eq!(
            resolve_root_source(Some("ab"), Some("cd")),
            RootSource::Env("ab".into())
        );
        assert_eq!(
            resolve_root_source(Some("  "), Some("cd")),
            RootSource::Baked("cd".into())
        );
        assert_eq!(
            resolve_root_source(None, Some("cd")),
            RootSource::Baked("cd".into())
        );
        assert_eq!(resolve_root_source(None, Some("  ")), RootSource::Missing);
        assert_eq!(resolve_root_source(None, None), RootSource::Missing);
    }

    #[test]
    fn staged_root_hex_honors_env_then_bake() {
        let key = "ab".repeat(32);
        unsafe { std::env::set_var(RELEASE_ROOT_ENV, &key) };
        assert_eq!(staged_root_hex().as_deref(), Some(key.as_str()));
        unsafe { std::env::remove_var(RELEASE_ROOT_ENV) };
        // Test builds bake no root, so an unset env means fail closed.
        // A release build with FERRITE_RELEASE_ROOT_BAKED set resolves
        // Some here via the same precedence (covered by
        // root_source_env_beats_bake_empty_is_missing).
        assert_eq!(staged_root_hex(), None);
    }

    #[test]
    fn staged_toast_escalates_security_only() {
        let sec = staged_toast("1.2.3", true);
        assert!(sec.contains("Security update"), "escalation copy");
        let routine = staged_toast("1.2.3", false);
        assert!(!routine.contains("Security"), "routine stays calm");
        assert!(routine.contains("Restart"), "both name the action");
    }

    #[test]
    fn update_phase_defaults_to_idle() {
        assert!(matches!(UpdatePhase::default(), UpdatePhase::Idle));
    }

    #[test]
    fn relaunch_marker_round_trips_exactly_once() {
        let dir = std::env::temp_dir().join("ferrite-relaunch-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(take_relaunch_marker(&dir).is_none(), "absent means none");
        write_relaunch_marker(&dir, "1.2.3");
        assert_eq!(take_relaunch_marker(&dir).as_deref(), Some("1.2.3"));
        assert!(
            take_relaunch_marker(&dir).is_none(),
            "consumed exactly once"
        );
        write_relaunch_marker(&dir, "");
        assert!(take_relaunch_marker(&dir).is_none(), "empty means none");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn previous_binary_naming_matches_swap_convention() {
        assert_eq!(
            previous_binary(std::path::Path::new("/tmp/ferrite")),
            std::path::PathBuf::from("/tmp/ferrite.old")
        );
        assert_eq!(
            previous_binary(std::path::Path::new("C:\\bin\\ferrite.exe")),
            std::path::PathBuf::from("C:\\bin\\ferrite.old")
        );
        assert!(!has_previous_version(std::path::Path::new(
            "/tmp/definitely-not-ferrite-xyz"
        )));
    }

    #[test]
    fn rollback_refuses_without_retained_previous() {
        let dir = std::env::temp_dir().join("ferrite-rollback-noprev");
        let _ = std::fs::create_dir_all(&dir);
        let exe = dir.join("ferrite");
        let _ = std::fs::write(&exe, b"current");
        assert!(rollback_to_previous_with(&exe, |_| Some("9.9.9".into())).is_err());
        // Untouched: the refusal happens before any rename.
        assert_eq!(std::fs::read(&exe).unwrap(), b"current");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_restores_previous_and_reports_version() {
        let dir = std::env::temp_dir().join("ferrite-rollback-ok");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("ferrite");
        let prev = dir.join("ferrite.old");
        std::fs::write(&exe, b"bad-new").unwrap();
        std::fs::write(&prev, b"good-old").unwrap();
        let got = rollback_to_previous_with(&exe, |_| Some("1.1.0".into())).unwrap();
        assert_eq!(got, "1.1.0");
        assert_eq!(std::fs::read(&exe).unwrap(), b"good-old");
        assert!(!prev.exists(), "token consumed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_puts_current_back_when_health_fails() {
        let dir = std::env::temp_dir().join("ferrite-rollback-badprev");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("ferrite");
        let prev = dir.join("ferrite.old");
        std::fs::write(&exe, b"current").unwrap();
        std::fs::write(&prev, b"also-bad").unwrap();
        assert!(rollback_to_previous_with(&exe, |_| None).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"current", "state preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn phase_json_shape_is_fixed_and_bounded() {
        assert_eq!(
            phase_json(&UpdatePhase::Idle),
            serde_json::json!({"phase": "idle"})
        );
        assert_eq!(
            phase_json(&UpdatePhase::Downloading {
                downloaded: 1,
                total: Some(2)
            }),
            serde_json::json!({"phase": "downloading", "downloaded": 1, "total": 2})
        );
        let ready = phase_json(&UpdatePhase::Ready {
            version: "1.2.3".into(),
            security: true,
            reminded_at: Instant::now(),
        });
        assert_eq!(ready["phase"], "ready");
        assert_eq!(ready["version"], "1.2.3");
        assert_eq!(ready["security"], true);
        let failed = phase_json(&UpdatePhase::Failed("boom".into()));
        assert_eq!(failed["message"], "boom");
    }
}
