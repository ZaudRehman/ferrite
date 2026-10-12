//! Multi profile paths. Engine isolation is native (one WebContext per
//! profile). Persistent files live under the OS data dir
//! ([`data_root`]); pre-data-root installs migrate once from the launch
//! directory with byte-verified copies (see [`migrate_cwd_to_data_root`]).
//! `FERRITE_DATA_DIR` overrides everything (portable installs, tests).

use std::path::{Path, PathBuf};

pub const DEFAULT_ID: &str = "Default";
pub const BASE_DIR: &str = "ferrite-profiles";
pub const LEGACY_ENGINE_DIR: &str = "ferrite-profile";

/// Process profile id, fixed once at boot (profiles never change inside
/// a process: switching spawns a new one). Free-function path owners
/// (settings, session, crash marker) scope through this so their dozens
/// of call sites need no threading. Same one-way discipline as EPHEMERAL.
static PROCESS_PROFILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Fix the process profile (boot, once). Sanitized ids only; anything
/// else falls back to default, never a refusal to boot.
pub fn set_process_profile(id: &str) {
    let clean = sanitize(id).unwrap_or_else(|| DEFAULT_ID.to_owned());
    let _ = PROCESS_PROFILE.set(clean);
}

/// Profile id owning this process's stores. Default when unset (tests,
/// legacy callers).
pub fn process_profile() -> &'static str {
    PROCESS_PROFILE
        .get()
        .map(|s| s.as_str())
        .unwrap_or(DEFAULT_ID)
}

/// Single-instance lock path: incognito is one global window, default
/// keeps the legacy name, named profiles scope their own lock so two
/// profiles run side by side.
pub fn instance_lock_path(incognito: bool, profile_id: &str) -> std::path::PathBuf {
    let root = data_root();
    if incognito {
        root.join("ferrite-incognito.lock")
    } else if profile_id.eq_ignore_ascii_case(DEFAULT_ID) {
        root.join("ferrite.lock")
    } else {
        root.join(format!("ferrite-{}.lock", slug(profile_id)))
    }
}

/// Env override for the data root (portable mode, tests). Empty counts as unset.
pub const DATA_DIR_ENV: &str = "FERRITE_DATA_DIR";

/// OS data root for all persistent Ferrite files. Resolution order:
/// `FERRITE_DATA_DIR` override → OS convention → launch directory fallback
/// (dev only: home undetectable; logged by the caller when it matters).
/// Stdlib only: no `dirs` dependency for three env reads.
pub fn data_root() -> PathBuf {
    if let Ok(dir) = std::env::var(DATA_DIR_ENV)
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Some(root) = os_default_root() {
        return root;
    }
    PathBuf::from(".")
}

fn os_default_root() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.trim().is_empty()
        {
            return Some(PathBuf::from(appdata).join("Ferrite"));
        }
        if let Ok(home) = std::env::var("USERPROFILE")
            && !home.trim().is_empty()
        {
            return Some(
                PathBuf::from(home)
                    .join("AppData")
                    .join("Roaming")
                    .join("Ferrite"),
            );
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME")
            && !home.trim().is_empty()
        {
            return Some(
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("Ferrite"),
            );
        }
        return None;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.trim().is_empty()
        {
            return Some(PathBuf::from(xdg).join("ferrite"));
        }
        if let Ok(home) = std::env::var("HOME")
            && !home.trim().is_empty()
        {
            return Some(
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("ferrite"),
            );
        }
        None
    }
}

/// Threat-intel cache location (public hashes only, see the storage contract).
pub fn threat_db_path() -> PathBuf {
    data_root().join("ferrite-threat.db")
}

/// Crash log location (scrubbed, no user data by construction).
pub fn crash_log_path() -> PathBuf {
    data_root().join("ferrite-crash.log")
}

/// Known pre-data-root files in the launch directory, migrated once each.
/// Returns the names that moved (for one boot log line).
pub fn migrate_cwd_to_data_root() -> Vec<String> {
    let root = data_root();
    if root == Path::new(".") {
        return Vec::new();
    }
    // `current_dir` failure means there is no CWD to migrate from.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    migrate_dir_to_root(&cwd, &root)
}

fn migrate_dir_to_root(cwd: &Path, root: &Path) -> Vec<String> {
    const FILES: &[&str] = &[
        "ferrite.db",
        "ferrite-archive.db",
        "ferrite-settings.enc",
        "ferrite-settings.json",
        "ferrite-session.enc",
        "ferrite-session.json",
        "ferrite-threat.db",
        "ferrite-crash.log",
        "ferrite-recovery.wrap",
    ];
    let mut moved = Vec::new();
    // Plain files: byte-verified copy, then the original is renamed aside
    // (never deleted: user bytes are never destroyed by a migration).
    for name in FILES {
        let src = cwd.join(name);
        if !src.is_file() {
            continue;
        }
        let dest = root.join(name);
        if dest.exists() {
            rename_aside(&src);
            moved.push((*name).to_owned());
            continue;
        }
        if let Some(parent) = dest.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(&src) else {
            continue;
        };
        if std::fs::write(&dest, &bytes).is_err() {
            let _ = std::fs::remove_file(&dest);
            continue;
        }
        if std::fs::read(&dest).ok().as_deref() != Some(bytes.as_slice()) {
            let _ = std::fs::remove_file(&dest);
            continue;
        }
        rename_aside(&src);
        moved.push((*name).to_owned());
    }
    // Directories: same-volume rename first (the common case: data root sits
    // on the user volume), else recursive copy + verify + rename aside.
    // A failed move leaves the source untouched and skips it (no split brain:
    // the old CWD path keeps working until a later boot moves it).
    for name in [BASE_DIR, LEGACY_ENGINE_DIR] {
        let src = cwd.join(name);
        if !src.is_dir() {
            continue;
        }
        let dest = root.join(name);
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            continue;
        }
        if std::fs::rename(&src, &dest).is_ok() {
            moved.push((*name).to_owned());
        } else if copy_dir_recursive(&src, &dest) {
            rename_aside(&src);
            moved.push((*name).to_owned());
        }
    }
    moved
}

fn rename_aside(path: &Path) {
    let stamped = path.with_extension(format!(
        "migrated-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let _ = std::fs::rename(path, stamped);
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(src) else {
        return false;
    };
    if std::fs::create_dir_all(dest).is_err() {
        return false;
    }
    for entry in entries.filter_map(|e| e.ok()) {
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let ok = if from.is_dir() {
            copy_dir_recursive(&from, &to)
        } else {
            match std::fs::read(&from) {
                Ok(bytes) => {
                    std::fs::write(&to, &bytes).is_ok()
                        && std::fs::read(&to).ok().as_deref() == Some(bytes.as_slice())
                }
                Err(_) => false,
            }
        };
        if !ok {
            let _ = std::fs::remove_dir_all(dest);
            return false;
        }
    }
    true
}

/// Profile handle. Guests are wiped via [`destroy`], never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub ephemeral: bool,
}

impl Profile {
    pub fn default_profile() -> Self {
        Self {
            id: DEFAULT_ID.to_owned(),
            ephemeral: false,
        }
    }

    pub fn is_default(&self) -> bool {
        self.id.eq_ignore_ascii_case(DEFAULT_ID)
    }
}

/// Session: the single owner of per-mode state. Every tab, request,
/// store, and download resolves through this: no scattered `is_private`
/// booleans at call sites. `Persistent` survives restarts (SQLCipher on
/// disk); `Ephemeral` lives only in RAM + a temp dir that is wiped on exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// User mode: persistent identity backed by `profiles/<id>/` (or legacy
    /// CWD files for Default). Private tabs inside it stay ephemeral (see
    /// `TabState::is_private`) but the process itself persists.
    Persistent { profile: Profile },
    /// Incognito mode: one per process launch (`--incognito` or decision
    /// screen). Owns a temp guest profile; nothing encrypted is read or
    /// written, vault is `:memory:` only.
    Ephemeral { guest: Profile, session_id: String },
}

impl Session {
    /// Process session for boot: incognito flag selects the variant.
    /// Ephemeral ids are random per process so two incognito processes never
    /// share a jar even if PIDs recycle (PID alone is the old collision).
    pub fn for_boot(incognito: bool) -> Self {
        Self::for_boot_profile(incognito, None)
    }

    /// Boot session with a named profile (`--profile`). Sanitized and
    /// created on demand (Chrome parity: first use mints the dir);
    /// anything unusable falls back to default, never a refusal to boot.
    pub fn for_boot_profile(incognito: bool, profile: Option<&str>) -> Self {
        if incognito {
            return Self::ephemeral();
        }
        match profile.and_then(sanitize).and_then(|id| create(&id)) {
            Some(p) => Self::Persistent { profile: p },
            None => Self::Persistent {
                profile: Profile::default_profile(),
            },
        }
    }

    pub fn ephemeral() -> Self {
        let guest = guest();
        let session_id = format!("{}-{}", guest.id, uuid::Uuid::new_v4());
        Self::Ephemeral { guest, session_id }
    }

    pub fn is_ephemeral(&self) -> bool {
        matches!(self, Self::Ephemeral { .. })
    }

    /// Stable per-process id: profile id for persistent, random session id
    /// for ephemeral. Used as the proxy partition key and log label.
    pub fn id(&self) -> &str {
        match self {
            Self::Persistent { profile } => &profile.id,
            Self::Ephemeral { session_id, .. } => session_id,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        if self.is_ephemeral() {
            "incognito"
        } else {
            "user"
        }
    }

    fn profile_ref(&self) -> &Profile {
        match self {
            Self::Persistent { profile } => profile,
            Self::Ephemeral { guest, .. } => guest,
        }
    }

    pub fn vault_path(&self) -> PathBuf {
        super::profiles::vault_path(self.profile_ref())
    }

    pub fn archive_path(&self) -> PathBuf {
        super::profiles::archive_path(self.profile_ref())
    }

    pub fn settings_path(&self) -> PathBuf {
        super::profiles::settings_path(self.profile_ref())
    }

    pub fn engine_dir(&self) -> PathBuf {
        super::profiles::engine_dir(self.profile_ref())
    }

    pub fn downloads_dir(&self) -> PathBuf {
        super::profiles::guest_downloads_dir(self.profile_ref())
    }

    /// Immediate, synchronous teardown for the last-window close path:
    /// removes the temp guest tree (downloads included). Best-effort,
    /// never fatal; crash leftovers are swept at next boot.
    pub fn teardown(&self) {
        if let Self::Ephemeral { guest, .. } = self {
            destroy(guest);
        }
    }
}

/// Private-tab spill dir inside a persistent process: engine downloads from
/// `is_private` tabs land here (never `~/Downloads`), history rows are never
/// written, and the tree is wiped at clean exit + swept at boot.
pub fn private_downloads_dir() -> PathBuf {
    std::env::temp_dir()
        .join("ferrite")
        .join(format!("private-{}", std::process::id()))
}

/// Remove stale `private-<pid>` dirs whose owner is gone. Same liveness rule
/// as guests: live PIDs never touched, unparseable names left alone.
pub fn sweep_stale_private() {
    let base = std::env::temp_dir().join("ferrite");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix("private-")
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if sys.process(sysinfo::Pid::from_u32(pid)).is_none() {
            tracing::debug!(name, "removing stale private dir");
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Filesystem safe names only. Rejects reserved names and path tricks.
pub fn sanitize(name: &str) -> Option<String> {
    let t = name.trim();
    if t.is_empty() || t.len() > 32 {
        return None;
    }
    if t.eq_ignore_ascii_case(DEFAULT_ID) || t.eq_ignore_ascii_case("guest") {
        return None;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-')
    {
        return None;
    }
    if t.contains("..") || t.contains(['/', '\\', ':', '.']) {
        return None;
    }
    Some(t.to_owned())
}

/// Ensure the profile dir exists. Returns None on invalid names.
pub fn create(name: &str) -> Option<Profile> {
    let id = sanitize(name)?;
    let dir = profile_dir(&id);
    std::fs::create_dir_all(&dir).ok()?;
    Some(Profile {
        id,
        ephemeral: false,
    })
}

/// Fresh guest in the temp dir. Isolated by construction.
pub fn guest() -> Profile {
    let id = format!("guest-{}", std::process::id());
    // ponytail: temp guest, one dir per process; concurrent guests in one
    // process share it. Per-window guests want a counter/nonce here.
    let _ = std::fs::create_dir_all(guest_dir(&id));
    Profile {
        id,
        ephemeral: true,
    }
}

/// Delete a profile dir. Default is never removed here.
pub fn destroy(profile: &Profile) {
    if profile.is_default() {
        return;
    }
    let dir = if profile.ephemeral {
        guest_dir(&profile.id)
    } else {
        profile_dir(&profile.id)
    };
    let _ = std::fs::remove_dir_all(dir);
}

pub fn profile_dir(id: &str) -> PathBuf {
    data_root().join(BASE_DIR).join(id)
}

/// Named profile ids on disk, sorted. Default is implied (legacy files),
/// never listed: callers prepend it. Files, hidden entries, and names
/// that no longer sanitize are skipped, never fatal.
pub fn list() -> Vec<String> {
    let base = data_root().join(BASE_DIR);
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| sanitize(n).is_some_and(|s| s == *n))
        .collect();
    out.sort();
    out
}

/// Filename slug for per-profile instance files (locks, forward
/// endpoints): spaces become underscores (shell-safe, log-clean).
/// Default yields ""; callers keep legacy bare names for it.
pub fn slug(id: &str) -> String {
    id.replace(' ', "_")
}

fn guest_dir(id: &str) -> PathBuf {
    std::env::temp_dir().join("ferrite").join(id)
}

/// Downloads dir inside a guest profile (created on demand by the
/// caller). Guest downloads never touch the user's Downloads folder.
pub fn guest_downloads_dir(profile: &Profile) -> PathBuf {
    if profile.ephemeral {
        std::env::temp_dir()
            .join("ferrite")
            .join(&profile.id)
            .join("downloads")
    } else {
        profile_dir(&profile.id).join("downloads")
    }
}

/// Remove guest dirs whose owner process is gone (crashed guest sessions
/// must not leave downloaded files behind). Live PIDs are never touched;
/// unparseable names are left alone. Best-effort, logged, never fatal.
pub fn sweep_stale_guests() {
    let base = std::env::temp_dir().join("ferrite");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix("guest-")
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if sys.process(sysinfo::Pid::from_u32(pid)).is_none() {
            tracing::debug!(name, "removing stale guest dir");
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn file_for(profile: &Profile, default: &str, name: &str) -> PathBuf {
    if profile.is_default() {
        return data_root().join(default);
    }
    let base = if profile.ephemeral {
        guest_dir(&profile.id)
    } else {
        profile_dir(&profile.id)
    };
    base.join(name)
}

/// Default profile files live directly under the data root.
pub fn vault_path(p: &Profile) -> PathBuf {
    file_for(p, "ferrite.db", "vault.db")
}
pub fn archive_path(p: &Profile) -> PathBuf {
    file_for(p, "ferrite-archive.db", "archive.db")
}
pub fn settings_path(p: &Profile) -> PathBuf {
    file_for(p, "ferrite-settings.json", "settings.json")
}
pub fn engine_dir(p: &Profile) -> PathBuf {
    if p.is_default() {
        return data_root().join(LEGACY_ENGINE_DIR);
    }
    file_for(p, LEGACY_ENGINE_DIR, "engine")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env-backed path tests run serially: `FERRITE_DATA_DIR` is
    /// process-global, so concurrent readers would race the override.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvOverride {
        _guard: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl EnvOverride {
        fn set(dir: &Path) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var(DATA_DIR_ENV).ok();
            // `set_var` is `unsafe` on this toolchain (edition 2024
            // `std::env` rule): single-threaded under the serial lock.
            unsafe { std::env::set_var(DATA_DIR_ENV, dir) };
            Self {
                _guard: guard,
                previous,
            }
        }
    }

    impl Drop for EnvOverride {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => unsafe { std::env::set_var(DATA_DIR_ENV, v) },
                None => unsafe { std::env::remove_var(DATA_DIR_ENV) },
            }
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-profiles-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths_split_per_profile() {
        let root = scratch("split");
        let _env = EnvOverride::set(&root);
        let d = Profile::default_profile();
        assert_eq!(vault_path(&d), root.join("ferrite.db"));
        assert_eq!(engine_dir(&d), root.join("ferrite-profile"));
        let p = Profile {
            id: "Work".to_owned(),
            ephemeral: false,
        };
        assert_eq!(vault_path(&p), root.join("ferrite-profiles/Work/vault.db"));
        assert_eq!(
            archive_path(&p),
            root.join("ferrite-profiles/Work/archive.db")
        );
        assert_eq!(
            settings_path(&p),
            root.join("ferrite-profiles/Work/settings.json")
        );
        assert!(engine_dir(&p).starts_with(root.join("ferrite-profiles/Work")));
        let g = Profile {
            id: "guest-1".to_owned(),
            ephemeral: true,
        };
        assert!(!vault_path(&g).starts_with(root.join("ferrite-profiles")));
        assert!(!vault_path(&g).starts_with(root.join("ferrite.db")));
        assert!(sanitize("Work 2").is_some());
        for bad in [
            "",
            "a/b",
            "..",
            "x:y",
            "Default",
            "GUEST",
            ".",
            "x".repeat(33).as_str(),
        ] {
            assert!(sanitize(bad).is_none(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_create_destroy_round_trip() {
        let root = scratch("list");
        let _env = EnvOverride::set(&root);
        // Empty store lists nothing (Default is implied, never listed).
        assert!(list().is_empty());
        assert!(create("Work").is_some());
        assert!(create("Personal").is_some());
        // Invalid names create nothing.
        assert!(create("").is_none());
        assert!(create("a/b").is_none());
        assert!(create("Default").is_none());
        assert_eq!(list(), vec!["Personal".to_owned(), "Work".to_owned()]);
        // Stray files and hidden dirs never list.
        std::fs::write(root.join(BASE_DIR).join("notes.txt"), b"x").unwrap();
        std::fs::create_dir_all(root.join(BASE_DIR).join(".hidden")).unwrap();
        assert_eq!(list(), vec!["Personal".to_owned(), "Work".to_owned()]);
        // Destroy removes; default refuses.
        destroy(&Profile {
            id: "Work".to_owned(),
            ephemeral: false,
        });
        assert_eq!(list(), vec!["Personal".to_owned()]);
        // Default refuses destroy (never removed here).
        destroy(&Profile::default_profile());
        assert_eq!(list(), vec!["Personal".to_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn boot_profile_falls_back_to_default() {
        let root = scratch("bootprof");
        let _env = EnvOverride::set(&root);
        // for_boot keeps legacy behavior (default, ephemeral on flag).
        assert_eq!(Session::for_boot(false).id(), "Default");
        assert!(Session::for_boot(true).is_ephemeral());
        // Named profiles mint on demand; garbage falls back to default.
        let named = Session::for_boot_profile(false, Some("Work"));
        assert_eq!(named.id(), "Work");
        assert!(!named.is_ephemeral());
        let fallback = Session::for_boot_profile(false, Some("a/b"));
        assert_eq!(fallback.id(), "Default");
        let empty = Session::for_boot_profile(false, None);
        assert_eq!(empty.id(), "Default");
        // Incognito ignores profiles entirely.
        assert!(Session::for_boot_profile(true, Some("Work")).is_ephemeral());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scoped_names_keep_default_legacy() {
        // Lock + slug: default bare, named suffixed, spaces underscored.
        assert!(
            instance_lock_path(false, "Default")
                .file_name()
                .unwrap()
                .eq("ferrite.lock")
        );
        assert!(
            instance_lock_path(true, "Work")
                .file_name()
                .unwrap()
                .eq("ferrite-incognito.lock")
        );
        let scoped = instance_lock_path(false, "My Work");
        assert_eq!(scoped.file_name().unwrap(), "ferrite-My_Work.lock");
        assert_eq!(slug("My Work"), "My_Work");
        assert_eq!(slug("Default"), "Default");
    }

    #[test]
    fn session_owns_paths_and_kinds() {
        let root = scratch("session");
        let _env = EnvOverride::set(&root);
        let user = Session::for_boot(false);
        assert!(!user.is_ephemeral());
        assert_eq!(user.kind_str(), "user");
        assert_eq!(user.id(), "Default");
        assert_eq!(user.vault_path(), root.join("ferrite.db"));
        assert_eq!(user.engine_dir(), root.join("ferrite-profile"));

        let a = Session::ephemeral();
        let b = Session::ephemeral();
        assert!(a.is_ephemeral() && b.is_ephemeral());
        assert_eq!(a.kind_str(), "incognito");
        // Random per process: two ephemeral sessions never share a jar.
        assert_ne!(a.id(), b.id());
        // Guest paths stay out of the persistent tree.
        assert!(!a.vault_path().starts_with(root.join("ferrite-profiles")));
        assert!(!a.vault_path().starts_with(root.join("ferrite.db")));

        let priv_dir = private_downloads_dir();
        assert!(priv_dir.ends_with(format!("private-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn data_root_override_wins_and_empty_falls_through() {
        let root = scratch("override");
        {
            let _env = EnvOverride::set(&root);
            assert_eq!(data_root(), root);
            assert_eq!(threat_db_path(), root.join("ferrite-threat.db"));
            assert_eq!(crash_log_path(), root.join("ferrite-crash.log"));
        }
        // Empty override counts as unset: must resolve without panicking and
        // must not echo the empty string back as a path.
        let _env = EnvOverride::set(Path::new(""));
        assert!(!data_root().as_os_str().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_moves_cwd_files_once_with_evidence() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let work = scratch("migrate-work");
        let root = scratch("migrate-root");
        std::fs::write(work.join("ferrite.db"), b"vault-bytes").unwrap();
        std::fs::write(work.join("ferrite-session.json"), r#"{"version":1}"#).unwrap();
        std::fs::create_dir_all(work.join("ferrite-profiles/Work")).unwrap();
        std::fs::write(work.join("ferrite-profiles/Work/vault.db"), b"w").unwrap();
        // Pre-existing destination wins, but the source is still renamed
        // aside (never two truths, never silent deletion).
        std::fs::write(root.join("ferrite.db"), b"dest-bytes").unwrap();

        let moved = migrate_dir_to_root(&work, &root);
        assert!(moved.contains(&"ferrite.db".to_owned()));
        assert!(moved.contains(&"ferrite-session.json".to_owned()));
        assert!(moved.contains(&BASE_DIR.to_owned()));
        assert_eq!(
            std::fs::read(root.join("ferrite.db")).unwrap(),
            b"dest-bytes"
        );
        assert_eq!(
            std::fs::read(root.join("ferrite-session.json")).unwrap(),
            br#"{"version":1}"#
        );
        assert_eq!(
            std::fs::read(root.join("ferrite-profiles/Work/vault.db")).unwrap(),
            b"w"
        );
        assert!(!work.join("ferrite.db").exists());
        assert!(!work.join("ferrite-session.json").exists());
        assert!(!work.join("ferrite-profiles").exists());
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&root);
    }
}
