//! Shell: winit window plus wry webviews. Chrome is a bundled
//! `ferrite://chrome` webview driven by state pushes. Content lives in
//! child webviews below the chrome bars, fed by typed IPC commands.
#![allow(
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::too_many_arguments
)]

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, NamedKey},
    window::{Fullscreen, Icon, Window, WindowAttributes, WindowId},
};
use zeroize::Zeroize;

use ferrite_chrome::{
    chrome::{
        ChromeState, EngineRow, GroupRow, ProfileRow, StatusRow, SugRow, TabRow, WsRow, push_script,
    },
    ipc::ChromeCommand,
    tokens::ThemePack,
};
use ferrite_tabs::{
    IpcBridge, LogicalRect, NavigationHistory, TabState, WebViewHandle, WryFactory,
};

use crate::{
    app_context::AppContext,
    error::{ShellError, ShellResult},
    theme::{AccentContext, ThemeConfig},
};

/// Chrome height px. Must match the chrome template.
pub const CHROME_TOP_PX: f64 = 88.0;
/// Vertical tab rail width px. Must match the chrome template's vtab rail.
pub const VTAB_W_PX: f64 = 232.0;
/// Chrome height px with the vertical rail (tabbar docks left, so only
/// the 48px omnibar sits on top). Must match the template.
pub const CHROME_TOP_VTAB_PX: f64 = 48.0;
/// Bookmarks bar height when visible. Must match the template.
pub const BAR_H_PX: f64 = 30.0;
/// Crash infobar height when visible. Must match the template.
pub const INFOBAR_H_PX: f64 = 40.0;
/// Archive gold, constant across packs.
const ARCHIVE_GOLD: &str = "#f0c75e";
/// Tick: drain IPC and push dirty chrome state.
const HEARTBEAT: Duration = Duration::from_millis(500);
/// Dead tunnel redirect window after navigation start.
const ERROR_REDIRECT_WINDOW_MS: i64 = 30_000;

/// Wakes from the wry IPC thread. Without it messages wait for the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellWake {
    Ipc,
    /// A second instance handed over URLs (see `crate::forward`): the tick
    /// drains the queue. Reuses the same wake path as IPC (any wake drains
    /// every queue), so no second timer or thread contract is needed.
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullscreenMode {
    Windowed,
    Borderless,
}
impl FullscreenMode {
    fn toggle(self) -> Self {
        match self {
            Self::Windowed => Self::Borderless,
            Self::Borderless => Self::Windowed,
        }
    }
    fn debug_label(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Borderless => "borderless fullscreen",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShellConfig {
    pub title: String,
    pub initial_size: LogicalSize<f32>,
    pub min_size: Option<LogicalSize<f32>>,
    pub resizable: bool,
    pub theme: ThemeConfig,
    pub diagnostics_on: bool,
    /// Initial tab URL. Falls back to session restore, then newtab.
    pub initial_url: Option<String>,
    /// Incognito window: ephemeral from the first paint (memory vault,
    /// temp engine profile, no stores, no gate). Set by `--incognito`.
    pub incognito: bool,
    /// Named profile id (sanitized). None = default profile. Set by
    /// `--profile`: its vault, session, lock, and forward endpoint all
    /// scope to the profile, so two profiles run side by side.
    pub profile: Option<String>,
}
impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            title: "Ferrite".to_string(),
            initial_size: LogicalSize::new(1280.0, 800.0),
            min_size: Some(LogicalSize::new(640.0, 480.0)),
            resizable: true,
            theme: ThemeConfig::default(),
            // Off: status shows URL and cursor, which leaks in screenshots.
            diagnostics_on: false,
            initial_url: None,
            incognito: false,
            profile: None,
        }
    }
}
impl ShellConfig {
    /// Raw RGBA pixels of the brand favicon (pure: no platform calls,
    /// unit-tested against the real file).
    fn window_icon_pixels() -> Option<(Vec<u8>, u32, u32)> {
        use png::{BitDepth, ColorType};
        let bytes = include_bytes!("../../../brand/favicon.png");
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes.as_slice()));
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        let rgba = match (info.color_type, info.bit_depth) {
            (ColorType::Rgba, BitDepth::Eight) => buf,
            (ColorType::Rgb, BitDepth::Eight) => {
                let mut out = Vec::with_capacity(buf.len() / 3 * 4);
                for px in buf.chunks_exact(3) {
                    out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
                }
                out
            }
            (ct, bd) => {
                debug!(?ct, ?bd, "window icon skipped: unsupported png form");
                return None;
            }
        };
        Some((rgba, info.width, info.height))
    }

    /// Window icon from the brand kit (`brand/favicon.png`, baked into
    /// the binary). Decoded once per window via the already-vendored
    /// `png` crate (no new supply-chain edge). None on any failure:
    /// a missing titlebar glyph never blocks boot.
    fn window_icon() -> Option<Icon> {
        let (rgba, w, h) = Self::window_icon_pixels()?;
        match Icon::from_rgba(rgba, w, h) {
            Ok(icon) => Some(icon),
            Err(e) => {
                debug!(error = %e, "window icon skipped: bad dimensions");
                None
            }
        }
    }

    fn window_attributes(&self) -> WindowAttributes {
        let mut attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_window_icon(Self::window_icon())
            .with_inner_size(self.initial_size)
            .with_active(true)
            .with_resizable(self.resizable)
            .with_visible(true);
        if let Some(min) = self.min_size {
            attrs = attrs.with_min_inner_size(min);
        }
        // macOS: transparent titlebar + full-size content so the chrome
        // sits under the traffic lights per HIG. No vibrancy view is
        // inserted (the webview covers the content); the winframe tint
        // stays Windows-only. Best-effort, failures fall back to default.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true);
        }
        attrs
    }
}

/// Boot gate: encrypted stores failed to open. Only the decision tab may
/// exist until Retry/Guest/Quit resolves it. `reasons` are the
/// plain-language notices (no paths, no key material).
#[derive(Debug, Default)]
struct BootBlock {
    reasons: Vec<String>,
    tab: Option<Uuid>,
}

/// Process-wide ephemeral flag, set exactly once when Guest is chosen on
/// the decision screen. File writers (settings, session) refuse while set.
/// A global rather than a parameter because the writers are free functions
/// with dozens of call sites; the flag flips one way, once, never back.
static EPHEMERAL: AtomicBool = AtomicBool::new(false);

fn set_ephemeral() {
    EPHEMERAL.store(true, Ordering::Release);
}

pub(crate) fn is_ephemeral() -> bool {
    EPHEMERAL.load(Ordering::Acquire)
}

pub(crate) struct ShellApp {
    config: ShellConfig,
    /// Process session: the single owner of user-vs-incognito state.
    /// `guest_mode`/`guest_profile`/`EPHEMERAL` stay as the enforcement
    /// mechanism (free-function writers check them); this is the identity
    /// they derive from: proxy partition key, path owner, chrome signal.
    session: crate::profiles::Session,
    proxy: Option<ferrite_proxy::ProxyHandle>,
    /// Session-partitioned proxy for private tabs (own ConnPool, no archive
    /// fallback so private payloads never become snapshots). `None` in
    /// ephemeral processes (single pool is already ephemeral) or on spawn
    /// failure (private tabs fall back to the shared proxy, never direct).
    private_proxy: Option<ferrite_proxy::ProxyHandle>,
    /// Set when the proxy was wanted but is unusable (spawn or health-probe
    /// failure). First tab opens on `ferrite://error?reason=proxy-down`
    /// instead of failing silently or browsing direct without saying so.
    proxy_down: bool,
    proxy_requests: u64,
    proxy_dropped: u64,
    private_proxy_requests: u64,
    private_proxy_dropped: u64,
    /// Plaintext HTTPS-upgrade fallbacks observed (the fetch succeeded,
    /// just not over TLS). Counter only: fallback working as designed
    /// is not user-actionable, but silent downgrades must stay countable.
    proxy_downgraded: u64,
    /// History rows the vault writer was too full to take (burst above
    /// the 4096 channel). Retried on later ticks, oldest-first; the RAM
    /// cache already holds them, so nothing visible is lost meanwhile.
    pending_history: std::collections::VecDeque<(String, Option<String>, String)>,
    /// Spillover rows dropped for the cap above (sustained firehose past
    /// drain). Counted and warned, never silent.
    history_spill_dropped: u64,
    /// Hosts whose engine reported translation unavailable this session.
    /// The toolbar stops offering what the engine just proved it can't
    /// do, instead of failing late on every click.
    translate_unavailable: std::collections::HashSet<String>,
    /// DRM-denial notices for background tabs, shown when the tab goes
    /// active (the live arm only toasts the active tab, anti-spam).
    drm_pending: std::collections::HashMap<Uuid, String>,
    /// Permission broker (shared with the webview factory sync
    /// handler). Always-grants persist in settings. Everything else is
    /// session state.
    permissions: std::sync::Arc<ferrite_tabs::PermissionStore>,
    /// Device trust graph + pairing ceremony (settings UI). None without a
    /// vault (fail-closed: no trust UI, no pairing). Ephemeral processes get
    /// session-scoped trust on the memory vault (dies with the window).
    sync_trust: Option<SyncTrust>,
    /// Live LAN sync session (pinned QUIC + mDNS + CRDT rounds). Built after
    /// construction (needs an async bind); None in ephemeral mode and when
    /// the platform refuses (bind/mDNS failure runs transport-off, logged).
    sync_session: Option<std::sync::Arc<ferrite_sync::SyncSession>>,
    /// Category set shared with the session driver (UI toggles write,
    /// rounds read). Persisted to the data root on change (never read or
    /// written in ephemeral mode: incognito reads no files).
    sync_settings: std::sync::Arc<std::sync::RwLock<ferrite_sync::SyncSettings>>,
    /// Set by the sync thread when a round received actions: the tick
    /// refreshes history/bookmarks/newtab/settings surfaces once.
    sync_dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Flipped on shutdown so the sync thread (serve + rounds) exits
    /// promptly instead of riding out its sleep to process death.
    sync_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Pending manual-pairing approval: a decoded code awaiting Approve or
    /// Deny. Session-only by construction: never persisted, cleared on
    /// decision. Distinct from the transport's own pairing queue (which does
    /// not exist yet; live sync transport is Phase 2b).
    pending_pair: Option<ferrite_sync::PairingPayload>,
    /// Last pushed round's (actions_received, at_ms) for the bulk
    /// catch-up streak. Cell because pushes take &self; loop-confined,
    /// never shared across threads.
    sync_streak: std::cell::Cell<(usize, i64)>,
    /// Per-origin zoom cache (vault is persistence, no per-tick SQL).
    /// Unknown origins read 100% without storing anything.
    zoom_cache: std::collections::HashMap<String, f64>,
    /// Last-applied (origin, factor) per live tab. The tick restores any
    /// drifted active webview from this. Navigation, switch, and session
    /// restore funnel through one check, no per-event plumbing.
    zoom_applied: std::collections::HashMap<Uuid, (String, f64)>,
    privacy: std::sync::Arc<ferrite_privacy::PrivacyEngine>,
    /// Encrypted store for history/bookmarks/imports. `None` when the
    /// keyring or vault fails to open. Import/export then logs and
    /// declines instead of running on a half-built store (fail-closed).
    vault: Option<std::sync::Arc<ferrite_vault::Vault>>,
    /// Retained archive handle for rotation (the proxy provider and the
    /// factory hold their own clones; this one serves rekey). `None` when
    /// the archive failed to open or in guest mode.
    archive_store: Option<std::sync::Arc<ferrite_archive::Archive>>,
    /// Boot gate: set when encrypted stores failed to open (see
    /// [`BootBlock`]). Cleared by Retry-success or explicit Guest.
    boot_block: Option<BootBlock>,
    /// Profile picker pending: the `ferrite://profiles` tab owns first
    /// paint until a choice resolves it (switch spawns + exits, default
    /// and incognito continue in place, quit exits).
    profile_pick_pending: bool,
    /// Boot clock (construction instant) + stores-resolved stamp for the
    /// local-only timing log. Paint stamps at bootstrap end.
    boot_t0: Instant,
    boot_stores_ms: Option<u64>,
    /// Explicit ephemeral mode, chosen on the decision screen and bannered
    /// on every newtab. Memory stores, no persistence, no restore.
    guest_mode: bool,
    /// Profile id owning this process's stores ("Default" unless
    /// `--profile`). Scopes the session file, crash marker, and forward
    /// endpoint; vault/archive/settings scope via the boot session.
    profile_id: String,
    /// Guest profile handle (temp dir owner). Destroyed on clean exit;
    /// crash leftovers are swept at boot. `None` outside guest sessions.
    guest_profile: Option<crate::profiles::Profile>,
    /// Guest download temp dir, shared with the webview factory (engine
    /// downloads land here, never in Downloads). Set at guest entry.
    dl_dir_slot: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    /// First-guest-download warning toast (once per session).
    guest_dl_warned: bool,
    /// Decision-screen Quit: graceful exit on the next event-loop wait.
    quit_requested: bool,
    /// Decision-screen Retry: spawn a fresh process (clean state, real
    /// keyring read, full store verification) and exit this one.
    relaunch_requested: bool,
    /// Active DESIGN pack (persisted as `chrome_pack` in settings).
    theme_pack: ThemePack,
    /// Home button destination (persisted as `home_url`). Only
    /// http/https/ferrite URLs are accepted. Anything else falls back.
    home_url: String,
    /// Bookmarks bar mode: `always` | `never` | `ntp` (persisted).
    bar_mode: String,
    /// Auto-reveal fired once, so dismissing the bar sticks.
    bar_seen: bool,
    /// Vertical tab rail (persisted as `tabs_vertical`). The strip docks
    /// left at `VTAB_W_PX`; the content slot shifts right to match.
    tabs_vertical: bool,
    /// Side panel: open, section (`bookmarks`|`history`|`reading`), width.
    panel_open: bool,
    panel_section: String,
    panel_w: u32,
    /// Find-in-page: transient session state only. Never persisted, never
    /// logged (a query can reveal medical, financial, or legal intent).
    find_open: bool,
    find_query: String,
    find_case: bool,
    find_total: u32,
    find_index: u32,
    find_tab: Option<Uuid>,
    /// Last non-empty find term: prefill default for the next Ctrl+F.
    last_find: String,
    /// Content context menu: stored payload + overlay handle. Actions run
    /// against the stored payload, never later page-supplied data.
    ctx_menu: Option<CtxPayload>,
    ctx_overlay: Option<Rc<dyn WebViewHandle>>,
    /// Latest scanner fields per tab (fill-offer hit testing).
    scan_cache: std::collections::HashMap<Uuid, Vec<ScanField>>,
    /// Fill offers for the open menu (rebuilt per open, cleared on hide).
    fill_options: Vec<PendingFill>,
    /// Focus-dropdown type-ahead state (overlay shared with context menu).
    /// Rows render from `af_items`, narrow by `af_filter`, highlight `af_hl`.
    /// All reset on hide.
    af_open: bool,
    af_items: Vec<serde_json::Value>,
    af_filter: String,
    af_hl: Option<usize>,
    /// Credential pair fills for the open dropdown (same lifecycle).
    cred_fills: Vec<PendingCredFill>,
    /// Form-history fills for the open dropdown (same lifecycle).
    form_fills: Vec<PendingFormFill>,
    /// Last generator length picked on the passwords page (the dropdown
    /// has no room for length UI, so it follows the manager control).
    /// Session-only, default 20. Charset stays full in both.
    gen_length: usize,
    /// TOTP code fills for the open dropdown (same lifecycle).
    totp_fills: Vec<PendingTotpFill>,
    /// Generated-password target: (tab, password selector) for `af-gen`.
    af_gen: Option<(Uuid, String)>,
    /// Manager URL behind the dropdown footer row (`af-manage`).
    af_footer: Option<String>,
    /// Extracted articles by source URL, capped. Save-for-later reuses
    /// this pipeline, not a second one.
    reader_cache: std::collections::VecDeque<(String, ReaderArticle)>,
    /// In-flight chunked extractions, per extracting tab.
    reader_pending: std::collections::HashMap<Uuid, ReaderAssembly>,
    /// Reader typography prefs (own surface, not the chrome theme).
    reader_size: i32,
    reader_serif: bool,
    reader_wide: bool,
    reader_theme: String,
    /// Autofill toggles (independent: addresses without cards, or neither).
    autofill_address: bool,
    autofill_payment: bool,
    /// Form-history toggle (remember typed values per origin, default on,
    /// like the address and payment toggles).
    autofill_form: bool,
    /// On-device translation master switch plus target language (persisted as
    /// `translate_enabled` and `translate_target`). The engine is page-local.
    /// These gate only the triggers and the target passed to them.
    translate_enabled: bool,
    translate_target: String,
    /// DRM-block toasts already shown, as dedupe keys (tab, host, system).
    /// Session-only and bounded. One honest message per gated player, never
    /// a toast loop on retry-heavy streaming pages.
    drm_reported: std::collections::VecDeque<String>,
    /// In-flight print jobs per tab (the engine throws on concurrent jobs
    /// per webview instead of queueing). The guard refuses double triggers
    /// with a message, and navigation commits release the tab.
    print_jobs: ferrite_tabs::print::PrintGuard,
    /// Open PDF documents for `ferrite://pdf/` viewer tabs. Bytes and
    /// passwords live in RAM only (zeroized on close); the isolated
    /// renderer child owns every parse. LRU-capped; tab close and
    /// navigate-away drop the tab's sessions.
    pdf_sessions: crate::pdf::PdfSessions,
    /// Remote PDF fetch completions from worker threads (mirrors the
    /// notice/update channels: produced off-loop, drained on tick).
    pdf_fetch_tx: std::sync::mpsc::Sender<crate::pdf::PdfFetchDone>,
    pdf_fetch_rx: std::sync::mpsc::Receiver<crate::pdf::PdfFetchDone>,
    /// Engine PDF-link interceptions from the navigation veto (mirrors
    /// the ask-downloads divert: denied in-handler, opened on tick).
    /// The sender half lives in the factory only (like `dlask_tx`):
    /// nothing post-construction sends interceptions.
    pdf_intercept_rx: std::sync::mpsc::Receiver<ferrite_tabs::PdfIntercept>,
    /// Self-update backend: phase for the About hook, worker event bridge,
    /// last scheduled check, and whether a manual check is in flight.
    /// Background failures log, manual failures toast.
    update_phase: crate::update::UpdatePhase,
    update_tx: std::sync::mpsc::Sender<crate::update::UpdateEvent>,
    update_rx: std::sync::mpsc::Receiver<crate::update::UpdateEvent>,
    last_update_check: Instant,
    manual_update_check: bool,
    /// Previous version when this boot follows an applied update
    /// (marker consumed once in the constructor). The bootstrap opens
    /// About with an update banner exactly once from this.
    updated_from: Option<String>,
    /// Process table backend (task manager polls it, never owns PIDs).
    sys: sysinfo::System,
    /// Cert viewer fetch threads report back here (tick drain pushes).
    cert_tx: std::sync::mpsc::Sender<(Uuid, crate::cert::CertReport)>,
    cert_rx: std::sync::mpsc::Receiver<(Uuid, crate::cert::CertReport)>,
    /// File-picker results (dialogs run on workers. The picked path, or
    /// cancellation, lands here for the tick drain).
    filedlg_tx: std::sync::mpsc::Sender<crate::filedlg::PickDone>,
    filedlg_rx: std::sync::mpsc::Receiver<crate::filedlg::PickDone>,
    /// Script-dialog events from the Windows glue (the deferral is already
    /// held under the frame seq there). Same thread-bridge shape as
    /// cert and viewsource. Absent off-Windows, nothing ever arrives.
    #[cfg(target_os = "windows")]
    dialog_rx: std::sync::mpsc::Receiver<ferrite_tabs::DialogOpened>,
    /// Script-dialog queues plus suppression (platform-independent state,
    /// fed only where interception exists).
    dialogs: ferrite_tabs::DialogStore,
    /// Script-dialog overlay webview (`ferrite://dialog`, lazy) and the
    /// shown frame (tab plus seq). Overlay posts pair by seq. Stale posts
    /// (superseded frames, guessed ids) are ignored.
    dlg_overlay: Option<Rc<dyn WebViewHandle>>,
    dlg_shown: Option<(Uuid, u64)>,
    /// Interception sink for deferral completions plus suppression updates.
    /// `None` without factory interception (always, off-Windows).
    #[cfg(target_os = "windows")]
    dialog_sink: Option<ferrite_tabs::dialog_win::DialogSink>,
    /// Worker failure texts (save, export, fetch) for the toast. Success
    /// stays silent by design. Only failures travel this channel.
    notice_tx: std::sync::mpsc::Sender<String>,
    notice_rx: std::sync::mpsc::Receiver<String>,
    /// Ask-every-time divert: denied engine downloads land here. The
    /// drain shows a save dialog and refetches. Sender moves into the
    /// factory at build, only the receiver lives here.
    dlask_rx: std::sync::mpsc::Receiver<ferrite_tabs::DeniedDownload>,
    /// Ask-every-time flag, shared with the factory sync handler
    /// (persisted as `ask_downloads`, default off).
    ask_downloads: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Last-used save directory (explicit Save-As only, never the
    /// passive-download default).
    save_last_dir: Option<std::path::PathBuf>,
    /// Staged plaintext password CSV between the export post and the
    /// save pick. Cleared on write or cancel. Never lingers.
    pending_export: Option<String>,
    /// Staged export bytes (everything-tar or sealed backup) between the
    /// build post and the save pick. Cleared (zeroized) on write or
    /// cancel. Never lingers.
    pending_blob: Option<Vec<u8>>,
    /// Backup file picked for restore, awaiting its passphrase from the
    /// page. Cleared on restore or cancel.
    pending_backup_path: Option<std::path::PathBuf>,
    /// In-flight blob rescues: ((tab, transfer id) to (bytes, received,
    /// started)). Page-side fetch streams base64 slices in (IPC-capped).
    /// Stale entries sweep in tick. 100MB cap per transfer.
    blob_asm: std::collections::HashMap<(Uuid, String), (Vec<u8>, u64, std::time::Instant)>,
    /// Blob transfer id counter (per-process unique with the tab id).
    blob_seq: u64,
    /// Toast queue (transient confirmations: failures, completions,
    /// clipboard, undoable deletes). Same-source coalesces. Queues
    /// across sources one at a time, chrome renders the head only.
    toasts: crate::toast::ToastQueue,
    /// Staged rows behind the visible Undo toast (single slot, a newer
    /// undoable delete supersedes). Dropped on expiry, dismiss, supersede
    /// or consume. Then the delete stands.
    pending_undo: Option<crate::toast::PendingUndo>,
    /// Staged secret behind the visible Save-login toast (single slot).
    /// Dropped (zeroized) on expiry, dismiss, supersede or consume. The
    /// password bytes never outlive the prompt that offered to save them.
    pending_save: Option<crate::toast::PendingSave>,
    /// Password-import conflicts awaiting user choice (same origin and
    /// username, different password, existing entry kept). Plaintext lives
    /// here only until resolved, zeroized on drop, never pushed to any page.
    pw_conflicts: Vec<PendingPasswordConflict>,
    /// (origin, username) pairs already prompted this session. Login
    /// retries must not nag on every submit. Bounded, cleared when full
    /// (re-prompting beats unbounded growth).
    save_prompted: std::collections::HashSet<(String, String)>,
    /// Tabs with a fetch in flight (dedupes rapid re-requests).
    cert_pending: std::collections::HashSet<Uuid>,
    /// Last viewer report per tab: (host, leaf fingerprint). A trust
    /// action must match these. The page may ask to trust only the cert
    /// Ferrite actually fetched, never an arbitrary pasted fingerprint.
    cert_last: std::collections::HashMap<Uuid, (String, String)>,
    /// View-source fetch threads report back here (tick drain pushes).
    vs_tx: std::sync::mpsc::Sender<(Uuid, crate::viewsource::VsReport)>,
    vs_rx: std::sync::mpsc::Receiver<(Uuid, crate::viewsource::VsReport)>,
    /// Tabs with a source fetch in flight.
    vs_pending: std::collections::HashSet<Uuid>,
    /// Remote suggestion threads report back here: (generation, input
    /// snapshot, phrases). The generation discards stale (slower, older)
    /// responses so they never overwrite a newer list.
    sug_tx: std::sync::mpsc::Sender<(u64, String, Vec<String>)>,
    sug_rx: std::sync::mpsc::Receiver<(u64, String, Vec<String>)>,
    sug_gen: u64,
    /// Latest accepted remote rows: (generation, input snapshot, rows).
    /// Merged only while the snapshot still matches the live input.
    remote_sugs: (u64, String, Vec<SugRow>),
    /// Remote suggestions toggle (explicit opt-in, default off).
    remote_enabled: bool,
    /// Favicon icon-URL -> rendered data URL (raster only, capped).
    /// Bounded LRU-ish: insertion order in `favicon_order`, evicted past
    /// `FAVICON_CACHE_CAP`. Session-only; restores refetch on demand.
    favicon_cache: std::collections::HashMap<String, String>,
    favicon_order: std::collections::VecDeque<String>,
    /// Favicon fetch threads report back here (tick drain applies).
    favicon_tx: std::sync::mpsc::Sender<crate::favicon::FaviconDone>,
    favicon_rx: std::sync::mpsc::Receiver<crate::favicon::FaviconDone>,
    /// Icon URLs with a fetch in flight (dedupes reporter chatter).
    favicon_pending: std::collections::HashSet<String>,
    /// Dictionary provisioning reports (tick drain logs per tag).
    dict_tx: std::sync::mpsc::Sender<crate::dicts::DictDone>,
    dict_rx: std::sync::mpsc::Receiver<crate::dicts::DictDone>,
    /// First-tick dictionary provisioning (runs once, after everything
    /// exists, so boot ordering stays out of the giant constructor).
    dict_boot_pending: bool,
    /// OS accent follow (Linux GNOME / FERRITE_ACCENT override). `None`
    /// keeps the pack accent. Resolved once at boot, applied per push.
    system_accent: Option<String>,
    /// Auto-PiP: when the active tab switches away from a page holding a
    /// playing video, float it without waiting for the button. Explicit
    /// opt-in, default off. Persisted as `pip_auto`.
    pip_auto: bool,
    /// Pending native-share request: OG extraction evaluated, waiting on
    /// the `share_meta` report (or the 2s timeout sweep in `tick`). Tab id
    /// only. Payload rebuilds from the live tab, never the stale click.
    pending_share: Option<(Uuid, Instant)>,
    /// Recently-run palette commands, MRU front, capped. Session-only by
    /// design: command frecency is convenience state, not worth a settings
    /// schema entry or disk writes on every run.
    palette_recent: std::collections::VecDeque<String>,
    /// Last palette snapshot pushed while the overlay is open. Tabs mutate
    /// underneath the popup (closes, loads). The tick re-pushes on change
    /// so a run never targets a stale row.
    palette_sig: String,
    /// View-source line wrap (persisted preference).
    viewsource_wrap: bool,
    /// Last task-manager push (500ms throttle, like downloads progress).
    last_tm_push: Instant,
    /// Favicon data URLs last pushed, by tab id string. Rows carry the
    /// icon only on change (the client caches the rest); keeps pushes
    /// small when 200 icon-heavy tabs re-render. Cleared on
    /// RequestState (fresh consumer needs everything).
    favicon_sent: std::collections::HashMap<String, String>,
    /// Engine profile size (bytes) + when measured. Refreshed at most
    /// every 60s and only while a task-manager page is open: directory
    /// walks are slow-path work, never heartbeat work.
    engine_profile_bytes: u64,
    engine_profile_at: Instant,
    /// Crash-restore infobar: shown once post-crash (window-scoped), plus
    /// the autosaved tab count it offers. "Always restore" skips the bar.
    crash_offer: bool,
    crash_tabs: u32,
    restore_always: bool,
    /// Startup behavior: `restore` (silent session reopen), `newtab`
    /// (fresh blank tab), `urls` (open `startup_urls`). Persisted.
    startup_mode: String,
    /// Startup URLs for `urls` mode: validated, deduped, capped.
    startup_urls: Vec<String>,
    /// History retention, days (persisted as `history_retention_days`).
    /// 0 = keep forever. Pruned daily by `retention_sweep`.
    history_retention_days: u64,
    /// Last retention sweep (history prune + free-page reclaim).
    last_retention_sweep: Instant,
    /// Ask which profile at startup (picker page). Default true; only
    /// matters when several profiles exist and no `--profile` was given.
    profile_picker: bool,
    /// Auto-sleep timeout, minutes (persisted as `sleep_timeout_min`).
    /// 0 = never (auto-discard off, manual Sleep still works).
    sleep_timeout_min: u64,
    /// Sleep exception hosts, normalized lowercase (persisted as
    /// `sleep_exceptions`). Suffix-matched; auto-discard only.
    sleep_exceptions: Vec<String>,
    /// Full restore page pending (crash loop): like the infobar offer,
    /// the session file stays frozen until answered.
    restore_pending: bool,
    /// First-run wizard state (checked once at boot, set by Done/Skip).
    onboarding_completed: bool,
    /// Last session autosave (crash restore needs live data, not just the
    /// clean-exit save, otherwise the offer restores stale tabs).
    last_autosave: Instant,
    /// Scroll offsets moved since the last save. Scroll is the only
    /// session state that bypasses the dirty flag (per-scroll writes
    /// would churn the disk); the backstop consults this instead.
    scroll_dirty: bool,
    /// Last OS memory sample feeding the discard table. Gated by
    /// MEM_SAMPLE_INTERVAL: sysinfo walks are slow-path work.
    mem_sample_at: Instant,
    /// Session block verdicts already flushed to the daily counter (the
    /// dashboard reads the vault, not these live counters).
    last_blocked_total: u64,
    /// Last threat-context refresh (frequent hosts plus override snapshot
    /// pushed into the privacy engine). 5-minute cadence. Frecency moves
    /// slowly, and the engine consults memory on every navigation.
    last_threat_push: Instant,
    /// Autocomplete suggestions toggle (local-only data. Default on).
    suggestions_enabled: bool,
    /// Full-window chrome webview (`ferrite://chrome`). Created at boot;
    /// content webviews are siblings positioned in the slot below it.
    chrome: Option<Rc<dyn WebViewHandle>>,
    chrome_id: Uuid,
    chrome_sig: String,
    /// HTML tab-search overlay. Lazily created on first open. Hidden
    /// otherwise so content webviews keep running underneath the dim
    /// backdrop.
    overlay: Option<Rc<dyn WebViewHandle>>,
    overlay_open: bool,
    modifiers: winit::keyboard::ModifiersState,
    window: Option<Arc<Window>>,
    app: AppContext,
    fullscreen: FullscreenMode,
    status_visible: bool,
    pending_error: Option<ShellError>,
    ipc_bridge: IpcBridge,
    /// Second-instance URL handovers (see `crate::forward`), drained by the
    /// tick. Same-mode only: the forwarder never crosses the
    /// normal/incognito boundary, so this queue is already scoped.
    forward_queue: crate::forward::ForwardQueue,
    downloads: ferrite_tabs::DownloadManager,
    /// Recent download events for the chrome status strip (capped ring).
    download_log: std::collections::VecDeque<ferrite_tabs::DownloadEvent>,
    /// Last completed download, for the toolbar checkmark flash window.
    last_dl_done: Option<Instant>,
    /// Hovered link (tab, raw href) for the status preview. Never drives
    /// network activity. Display only.
    hover: Option<(Uuid, String)>,
    /// Recently closed tabs (url, title), session-scoped for Ctrl+Shift+T.
    /// Distinct from History: ephemeral, capped, never persisted.
    closed_tabs: Vec<ClosedTab>,
    /// Latest filter per history tab (for live re-push on new visits).
    history_queries: std::collections::HashMap<Uuid, String>,
    /// Manager view per bookmarks tab (selected folder + search text).
    bm_views: std::collections::HashMap<Uuid, BmView>,
    /// Latest (filter, status) per downloads tab (for live progress).
    dl_queries: std::collections::HashMap<Uuid, (String, String)>,
    /// Last live progress push (the 500ms tick IS the throttle).
    last_dl_push: Instant,
    /// Last visibility sweep counts. The sweep logs only on change so
    /// steady multi-tab state does not spam the log.
    last_vis: (usize, usize),
}
impl Default for ShellApp {
    fn default() -> Self {
        Self::new(
            ShellConfig::default(),
            None,
            crate::forward::ForwardQueue::default(),
        )
    }
}

/// A closed tab kept for reopen. History, scroll, and pin restore
/// with it (parity with session restore and with Chrome/Firefox reopen).
/// Private tabs are never recorded: their URLs must not outlive the
/// tab even in memory. Memory-only: the stack dies with the process;
/// quit/crash recovery owns persistence through the session file.
#[derive(Debug, Clone, PartialEq)]
struct ClosedTab {
    url: String,
    title: String,
    history: NavigationHistory,
    scroll: (f32, f32),
    pinned: bool,
}

impl ClosedTab {
    /// History-less entry for sources that only carry URL+title
    /// (workspace drops, which filter private tabs, and crash-session
    /// docs, which exclude them): restores as a fresh navigation.
    fn bare(url: String, title: String) -> Self {
        Self {
            url,
            title,
            history: NavigationHistory::default(),
            scroll: (0.0, 0.0),
            pinned: false,
        }
    }
}

/// Recently-closed cap (Chrome/Firefox parity: 25 per window).
const CLOSED_CAP: usize = 25;

impl ShellApp {
    fn new(
        config: ShellConfig,
        wake: Option<EventLoopProxy<ShellWake>>,
        forward_queue: crate::forward::ForwardQueue,
    ) -> Self {
        // Boot clock for the local-only timing log (see BOOT_LOG_NAME):
        // construction-relative; phases below stamp against it.
        let boot_t0 = Instant::now();
        let theme = config.theme.clone();
        let status_visible = config.diagnostics_on;
        let ipc_bridge = IpcBridge::new();
        let downloads = ferrite_tabs::DownloadManager::new();
        // Guest download override slot, shared with the factory before it
        // is built (guest entry flips it to a temp dir later). Stale
        // crash-leftover guest dirs are swept here, once, by live-PID
        // check (never delete a running session's files).
        let dl_dir_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        crate::profiles::sweep_stale_guests();
        // Crash-orphaned download partials: no live transfer of ours can
        // be a day quiet (workers die after 30 minutes), so anything older
        // is safe to remove. Shared download dirs stay safe via recency.
        let stale_partials = ferrite_tabs::downloads::sweep_stale_partials(
            &ferrite_tabs::default_download_dir(),
            std::time::Duration::from_secs(86_400),
        );
        if stale_partials > 0 {
            tracing::debug!(stale_partials, "swept crash-orphaned partials");
        }
        // Rotation resume first (skipped for incognito: no keyring use,
        // no file stores, the journal waits for a normal boot): an
        // interrupted rotation converges here, before any store opens, so
        // every key read below is already the converged one. A failed
        // resume keeps its journal and blocks with a generic reason (the
        // detail stays in the log: file errors can carry paths, which must
        // never reach the decision screen).
        let resume_err = if config.incognito {
            None
        } else {
            match maybe_resume_rotation() {
                Ok(_) => None,
                Err(e) => {
                    warn!(error = %e, "rotation resume failed");
                    Some("An interrupted encryption-key rotation couldn't be completed.".to_owned())
                }
            }
        };
        // Device master key first: every persistent store below (settings,
        // vault, archive, session) encrypts under it. Durability travels
        // with the key: an ephemeral key must never back persistent files
        // (they would become unopenable time bombs tomorrow). Incognito
        // resolves nothing: zero keyring operations by design.
        let (master_key, key_durable, key_reason) = if config.incognito {
            (None, false, None)
        } else {
            resolve_master_key()
        };
        // Settings JSON is read once, sealed (legacy plaintext replayed):
        // pack selection must precede the factory build (first paint
        // already themed), engine restore lands on the context below.
        // Incognito reads no files: defaults only, nothing decrypted.
        let mut settings_json: serde_json::Value = if config.incognito {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            crate::settings_file::read_settings_value(master_key.as_deref())
        };
        // Managed first run: `master-preferences.json` beside the data
        // root seeds homepage, startup, and engine when no settings file
        // exists yet (fresh profile). Explicit user settings always win:
        // seeding runs once, on absence, never as an overlay.
        if !config.incognito && settings_json.as_object().is_some_and(|o| o.is_empty()) {
            let (path, _) = crate::settings_file::settings_paths();
            if !path.exists()
                && let Some(master) = read_master_prefs()
            {
                settings_json = apply_master_prefs(settings_json, &master);
            }
        }
        let theme_pack = settings_json
            .get("chrome_pack")
            .and_then(|x| x.as_str())
            .and_then(ThemePack::from_name)
            .unwrap_or(ThemePack::Tenebra);
        let suggestions_enabled = settings_json
            .get("suggestions")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        // Remote suggestions: explicit opt-in (keystrokes leave the
        // device), off unless the user turned it on.
        let remote_enabled = settings_json
            .get("suggestions_remote")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let pip_auto = settings_json
            .get("pip_auto")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let restore_always = settings_json
            .get("restore_always")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        // Startup behavior: restore (silent reopen), newtab (fresh), or
        // urls (open the list). Unknown modes fall back to restore.
        let startup_mode = clean_startup_mode(
            settings_json
                .get("startup_mode")
                .and_then(|x| x.as_str())
                .unwrap_or("restore"),
        )
        .to_owned();
        let startup_urls = settings_json
            .get("startup_urls")
            .map(clean_startup_urls)
            .unwrap_or_default();
        // History retention in days (0 = forever). Absurd values clamp:
        // a hostile file must not set billion-day retention (equivalent
        // to forever, silently) or churn daily deletes.
        let history_retention_days = settings_json
            .get("history_retention_days")
            .and_then(|x| x.as_u64())
            .unwrap_or(90);
        let history_retention_days = if history_retention_days == 0 {
            0
        } else {
            history_retention_days.clamp(7, 3650)
        };
        // Profile picker at startup: default on (only matters with
        // several profiles and no explicit `--profile`).
        let profile_picker = settings_json
            .get("profile_picker")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        // Auto-sleep: minutes of idle before background tabs unload (0 =
        // never). Clamped: a hostile settings file must not set absurd
        // timeouts (u64 days would pin tabs forever, silently).
        let sleep_timeout_min = settings_json
            .get("sleep_timeout_min")
            .and_then(|x| x.as_u64())
            .unwrap_or(30)
            .min(24 * 60);
        let sleep_exceptions = settings_json
            .get("sleep_exceptions")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(clean_sleep_host)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let onboarding_completed = settings_json
            .get("onboarding_completed")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let viewsource_wrap = settings_json
            .get("viewsource_wrap")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let autofill_address = settings_json
            .get("autofill_address")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let autofill_payment = settings_json
            .get("autofill_payment")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let autofill_form = settings_json
            .get("autofill_form")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let translate_enabled = settings_json
            .get("translate_enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let translate_target = settings_json
            .get("translate_target")
            .and_then(|x| x.as_str())
            .map(ferrite_tabs::translate::normalize_target)
            .unwrap_or_else(|| ferrite_tabs::translate::DEFAULT_TARGET.to_owned());
        let reader_size = settings_json
            .get("reader_size")
            .and_then(|x| x.as_i64())
            .map(|n| n.clamp(87, 150) as i32)
            .unwrap_or(100);
        let reader_serif = settings_json
            .get("reader_serif")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let reader_wide = settings_json
            .get("reader_wide")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let reader_theme = settings_json
            .get("reader_theme")
            .and_then(|x| x.as_str())
            .filter(|s| matches!(*s, "light" | "sepia" | "dark"))
            .unwrap_or("light")
            .to_owned();
        let home_url = settings_json
            .get("home_url")
            .and_then(|x| x.as_str())
            .filter(|u| valid_home_url(u))
            .unwrap_or("ferrite://newtab")
            .to_owned();
        let bar_mode = settings_json
            .get("bar_mode")
            .and_then(|x| x.as_str())
            .filter(|m| matches!(*m, "always" | "never" | "ntp"))
            .unwrap_or("never")
            .to_owned();
        let bar_seen = settings_json
            .get("bar_seen")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let tabs_vertical = settings_json
            .get("tabs_vertical")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let panel_open = settings_json
            .get("panel_open")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let panel_section = settings_json
            .get("panel_section")
            .and_then(|x| x.as_str())
            .filter(|s| matches!(*s, "bookmarks" | "history" | "reading"))
            .unwrap_or("bookmarks")
            .to_owned();
        let panel_w = settings_json
            .get("panel_w")
            .and_then(|x| x.as_u64())
            .map(|w| w.clamp(240, 480) as u32)
            .unwrap_or(320);

        let fixed_port = std::env::var("FERRITE_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok());
        let privacy_engine = {
            // Threat cache lives in the data root (public hashes only, see
            // the storage contract). Any open failure degrades to the
            // in-memory DB, never to another path: `PrivacyEngine::new`
            // touches no files by construction.
            let engine =
                ferrite_privacy::threat::ThreatDb::new_file(crate::profiles::threat_db_path())
                    .map_err(|e| {
                        error!(error = %e, "threat cache open failed, in-memory fallback");
                        e
                    })
                    .and_then(|db| {
                        ferrite_privacy::PrivacyEngine::with_threat_db(db).map_err(|e| {
                            error!(error = %e, "privacy engine init failed, in-memory fallback");
                            e.to_string()
                        })
                    })
                    .or_else(|_| {
                        ferrite_privacy::PrivacyEngine::new().map_err(|e| {
                    error!(error = %e, "privacy engine init failed, falling back to defaults");
                    e.to_string()
                })
                    });
            let engine = std::sync::Arc::new(
                engine.unwrap_or_else(|_| ferrite_privacy::PrivacyEngine::default()),
            );
            engine.import_settings_json(&settings_json.to_string());
            let _ = engine.load_filters(&[
                ferrite_privacy::FilterList {
                    name: "easylist_startup",
                    category: ferrite_privacy::ListCategory::Ads,
                    text: "||ads.example^\n",
                    expected_sha256: None,
                },
                ferrite_privacy::FilterList {
                    name: "easyprivacy_startup",
                    category: ferrite_privacy::ListCategory::Tracking,
                    text: "||tracker.test^\n",
                    expected_sha256: None,
                },
            ]);
            let _ = engine.load_threat_snapshot(&["phishing.test", "malware.test"]);
            let updater = std::sync::Arc::clone(&engine);
            updater.spawn_hourly_updater(
                || {
                    // One maintained list per category: env URL wins, file
                    // fallback next, absent source simply omitted (the other
                    // lists still swap. Categories degrade independently,
                    // never all-or-nothing on one missing mirror).
                    // `None` retains last-known-good. Never a stub anymore.
                    let mut out = Vec::new();
                    // Ferrite's own fetch: direct origin GET (never the
                    // loopback proxy), same checksum contract as file loads.
                    if let Ok(url) = std::env::var("FERRITE_FILTER_URL") {
                        if let Some((text, sha)) = block_on_direct_fetch(&url) {
                            out.push(ferrite_privacy::FilterFetch {
                                name: "easylist".to_owned(),
                                category: ferrite_privacy::ListCategory::Ads,
                                text,
                                sha256: sha,
                            });
                        }
                    } else if let Ok(text) = std::fs::read_to_string("easylist.txt") {
                        out.push(ferrite_privacy::FilterFetch {
                            name: "easylist".to_owned(),
                            category: ferrite_privacy::ListCategory::Ads,
                            text,
                            sha256: None,
                        });
                    }
                    if let Ok(url) = std::env::var("FERRITE_EASYPRIVACY_URL") {
                        if let Some((text, sha)) = block_on_direct_fetch(&url) {
                            out.push(ferrite_privacy::FilterFetch {
                                name: "easyprivacy".to_owned(),
                                category: ferrite_privacy::ListCategory::Tracking,
                                text,
                                sha256: sha,
                            });
                        }
                    } else if let Ok(text) = std::fs::read_to_string("easyprivacy.txt") {
                        out.push(ferrite_privacy::FilterFetch {
                            name: "easyprivacy".to_owned(),
                            category: ferrite_privacy::ListCategory::Tracking,
                            text,
                            sha256: None,
                        });
                    }
                    if let Ok(url) = std::env::var("FERRITE_FINGERPRINT_URL") {
                        if let Some((text, sha)) = block_on_direct_fetch(&url) {
                            out.push(ferrite_privacy::FilterFetch {
                                name: "fingerprinting".to_owned(),
                                category: ferrite_privacy::ListCategory::Fingerprinting,
                                text,
                                sha256: sha,
                            });
                        }
                    } else if let Ok(text) = std::fs::read_to_string("fingerprinting.txt") {
                        out.push(ferrite_privacy::FilterFetch {
                            name: "fingerprinting".to_owned(),
                            category: ferrite_privacy::ListCategory::Fingerprinting,
                            text,
                            sha256: None,
                        });
                    }
                    out
                },
                || {
                    if let Ok(url) = std::env::var("FERRITE_THREAT_URL") {
                        block_on_direct_fetch(&url)
                            .and_then(|(body, _)| serde_json::from_str::<Vec<String>>(&body).ok())
                    } else if let Ok(data) = std::fs::read_to_string("threats.json") {
                        serde_json::from_str::<Vec<String>>(&data).ok()
                    } else {
                        None
                    }
                },
            );
            engine
        };
        // Sovereign Archive and Vault open together, once, under one key:
        // any failure collects plain-language reasons for the boot gate's
        // decision screen (never a silent fallback). See STORAGE_CONTRACT.
        // Incognito opens nothing: the ephemeral entry below owns stores.
        let (vault, archive, boot_block) = if config.incognito {
            (None, None, None)
        } else {
            let outcome = open_persistent_stores(master_key, key_durable, key_reason);
            let mut gate_reasons = outcome.reasons;
            if let Some(e) = resume_err {
                gate_reasons.insert(0, e);
            }
            let block = if gate_reasons.is_empty() {
                None
            } else {
                Some(BootBlock {
                    reasons: gate_reasons,
                    tab: None,
                })
            };
            (outcome.vault, outcome.archive, block)
        };
        // Stores resolved (or gate-blocked): stamp the phase for the log.
        let boot_stores_ms = boot_t0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        // Offline snapshot fallback for the proxy, fed by the archive that
        // just opened (absent when the gate blocked: no fallback, and the
        // decision screen says so).
        let archive_provider =
            archive.clone().map(|store| {
                std::sync::Arc::new(move |url: &str| {
                    store.get_latest(url).ok().flatten().map(|snap| {
                        ferrite_proxy::ArchiveSnapshot {
                            html: snap.html.into_bytes(),
                            headers: snap.headers,
                            timestamp: snap.timestamp,
                        }
                    })
                }) as std::sync::Arc<ferrite_proxy::ArchiveProvider>
            });
        let proxy = if !privacy_engine.settings().proxy_enabled {
            // Explicit user opt-out (needs a restart to take effect. The
            // factory pins the port at webview creation, Microsoft pins the
            // environment at init: no live toggle exists on this path).
            info!("proxy disabled in settings: webviews route direct");
            None
        } else {
            match ferrite_proxy::spawn(
                ferrite_proxy::ProxyConfig {
                    fixed_port,
                    archive: archive_provider,
                    dns: privacy_engine.dns_config(),
                },
                privacy_engine.clone() as std::sync::Arc<dyn ferrite_proxy::DecisionHook>,
            ) {
                Ok(handle) => {
                    // Health probe: a bound socket that never accepts is a
                    // dead browser with no symptom. One blocking 500ms dial
                    // here (boot, not the hot path) buys a visible error
                    // page instead of "the internet is broken".
                    let probe = std::net::SocketAddr::from(([127, 0, 0, 1], handle.port));
                    match std::net::TcpStream::connect_timeout(
                        &probe,
                        std::time::Duration::from_millis(500),
                    ) {
                        Ok(_) => {
                            info!(
                                port = handle.port,
                                "Ferrite proxy listening: all webview traffic routes through it"
                            );
                            Some(handle)
                        }
                        Err(err) => {
                            error!(error = %err, "proxy health probe failed: treating proxy as down");
                            handle.stop();
                            None
                        }
                    }
                }
                Err(err) => {
                    error!(error = %err, "proxy spawn failed: first tab shows proxy-down error page");
                    None
                }
            }
        };
        // Down = wanted-but-unusable. Disabled-by-setting is NOT down (the
        // user chose direct). Only a failed spawn or probe lands the error page.
        let proxy_down = privacy_engine.settings().proxy_enabled && proxy.is_none();
        let proxy_port = proxy.as_ref().map(|p| p.port);
        // PDF stack capability, said once at boot: the renderer child plus
        // the pdfium library ship beside the binary. Absent files mean dead
        // viewer/search/fill, reported here instead of per-document warnings.
        {
            let (renderer_present, pdfium_source) = ferrite_archive::pdf::pdf_boot_report();
            if renderer_present && pdfium_source != "system-or-missing" {
                info!(
                    pdfium_source = pdfium_source,
                    "pdf stack ready: renderer shipped, engine resolvable"
                );
            } else {
                warn!(
                    renderer_present = renderer_present,
                    pdfium_source = pdfium_source,
                    "pdf degraded: viewer/search/fill unavailable until the pdf stack ships beside the binary"
                );
            }
        }
        // Process session identity: one enum owns user-vs-incognito for the
        // whole process (proxy partition key, path owner, chrome signal).
        // Created here so the private proxy + factory below derive from it.
        let boot_session =
            crate::profiles::Session::for_boot_profile(config.incognito, config.profile.as_deref());
        // Profile id for scoped paths (session file, crash marker, forward
        // endpoint). Ephemeral sessions keep legacy global names.
        let profile_id = match &boot_session {
            crate::profiles::Session::Persistent { profile } => profile.id.clone(),
            crate::profiles::Session::Ephemeral { .. } => crate::profiles::DEFAULT_ID.to_owned(),
        };
        // Session-partitioned proxy for private tabs: own loopback port +
        // own ConnPool, no archive fallback (private payloads must never
        // become snapshots). Static filter rules stay shared read-only via
        // the same DecisionHook. Only in persistent processes with a live
        // main proxy; ephemeral processes already isolate by process.
        // Failure is non-fatal: private tabs fall back to the shared proxy,
        // never direct.
        let private_proxy = if config.incognito
            || proxy_port.is_none()
            || !privacy_engine.settings().proxy_enabled
        {
            None
        } else {
            match ferrite_proxy::spawn(
                ferrite_proxy::ProxyConfig {
                    fixed_port: None,
                    archive: None,
                    dns: privacy_engine.dns_config(),
                },
                privacy_engine.clone() as std::sync::Arc<dyn ferrite_proxy::DecisionHook>,
            ) {
                Ok(handle) => {
                    let probe = std::net::SocketAddr::from(([127, 0, 0, 1], handle.port));
                    match std::net::TcpStream::connect_timeout(
                        &probe,
                        std::time::Duration::from_millis(500),
                    ) {
                        Ok(_) => {
                            info!(
                                port = handle.port,
                                session = boot_session.id(),
                                "Ferrite private proxy listening: private tabs partitioned"
                            );
                            Some(handle)
                        }
                        Err(err) => {
                            error!(error = %err, "private proxy probe failed: private tabs share main proxy");
                            handle.stop();
                            None
                        }
                    }
                }
                Err(err) => {
                    error!(error = %err, "private proxy spawn failed: private tabs share main proxy");
                    None
                }
            }
        };
        let private_proxy_port = private_proxy.as_ref().map(|p| p.port);
        // Bundled spellcheck dictionaries: page-initiated fetches stay
        // blocked at the proxy either way (fewer suggestions, never a
        // page fetch). Missing dictionaries for the active languages are
        // provisioned once, first tick, through the direct client (see
        // TELEMETRY.md). The count here tells operators whether files
        // are actually installed yet.
        debug!(
            dicts = ferrite_privacy::bundled_dict_count(),
            dir = %ferrite_privacy::dict_bundle_dir().display(),
            "bundled spellcheck dictionaries"
        );

        // Script-dialog interception channel plus sink slot (Windows WebView2
        // ScriptDialogOpening via the public wry surface, engine defaults
        // elsewhere). The factory block below consumes the sender. The
        // sink is cloned back out for deferral completions.
        #[cfg(target_os = "windows")]
        let (dialog_tx, dialog_rx) = std::sync::mpsc::channel();
        // Deferred init: assigned exactly once in the factory block below.
        #[cfg(target_os = "windows")]
        let dialog_sink: Option<ferrite_tabs::dialog_win::DialogSink>;
        // Ask-every-time divert channel + flag (factory handler needs
        // both before the factory block below).
        let (dlask_tx, dlask_rx) = std::sync::mpsc::channel();
        // Engine PDF-link interceptions (vetoed in-handler, opened on
        // tick; same placement constraint as the divert above).
        let (pdf_intercept_tx, pdf_intercept_rx) = std::sync::mpsc::channel();
        let pdf_intercept_on = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ask_downloads = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            settings_json
                .get("ask_downloads")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        ));
        // Permission broker: fail-closed sync decisions, pending rows
        // surface as the omnibox chip (never a banner). Truth lives in the
        // Vault `site_permissions` table (SQLCipher). The legacy plaintext
        // `perm_grants` key migrates once, then is deleted.
        let permissions = std::sync::Arc::new(ferrite_tabs::PermissionStore::new());
        // Global notifications kill switch (Settings to Privacy, persisted
        // like any other setting, default on).
        permissions.set_notifications_global(
            settings_json
                .get("notifications_global")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
        );
        let perm_now = ferrite_tabs::now_millis();
        let mut perm_booted = false;
        if let Some(v) = vault.as_ref() {
            match v.list_permission_grants() {
                Ok(rows) => {
                    let mut snap = Vec::with_capacity(rows.len());
                    for r in rows {
                        match ferrite_tabs::GrantSource::parse(&r.source) {
                            Some(source) => snap.push(ferrite_tabs::StoredGrant {
                                origin: r.origin,
                                kind: r.kind,
                                source,
                                granted_at_ms: r.granted_at,
                                expires_at_ms: r.expires_at,
                            }),
                            None => {
                                warn!(origin = %r.origin, kind = %r.kind, "permission row with unknown source skipped")
                            }
                        }
                    }
                    permissions.load_snapshot(snap, perm_now);
                    perm_booted = true;
                    // Automation write-through: cooldown trips and expiries land
                    // in Vault from the decide path (user grants persist
                    // explicitly at their call sites, same snapshot shape).
                    let vv = std::sync::Arc::clone(v);
                    permissions.set_snapshot_sink(std::sync::Arc::new(move |snap| {
                        let rows: Vec<ferrite_vault::vault::SitePermissionRow> = snap
                            .into_iter()
                            .map(|g| ferrite_vault::vault::SitePermissionRow {
                                origin: g.origin,
                                kind: g.kind,
                                source: g.source.as_str().to_owned(),
                                granted_at: g.granted_at_ms,
                                expires_at: g.expires_at_ms,
                            })
                            .collect();
                        if let Err(e) = vv.replace_permission_grants(&rows) {
                            debug!(error = %e, "permission snapshot write-through failed");
                        }
                    }));
                }
                Err(e) => warn!(error = %e, "site permissions unreadable: legacy fallback"),
            }
        }
        if !perm_booted
            && let Some(grants) = settings_json.get("perm_grants").and_then(|x| x.as_array())
        {
            let pairs: Vec<(String, String)> = grants
                .iter()
                .filter_map(|g| {
                    let o = g.get("origin")?.as_str()?.to_owned();
                    let k = g.get("kind")?.as_str()?.to_owned();
                    Some((o, k))
                })
                .collect();
            permissions.load_always(pairs, perm_now);
            // One-way migration into the encrypted table. The plaintext key
            // is deleted only when the Vault actually persists to disk.
            if let Some(v) = vault.as_ref()
                && v.is_persistent()
            {
                let snap = permissions.export_snapshot(perm_now);
                let rows: Vec<ferrite_vault::vault::SitePermissionRow> = snap
                    .into_iter()
                    .map(|g| ferrite_vault::vault::SitePermissionRow {
                        origin: g.origin,
                        kind: g.kind,
                        source: g.source.as_str().to_owned(),
                        granted_at: g.granted_at_ms,
                        expires_at: g.expires_at_ms,
                    })
                    .collect();
                match v.replace_permission_grants(&rows) {
                    Ok(()) => clear_setting("perm_grants"),
                    Err(e) => warn!(error = %e, "permission migration failed: legacy key kept"),
                }
            }
        }
        // Threat context: user overrides into the engine (exact-host
        // bypass for list plus heuristic blocks) and the first frequent-host
        // snapshot for lookalike checks. Both refresh on tick. Vault is
        // the truth, the engine holds memory copies for the hot path.
        if let Some(v) = vault.as_ref() {
            match v.list_threat_exceptions() {
                Ok(hosts) => privacy_engine.set_threat_exceptions(hosts),
                Err(e) => warn!(error = %e, "threat overrides unreadable: none loaded"),
            }
            privacy_engine.set_frequent_hosts(v.top_hosts(256));
        }
        // Device trust graph: this device's long-term identity (vault-sealed
        // seed, stable id) plus the encrypted trust store the pairing
        // ceremony reads and writes. No vault, no trust UI (fail-closed).
        // Memory vaults yield session-scoped trust that dies with the
        // process, exactly like every other ephemeral store.
        // Category set for future sync rounds (shared with the session
        // driver when it starts). Ephemeral boots read no files: defaults
        // only, toggles apply for the window and are never persisted.
        let sync_settings = std::sync::Arc::new(std::sync::RwLock::new(if config.incognito {
            ferrite_sync::SyncSettings::default()
        } else {
            ferrite_sync::SyncSettings::load_from_file(
                &crate::profiles::data_root()
                    .join("sync-categories.json")
                    .to_string_lossy(),
            )
        }));
        let sync_trust = match vault.as_ref() {
            Some(v) => match init_sync_trust(v) {
                Ok(t) => Some(t),
                Err(e) => {
                    warn!(error = %e, "sync trust unavailable");
                    None
                }
            },
            None => None,
        };
        // Incognito engine profile: temp guest dir created up front so the
        // factory pins an isolated cache/cookie jar. The ephemeral entry
        // takes it over below (destroyed on clean exit). Derived from the
        // process session so identity stays single-sourced.
        let incognito_profile: Option<crate::profiles::Profile> = match &boot_session {
            crate::profiles::Session::Ephemeral { guest, .. } => {
                let _ = std::fs::create_dir_all(crate::profiles::engine_dir(guest));
                Some(guest.clone())
            }
            crate::profiles::Session::Persistent { .. } if config.incognito => {
                Some(crate::profiles::guest())
            }
            _ => None,
        };
        // Private-tab spill dir for persistent processes (see profiles):
        // created once, wiped at clean exit, swept at boot after crashes.
        let private_dl_dir = if config.incognito {
            None
        } else {
            let dir = crate::profiles::private_downloads_dir();
            match std::fs::create_dir_all(&dir) {
                Ok(()) => Some(dir),
                Err(e) => {
                    warn!(error = %e, "private downloads dir unwritable: private tabs share default");
                    None
                }
            }
        };
        crate::profiles::sweep_stale_private();
        let factory: Rc<dyn ferrite_tabs::WebViewFactory> = {
            let mut f = WryFactory::new(ipc_bridge.sender.clone(), ipc_bridge.limiter());
            f = f.with_downloads(downloads.clone());
            f = f.with_download_dir_override(std::sync::Arc::clone(&dl_dir_slot));
            if let Some(ref dir) = private_dl_dir {
                f = f.with_private_download_dir(dir.clone());
            }
            f = f.with_download_ask(dlask_tx, std::sync::Arc::clone(&ask_downloads));
            f = f.with_pdf_intercept(
                pdf_intercept_tx.clone(),
                std::sync::Arc::clone(&pdf_intercept_on),
            );
            f = f.with_permissions(std::sync::Arc::clone(&permissions));
            if let Some(ref store) = archive {
                f = f.with_archive(std::sync::Arc::clone(store));
            }
            // Proxy port is pinned here for the process lifetime: wry routes
            // each webview at creation (and WebView2 pins the environment at
            // init, Microsoft offers no post-init change). Toggling the proxy or
            // its port needs a restart or rebuild, never a live switch.
            // Private tabs pin the partitioned port when present (own pool,
            // no shared keep-alive across modes); fallback is the shared
            // proxy, never direct.
            if let Some(port) = proxy_port {
                f = f.with_proxy("127.0.0.1", port);
            }
            if let Some(port) = private_proxy_port {
                f = f.with_private_proxy("127.0.0.1", port);
            }
            // Engine profile (HTTP disk cache included): one predictable dir
            // next to the Ferrite stores, created once. Best-effort. An
            // unwritable CWD keeps the engine default rather than failing
            // boot (logged, never silent). The cache itself stays engine
            // format plus unencrypted: OS disk encryption is its protection
            // (see the About page note). History and vault stay SQLCipher.
            // Incognito runs the engine from a temp guest profile that the
            // ephemeral entry below takes over (destroyed on clean exit).
            let profile = crate::profiles::Profile::default_profile();
            let profile_dir =
                crate::profiles::engine_dir(incognito_profile.as_ref().unwrap_or(&profile));
            match std::fs::create_dir_all(&profile_dir) {
                Ok(()) => {
                    f = f.with_data_dir(profile_dir);
                }
                Err(e) => {
                    warn!(error = %e, "profile dir unwritable: engine default location used");
                }
            }
            f = f.with_asset(
                "geist-pixel-square.ttf",
                crate::fonts::GEIST_PIXEL_SQUARE_TTF,
            );
            f = f.with_asset(
                "jetbrains-mono-regular.ttf",
                crate::fonts::JETBRAINS_MONO_TTF,
            );
            // Single theme source: shell tokens plus the active pack
            // (`--f-*` names overlap, pack wins by cascade order, so the
            // first paint and every ferrite:// page share one theme). The
            // trailing color-scheme follows the pack (not the OS), so
            // prefers-color-scheme queries in pages track the design pack
            // the user picked. Pages use it to swap light/dark artwork.
            f = f.with_theme_css(full_theme_css(&theme, theme_pack));
            // Wake the loop from the wry IPC thread so suggestions and tab
            // events land in milliseconds, not on the next heartbeat.
            if let Some(proxy) = wake.as_ref() {
                let proxy = proxy.clone();
                f = f.with_waker(move || {
                    if let Err(e) = proxy.send_event(ShellWake::Ipc) {
                        tracing::debug!(error = %e, "shell wake failed (loop gone?)");
                    }
                });
            }
            #[cfg(target_os = "windows")]
            {
                f = f.with_dialogs(dialog_tx);
                dialog_sink = f.dialog_sink();
            }
            Rc::new(f)
        };
        let mut app = AppContext::new_with_factory(theme, factory);
        // Restore persisted engines: customs first (ids are deterministic),
        // then the current id (which may itself be custom).
        if let Some(customs) = settings_json
            .get("engines_custom")
            .and_then(|x| x.as_array())
        {
            for c in customs {
                let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let tpl = c.get("template").and_then(|x| x.as_str()).unwrap_or("");
                let _ = app.omnibox.registry.add_custom(name, tpl);
            }
        }
        // Restore the persisted search engine (unknown ids fall back to
        // the default inside set_current).
        if let Some(id) = settings_json.get("omnibox_engine").and_then(|x| x.as_str()) {
            let _ = app.omnibox.registry.set_current(id);
        }
        // Private-window default: absent key (or unknown id) means follow
        // the regular default. Never a dangling pointer.
        if let Some(id) = settings_json
            .get("omnibox_engine_private")
            .and_then(|x| x.as_str())
        {
            if app.omnibox.registry.set_private(id).is_err() {
                clear_setting("omnibox_engine_private");
            }
        }
        // Auto-sleep policy into the manager (timeout + exception hosts).
        // Zero minutes disables auto-discard; the manager keeps the value
        // for display while the tick skips the sweep.
        app.tabs
            .set_idle_timeout(Duration::from_secs(sleep_timeout_min * 60));
        app.tabs.set_sleep_exceptions(sleep_exceptions.clone());
        let (cert_tx, cert_rx) = std::sync::mpsc::channel();
        let cert_pending: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let cert_last: std::collections::HashMap<Uuid, (String, String)> =
            std::collections::HashMap::new();
        // Operator TLS pins for Ferrite's own fetches (filter mirrors,
        // threat intel, breach lookups): env wins, settings key fallback.
        // Empty/absent means standard root verification. No behavior
        // change. Malformed entries are skipped with a count, never applied.
        let pin_src = std::env::var("FERRITE_TLS_PINS").ok().or_else(|| {
            settings_json
                .get("tls_pins")
                .and_then(|x| x.as_str())
                .map(str::to_owned)
        });
        if let Some(src) = pin_src {
            let (pins, skipped) = ferrite_proxy::parse_pin_list(&src);
            if !pins.is_empty() || skipped > 0 {
                ferrite_proxy::set_tls_pins(pins);
            }
        }
        // Expired cert exceptions are hygiene (enforcement is on read),
        // swept once at boot rather than on any hot path.
        if let Some(v) = vault.as_ref()
            && let Err(e) = v.prune_expired_cert_exceptions(ferrite_tabs::now_millis())
        {
            debug!(error = %e, "cert exception prune failed");
        }
        let (filedlg_tx, filedlg_rx) = std::sync::mpsc::channel();
        let (notice_tx, notice_rx) = std::sync::mpsc::channel();
        let save_last_dir = settings_json
            .get("save_last_dir")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);
        // Remote suggestions: same thread-bridge as cert/viewsource.
        // (generation, input snapshot, phrases).
        let (sug_tx, sug_rx) = std::sync::mpsc::channel();
        let (vs_tx, vs_rx) = std::sync::mpsc::channel();
        let (favicon_tx, favicon_rx) = std::sync::mpsc::channel();
        let (dict_tx, dict_rx) = std::sync::mpsc::channel();
        let system_accent = crate::favicon::system_accent_hex();
        if let Some(a) = system_accent.as_deref() {
            tracing::info!(accent = %a, "system accent follow active");
        }
        // Self-update worker events (check/download run off the loop).
        let (update_tx, update_rx) = std::sync::mpsc::channel();
        // Remote PDF fetch completions (worker threads, drained on tick).
        let (pdf_fetch_tx, pdf_fetch_rx) = std::sync::mpsc::channel();
        let vs_pending: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let mut app = Self {
            config,
            session: boot_session,
            proxy,
            private_proxy,
            proxy_down,
            proxy_requests: 0,
            proxy_dropped: 0,
            private_proxy_requests: 0,
            private_proxy_dropped: 0,
            proxy_downgraded: 0,
            pending_history: std::collections::VecDeque::new(),
            history_spill_dropped: 0,
            translate_unavailable: std::collections::HashSet::new(),
            drm_pending: std::collections::HashMap::new(),
            privacy: privacy_engine,
            vault,
            archive_store: archive.clone(),
            boot_block,
            profile_pick_pending: false,
            boot_t0,
            boot_stores_ms: Some(boot_stores_ms),
            guest_mode: false,
            profile_id,
            guest_profile: incognito_profile,
            dl_dir_slot,
            guest_dl_warned: false,
            quit_requested: false,
            relaunch_requested: false,
            theme_pack,
            home_url,
            bar_mode,
            bar_seen,
            tabs_vertical,
            panel_open,
            panel_section,
            panel_w,
            find_open: false,
            find_query: String::new(),
            find_case: false,
            find_total: 0,
            find_index: 0,
            find_tab: None,
            last_find: String::new(),
            ctx_menu: None,
            ctx_overlay: None,
            scan_cache: std::collections::HashMap::new(),
            fill_options: Vec::new(),
            af_open: false,
            af_items: Vec::new(),
            af_filter: String::new(),
            af_hl: None,
            cred_fills: Vec::new(),
            form_fills: Vec::new(),
            gen_length: 20,
            totp_fills: Vec::new(),
            af_gen: None,
            af_footer: None,
            reader_cache: std::collections::VecDeque::new(),
            reader_pending: std::collections::HashMap::new(),
            reader_size,
            reader_serif,
            reader_wide,
            reader_theme,
            autofill_address,
            autofill_payment,
            autofill_form,
            translate_enabled,
            translate_target,
            drm_reported: std::collections::VecDeque::new(),
            print_jobs: ferrite_tabs::print::PrintGuard::new(),
            pdf_sessions: crate::pdf::PdfSessions::new(),
            pdf_fetch_tx,
            pdf_fetch_rx,
            pdf_intercept_rx,
            update_phase: crate::update::UpdatePhase::default(),
            update_tx,
            update_rx,
            last_update_check: Instant::now()
                .checked_sub(
                    crate::update::UPDATE_CHECK_INTERVAL - crate::update::FIRST_UPDATE_CHECK_DELAY,
                )
                .unwrap_or_else(Instant::now),
            manual_update_check: false,
            // Post-update banner, one-shot: consumed (file deleted) here
            // so a crash before bootstrap cannot re-fire it next boot.
            updated_from: crate::update::take_updated_marker().map(|(from, _)| from),
            crash_offer: false,
            crash_tabs: 0,
            restore_always,
            startup_mode,
            startup_urls,
            history_retention_days,
            // Epoch-past so the first backstop (~30s after boot) sweeps
            // once: hygiene must not block boot, but must not wait a day
            // after every fresh launch either.
            last_retention_sweep: Instant::now()
                .checked_sub(Duration::from_secs(86_400))
                .unwrap_or_else(Instant::now),
            profile_picker,
            sleep_timeout_min,
            sleep_exceptions,
            restore_pending: false,
            onboarding_completed,
            last_autosave: Instant::now(),
            scroll_dirty: false,
            // Epoch in the past so the first tick samples immediately:
            // the discard table should sense pressure from boot, not
            // after one quiet interval.
            mem_sample_at: Instant::now()
                .checked_sub(MEM_SAMPLE_INTERVAL)
                .unwrap_or_else(Instant::now),
            last_blocked_total: 0,
            last_threat_push: Instant::now(),
            cert_tx,
            cert_rx,
            cert_pending,
            cert_last,
            filedlg_tx,
            filedlg_rx,
            #[cfg(target_os = "windows")]
            dialog_rx,
            dialogs: ferrite_tabs::DialogStore::new(),
            dlg_overlay: None,
            dlg_shown: None,
            #[cfg(target_os = "windows")]
            dialog_sink,
            notice_tx,
            notice_rx,
            dlask_rx,
            ask_downloads: std::sync::Arc::clone(&ask_downloads),
            save_last_dir,
            pending_export: None,
            pending_blob: None,
            pending_backup_path: None,
            blob_asm: std::collections::HashMap::new(),
            blob_seq: 0,
            toasts: crate::toast::ToastQueue::new(),
            pending_undo: None,
            pending_save: None,
            pw_conflicts: Vec::new(),
            save_prompted: std::collections::HashSet::new(),
            vs_tx,
            vs_rx,
            vs_pending,
            sug_tx,
            sug_rx,
            sug_gen: 0,
            remote_sugs: (0, String::new(), Vec::new()),
            remote_enabled,
            favicon_cache: std::collections::HashMap::new(),
            favicon_order: std::collections::VecDeque::new(),
            favicon_tx,
            favicon_rx,
            favicon_pending: std::collections::HashSet::new(),
            dict_tx,
            dict_rx,
            dict_boot_pending: true,
            system_accent,
            pip_auto,
            pending_share: None,
            palette_recent: std::collections::VecDeque::new(),
            palette_sig: String::new(),
            viewsource_wrap,
            suggestions_enabled,
            permissions,
            sync_trust,
            sync_session: None,
            sync_settings,
            sync_dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sync_stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pending_pair: None,
            sync_streak: std::cell::Cell::new((0, 0)),
            zoom_cache: std::collections::HashMap::new(),
            zoom_applied: std::collections::HashMap::new(),
            chrome: None,
            chrome_id: Uuid::new_v4(),
            chrome_sig: String::new(),
            overlay: None,
            overlay_open: false,
            modifiers: winit::keyboard::ModifiersState::default(),
            window: None,
            app,
            fullscreen: FullscreenMode::Windowed,
            status_visible,
            pending_error: None,
            ipc_bridge,
            forward_queue,
            downloads,
            download_log: std::collections::VecDeque::new(),
            last_dl_done: None,
            hover: None,
            last_vis: (0, 0),
            closed_tabs: Vec::new(),
            history_queries: std::collections::HashMap::new(),
            bm_views: std::collections::HashMap::new(),
            dl_queries: std::collections::HashMap::new(),
            last_dl_push: Instant::now(),
            sys: sysinfo::System::new(),
            last_tm_push: Instant::now(),
            favicon_sent: std::collections::HashMap::new(),
            engine_profile_bytes: 0,
            // Epoch-past so the first open task manager measures at once.
            engine_profile_at: Instant::now()
                .checked_sub(Duration::from_secs(61))
                .unwrap_or_else(Instant::now),
        };
        app.enter_incognito_boot();
        app
    }

    /// `--incognito` boot tail: ephemeral from the first paint (memory
    /// vault, guest profile, global write refusal). On the impossible
    /// memory-vault failure, fall back to the decision screen with a
    /// reason instead of running storeless and silent.
    fn enter_incognito_boot(&mut self) {
        if !self.config.incognito {
            return;
        }
        if self.enter_ephemeral() {
            return;
        }
        error!("incognito boot: memory vault failed");
        self.boot_block = Some(BootBlock {
            reasons: vec!["Incognito couldn't start: memory allocation failed.".to_owned()],
            tab: None,
        });
    }

    // ── Geometry (replaces egui rect measurement) ──

    fn window_logical(&self) -> Option<(f64, f64)> {
        let window = self.window.as_ref()?;
        let scale = window.scale_factor();
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let size = window.inner_size();
        Some((size.width as f64 / scale, size.height as f64 / scale))
    }

    /// Whether the bookmarks bar shows right now. `ntp` mode follows the
    /// active tab. Everything else is static.
    fn bar_visible(&self) -> bool {
        match self.bar_mode.as_str() {
            "always" => true,
            "ntp" => match self.app.tabs.active_tab() {
                Some(t) => t.url == "ferrite://newtab" || t.url.is_empty(),
                None => true,
            },
            _ => false,
        }
    }

    /// Content slot: full window minus the chrome bars (plus the bookmarks
    /// bar and crash infobar when visible) on top, minus the open side
    /// panel on the right. Vertical tabs dock the strip left at
    /// `VTAB_W_PX`, so the slot shifts right and the top shrinks to the
    /// omnibar row (`CHROME_TOP_VTAB_PX`).
    fn content_rect(&self) -> Option<LogicalRect> {
        let (w, h) = self.window_logical()?;
        // Parenthesized: `else EXPR + ...` would bind the additions into
        // the else branch and drop them in vertical mode.
        let chrome_top = if self.tabs_vertical {
            CHROME_TOP_VTAB_PX
        } else {
            CHROME_TOP_PX
        };
        let top = chrome_top
            + if self.bar_visible() { BAR_H_PX } else { 0.0 }
            + if self.crash_offer { INFOBAR_H_PX } else { 0.0 };
        let left = if self.tabs_vertical { VTAB_W_PX } else { 0.0 };
        let pw = if self.panel_open {
            self.panel_w as f64
        } else {
            0.0
        };
        if w <= left + pw || h <= top {
            return None;
        }
        Some(LogicalRect::new(left, top, w - left - pw, h - top))
    }

    fn content_wry(&self) -> Option<wry::Rect> {
        let window = self.window.as_ref()?;
        let rect = self.content_rect()?;
        ferrite_tabs::logical_to_wry_rect(rect, window.scale_factor())
    }

    fn full_wry(&self) -> Option<wry::Rect> {
        let window = self.window.as_ref()?;
        let (w, h) = self.window_logical()?;
        ferrite_tabs::logical_to_wry_rect(LogicalRect::new(0.0, 0.0, w, h), window.scale_factor())
    }

    /// Reposition the active content webview in the slot. Cheap and
    /// idempotent. Called on resize, tab switch, and chrome push.
    fn layout_tabs(&self) {
        if let Some(rect) = self.content_wry()
            && let Err(e) = self.app.tabs.update_active_bounds(rect)
        {
            debug!(error = %e, "content bounds update failed");
        }
        // An open dialog overlay tracks the content slot across resizes
        // and panel toggles (geometry only, no state touched).
        if self.dlg_shown.is_some()
            && let (Some(ov), Some(bounds)) = (self.dlg_overlay.as_ref(), self.dlg_overlay_rect())
            && let Err(e) = ov.set_bounds(bounds)
        {
            debug!(error = %e, "dialog reposition failed");
        }
    }

    // ── Chrome state push ──

    /// Daily storage hygiene: history retention prune + free-page
    /// reclaim on both encrypted stores. Silent (debug log only):
    /// nagging toasts for background hygiene would train users to
    /// ignore toasts. Skipped without a vault (nothing owned to prune).
    fn retention_sweep(&mut self) {
        if self.vault.is_none() {
            return;
        }
        if self.history_retention_days > 0 {
            // u64 days → i64 millis: validated ≤3650 at every write, so
            // the cast cannot overflow (30M days would be needed).
            let days = self.history_retention_days.min(3650) as i64;
            let cutoff = now_millis().saturating_sub(days.saturating_mul(86_400_000));
            if let Some(v) = self.vault.as_ref() {
                match v.prune_history_older_than(cutoff) {
                    Ok(0) => {}
                    Ok(n) => debug!(n, "history retention pruned"),
                    Err(e) => debug!(error = %e, "history retention failed"),
                }
            }
        }
        if let Some(v) = self.vault.as_ref() {
            match v.reclaim_space() {
                Ok(0) => {}
                Ok(pages) => debug!(pages, "vault pages reclaimed"),
                Err(e) => debug!(error = %e, "vault reclaim failed"),
            }
        }
        if let Some(a) = self.archive_store.as_ref() {
            match a.reclaim_space() {
                Ok(0) => {}
                Ok(pages) => debug!(pages, "archive pages reclaimed"),
                Err(e) => debug!(error = %e, "archive reclaim failed"),
            }
        }
        self.last_retention_sweep = Instant::now();
    }

    /// Record one closed tab for reopen. Private tabs are never recorded:
    /// their URLs must not outlive the tab even in memory. Caps at
    /// CLOSED_CAP with oldest-first eviction.
    fn remember_closed(
        &mut self,
        url: String,
        title: String,
        history: NavigationHistory,
        scroll: (f32, f32),
        pinned: bool,
        private: bool,
    ) {
        if private {
            return;
        }
        self.closed_tabs.push(ClosedTab {
            url,
            title,
            history,
            scroll,
            pinned,
        });
        while self.closed_tabs.len() > CLOSED_CAP {
            self.closed_tabs.remove(0);
        }
    }

    /// History-less record for URL+title sources. Never private by
    /// construction: workspace drops filter private tabs, crash-session
    /// docs exclude them.
    fn remember_closed_bare(&mut self, url: String, title: String) {
        let bare = ClosedTab::bare(url, title);
        self.remember_closed(
            bare.url,
            bare.title,
            bare.history,
            bare.scroll,
            bare.pinned,
            false,
        );
    }

    /// Close tabs by id with full bookkeeping: closed-stack entry (undo),
    /// dialog cleanup, find reset. Pinned tabs refuse per-tab (manager
    /// rule) and count as kept; unknown ids are skipped. Returns
    /// (closed, kept) for result toasts. Shared by bulk close, group
    /// close, and any future multi-close gesture.
    fn close_tab_ids(&mut self, ids: &[Uuid]) -> (usize, usize) {
        let (mut closed, mut kept) = (0, 0);
        for cid in ids {
            if let Some(t) = self.app.tabs.get_tab(*cid).map(|t| {
                (
                    t.url.clone(),
                    t.title.clone(),
                    t.history.clone(),
                    t.scroll_position,
                    t.is_pinned,
                    t.is_private,
                )
            }) {
                let (url, title, history, scroll, pinned, private) = t;
                self.remember_closed(url, title, history, scroll, pinned, private);
                match self.app.tabs.close_tab(*cid) {
                    Ok(()) => closed += 1,
                    Err(_) => kept += 1,
                }
                self.clear_tab_dialogs(*cid);
            }
            if self.find_tab == Some(*cid) {
                self.find_reset();
            }
            // A closed tab takes its PDF viewer sessions with it
            // (renderer children killed, bytes zeroized).
            self.pdf_sessions.close_tab_sessions(*cid);
        }
        self.layout_tabs();
        self.push_chrome(true);
        (closed, kept)
    }

    /// Tabs owning in-flight downloads, derived from the event log:
    /// newest event per download id decides terminality; any non-terminal
    /// event names the tab (progress rows carry no tab half; the Started
    /// row does). Pure for tests. The drain applies it when events
    /// arrive; the autosave backstop reconciles (a dropped terminal
    /// event must never pin a tab awake forever).
    fn downloading_tabs(
        log: &std::collections::VecDeque<ferrite_tabs::DownloadEvent>,
    ) -> std::collections::HashSet<Uuid> {
        use ferrite_tabs::DownloadStateKind as Ds;
        use std::collections::HashMap;
        // Newest terminal per id kills it; newest non-terminal keeps it.
        let mut terminal: HashMap<&str, bool> = HashMap::new();
        let mut tabs: HashMap<&str, Uuid> = HashMap::new();
        for e in log.iter().rev() {
            let done = matches!(e.state, Ds::Completed | Ds::Failed | Ds::Cancelled);
            terminal.entry(e.id.as_str()).or_insert(done);
            if !done {
                if let Some(tid) = e.tab_id.as_deref().and_then(|s| s.parse::<Uuid>().ok()) {
                    tabs.entry(e.id.as_str()).or_insert(tid);
                }
            }
        }
        terminal
            .into_iter()
            .filter(|(_, done)| !done)
            .filter_map(|(id, _)| tabs.get(id).copied())
            .collect()
    }

    /// Apply the derived download set onto tab flags. Changes only, no
    /// dirty flag: protection state is runtime-only, never persisted.
    fn sync_download_flags(&mut self) {
        let want = Self::downloading_tabs(&self.download_log);
        for t in self.app.tabs.iter_mut() {
            let w = want.contains(&t.id);
            if t.has_active_download != w {
                t.has_active_download = w;
            }
        }
    }

    /// Settle after a workspace switch/create/delete: the newly live tree
    /// may hold a discarded active tab (fresh boot) or live-but-hidden
    /// webviews (in-memory park). Discarded actives restore through the
    /// window path; live ones are already visible from the unpark. Either
    /// way the slot relayouts and chrome repushes.
    fn settle_workspace_switch(&mut self) {
        let need_restore = self.app.tabs.active_tab().is_some_and(|t| t.is_discarded());
        if need_restore && let Some(idx) = self.app.tabs.active_index() {
            self.switch_to_index(idx);
        }
        self.layout_tabs();
        self.push_chrome(true);
    }

    /// One profile row for the switcher. Initial is the first
    /// alphanumeric, uppercased ("?" when none: display-only, never an id).
    fn profile_row(id: &str, current: bool) -> ProfileRow {
        let initial = id
            .chars()
            .find(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase().to_string())
            .unwrap_or_else(|| "?".to_owned());
        ProfileRow {
            id: id.to_owned(),
            name: id.to_owned(),
            initial,
            current,
        }
    }

    /// All profiles for switchers (boot picker + avatar popover):
    /// current first, Default always listed, rest alphabetical.
    fn profile_rows(&self) -> Vec<ProfileRow> {
        let mut rows = vec![Self::profile_row(
            crate::profiles::DEFAULT_ID,
            self.profile_id
                .eq_ignore_ascii_case(crate::profiles::DEFAULT_ID),
        )];
        let mut rest: Vec<ProfileRow> = crate::profiles::list()
            .into_iter()
            .filter(|id| !id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID))
            .map(|id| Self::profile_row(&id, id == self.profile_id))
            .collect();
        rows.append(&mut rest);
        if let Some(pos) = rows.iter().position(|r| r.current) {
            let cur = rows.remove(pos);
            rows.insert(0, cur);
        }
        rows
    }

    /// Launch a sibling process under a profile (side-by-side windows,
    /// Chrome model: switching never moves this window's tabs).
    /// Detached: no wait, no pipes. Failures toast; a silent avatar
    /// button reads as broken.
    fn spawn_profile(&mut self, id: &str) {
        match std::env::current_exe() {
            Ok(exe) => {
                match std::process::Command::new(&exe)
                    .arg("--profile")
                    .arg(id)
                    .spawn()
                {
                    Ok(_) => self.notify(format!("Opening profile {id}...")),
                    Err(e) => {
                        debug!(error = %e, "profile spawn failed");
                        self.notify("Couldn't open that profile.".to_owned());
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, "profile spawn failed: no current exe");
                self.notify("Couldn't open that profile.".to_owned());
            }
        }
    }

    /// Sleep one tab now by id: renderer freed, document lineage kept,
    /// click restores. Explicit gesture: bypasses sleep exceptions (so a
    /// listed host still sleeps when asked), but never sleeps the ACTIVE
    /// tab (unloading what you stare at is a bug, not a feature).
    fn sleep_tab_id(&mut self, id: Uuid) -> bool {
        if self.app.tabs.active_id() == Some(id) {
            return false;
        }
        let idx = self.app.tabs.iter().position(|t| t.id == id);
        match idx {
            Some(i) => {
                // Protected tabs (pinned, audio, downloading) make
                // discard_tab a silent no-op by design: verify the tab
                // actually slept so callers never report a lie.
                if let Err(e) = self.app.tabs.discard_tab(i) {
                    debug!(error = %e, "sleep tab refused");
                    return false;
                }
                let slept = self
                    .app
                    .tabs
                    .iter()
                    .nth(i)
                    .is_some_and(|t| t.is_discarded());
                if !slept {
                    debug!("sleep tab no-op (protected)");
                    return false;
                }
                self.clear_tab_dialogs(id);
                self.layout_tabs();
                self.push_chrome(true);
                true
            }
            None => false,
        }
    }

    /// Sleep the active tab for the palette/menu path. Sleeping what you
    /// stare at would blank the window, so the command first moves to a
    /// neighbor, then sleeps the previous tab. Single-tab windows refuse
    /// with a toast (nothing to move to), never a blank window.
    fn sleep_active_tab(&mut self) {
        // Resolve the neighbor index under one immutable borrow, then
        // release it before switching (borrowck discipline: the switch
        // mutates tab visibility).
        let job: Option<(Uuid, usize)> = self.app.tabs.active_id().and_then(|id| {
            let tabs = &self.app.tabs;
            // A downloading active tab refuses before any switch: moving
            // away just to fail would strand the user on a neighbor.
            if tabs.get_tab(id).is_some_and(|t| t.has_active_download) {
                return None;
            }
            let i = tabs.iter().position(|t| t.id == id)?;
            let n = tabs.len();
            if n < 2 {
                return None;
            }
            let j = if i + 1 < n { i + 1 } else { i - 1 };
            tabs.iter().nth(j).map(|_| (id, j))
        });
        match job {
            // Move to the neighbor first so the sleep never blanks the
            // window, then sleep the previous tab by id.
            Some((id, nidx)) => {
                self.switch_to_index(nidx);
                if self.sleep_tab_id(id) {
                    self.notify("Tab slept.".to_owned());
                } else {
                    // Protected previous tab (pinned, audio): the switch
                    // already happened, so say what did not.
                    self.notify("Couldn't sleep that tab.".to_owned());
                }
            }
            None => {
                let downloading = self
                    .app
                    .tabs
                    .active_tab()
                    .is_some_and(|t| t.has_active_download);
                if downloading {
                    self.notify("That tab is downloading. Wait or cancel first.".to_owned());
                } else {
                    self.notify("Nothing else open to switch to.".to_owned());
                }
            }
        }
    }

    /// Wanted zoom factor for an origin: memory cache, else vault, else
    /// 100 percent. Unknown origins never write (the default needs no storage).
    fn zoom_want(&mut self, origin: &str) -> f64 {
        if let Some(f) = self.zoom_cache.get(origin) {
            return *f;
        }
        let f = self
            .vault
            .as_ref()
            .and_then(|v| v.zoom_for(origin))
            .unwrap_or(1.0);
        self.zoom_cache.insert(origin.to_owned(), f);
        f
    }

    /// Engine call only (no persistence). Records the applied pair so the
    /// tick can tell drifted webviews apart from settled ones.
    fn zoom_apply_now(&mut self, tab: Uuid, origin: &str, factor: f64) {
        if let Some(t) = self.app.tabs.get_tab(tab)
            && let Some(h) = t.webview.as_ref()
            && let Err(e) = h.set_zoom(factor)
        {
            debug!(error = %e, "zoom apply failed");
            return;
        }
        if self.app.tabs.get_tab(tab).is_some() {
            self.zoom_applied.insert(tab, (origin.to_owned(), factor));
        }
    }

    /// Persist a user-chosen factor (100% clears the vault row) plus apply
    /// plus re-push so the chip follows immediately. Session rule: ephemeral
    /// processes apply session-only (memory cache, never the vault: there
    /// is no vault to write to, and the in-memory one dies with the window).
    fn zoom_set(&mut self, tab: Uuid, origin: String, factor: f64) {
        self.zoom_cache.insert(origin.clone(), factor);
        if self.session.is_ephemeral() {
            debug!("zoom session-only in ephemeral mode");
        } else if let Some(vault) = self.vault.as_ref()
            && let Err(e) = vault.set_zoom(&origin, factor)
        {
            debug!(error = %e, "zoom persist failed");
        }
        self.zoom_apply_now(tab, &origin, factor);
        self.push_chrome(true);
    }

    /// Current factor for the active tab (applied pair, else origin want,
    /// else 100%).
    fn zoom_current(&mut self) -> Option<(Uuid, String, f64)> {
        let id = self.app.tabs.active_id()?;
        let url = self
            .app
            .tabs
            .get_tab(id)
            .filter(|t| t.webview.is_some())
            .map(|t| t.url.clone())?;
        let origin = zoom_key_for(&url)?;
        let factor = self
            .zoom_applied
            .get(&id)
            .filter(|(o, _)| *o == origin)
            .map(|(_, f)| *f)
            .unwrap_or_else(|| self.zoom_want(&origin));
        Some((id, origin, factor))
    }

    /// Step the active tab through the preset table (or reset to 100%).
    /// No-ops on webview-less tabs and URLs without a zoom key.
    fn zoom_step_active(&mut self, dir: i64) {
        if let Some((id, origin, factor)) = self.zoom_current() {
            let pct = (factor * 100.0).round() as u32;
            let next = zoom_step(pct, dir);
            self.zoom_set(id, origin, next as f64 / 100.0);
        }
    }

    fn zoom_reset_active(&mut self) {
        if let Some((id, origin, _)) = self.zoom_current() {
            self.zoom_set(id, origin, 1.0);
        }
    }

    /// Tick-time restore: the active webview follows its origin saved
    /// factor. Covers navigation, tab switch, and session restore with no
    /// per-event plumbing. Background tabs keep their own engine zoom.
    fn zoom_sync_tick(&mut self) {
        let live: std::collections::HashSet<Uuid> = self.app.tabs.iter().map(|t| t.id).collect();
        self.zoom_applied.retain(|k, _| live.contains(k));
        let active = self.app.tabs.active_id().and_then(|id| {
            self.app
                .tabs
                .get_tab(id)
                .filter(|t| t.webview.is_some())
                .map(|t| (id, t.url.clone()))
        });
        if let Some((id, url)) = active
            && let Some(origin) = zoom_key_for(&url)
        {
            let want = self.zoom_want(&origin);
            let settled = self
                .zoom_applied
                .get(&id)
                .map(|(o, f)| o == &origin && *f == want)
                .unwrap_or(false);
            if !settled {
                self.zoom_apply_now(id, &origin, want);
            }
        }
    }

    fn chrome_state(&self) -> ChromeState {
        let active_id = self.app.tabs.active_id();
        let tabs: Vec<TabRow> = self
            .app
            .tabs
            .iter()
            .enumerate()
            .map(|(idx, t)| TabRow {
                id: t.id.to_string(),
                // Private tabs carry a visible marker: an incognito tab
                // with no indicator is a privacy footgun. Display-only.
                // Stored titles are untouched.
                title: if t.is_private {
                    format!("Private; {}", t.title)
                } else {
                    t.title.clone()
                },
                url: t.url.clone(),
                loading: t.is_loading,
                discarded: t.is_discarded(),
                crashed: t.is_crashed,
                active: Some(t.id) == active_id,
                pinned: t.is_pinned,
                muted: t.is_muted,
                playing: t.is_playing_audio,
                pip: t.is_pip_active,
                pip_available: t.pip_eligible,
                // Background-tab dialog badge: the overlay covers the
                // active tab's dialog, so only inactive tabs badge.
                dialog_pending: Some(t.id) != active_id && self.dialogs.has_pending(t.id),
                can_back: t.can_go_back(),
                can_forward: t.can_go_forward(),
                // Real manager index (see TabRow.idx): collapse hides rows
                // client-side, so visible positions are not indices.
                idx,
                group: t.group_id.map(|g| g.to_string()),
                private: t.is_private,
                favicon: t.favicon_data.clone(),
            })
            .collect();
        let groups: Vec<GroupRow> = self
            .app
            .tabs
            .groups()
            .iter()
            .map(|g| {
                let hex = ferrite_tabs::manager::GROUP_COLORS
                    .get(g.color as usize)
                    .map(|c| c.1.to_owned())
                    .unwrap_or_else(|| "#9AA0A6".to_owned());
                GroupRow {
                    id: g.id.to_string(),
                    name: g.name.clone(),
                    color: g.color,
                    color_hex: hex,
                    collapsed: g.collapsed,
                    count: self.app.tabs.tabs_in_group(g.id).len(),
                }
            })
            .collect();
        let (raw_url, sec, pinned_view) = match self.app.tabs.active_tab() {
            Some(t) => (
                t.url.clone(),
                {
                    if t.url.starts_with("ferrite://") {
                        "shield"
                    } else if t.url.starts_with("https://") {
                        "lock"
                    } else {
                        "info"
                    }
                }
                .to_owned(),
                t.is_pinned,
            ),
            None => (String::new(), "shield".to_owned(), false),
        };
        // Display parts come pre-split from Rust (IDN-safe). Newtab and
        // empty show the placeholder instead of a URL.
        let disp = if raw_url == "ferrite://newtab" || raw_url.is_empty() {
            ferrite_chrome::chrome::DispParts::default()
        } else if let Some(query) = raw_url.strip_prefix("ferrite://viewsource?") {
            // Deliberate exception to protocol-dimming: on a view-source
            // page the scheme prefix is the load-bearing information, so it
            // renders plainly as `view-source:` + target, not dimmed chrome.
            let target = ferrite_tabs::protocol::decode_query_param(query, "u").unwrap_or_default();
            let host = host_slice(&target);
            ferrite_chrome::chrome::DispParts {
                sch: "view-source".to_owned(),
                sub: String::new(),
                dom: host.to_owned(),
                path: target
                    .split_once(host)
                    .map(|(_, rest)| rest.to_owned())
                    .unwrap_or_default(),
            }
        } else {
            ferrite_omnibox::display_parts(&raw_url)
                .map(|(sch, sub, dom, path)| ferrite_chrome::chrome::DispParts {
                    sch,
                    sub,
                    dom,
                    path,
                })
                .unwrap_or_default()
        };
        let input = self.app.omnibox.input.clone();
        // Source scope (`@tabs`, `@bookmarks`, `@history`): the dropdown
        // filters to one provider at a wider budget; Enter still commits
        // by row kind (tab rows switch, the rest navigate/search), so no
        // commit-path change is needed. No answer row here: math is not
        // one of the scoped sources.
        let (scope, scope_q) = omni_scope(&input);
        let needle = scope_q.to_lowercase();
        // Open-tab matches first (switch, do not duplicate), then history.
        // Empty input shows session frecency instead of nothing. A math
        // answer is built first and re-pinned after the list below:
        // assigning `suggestions` wholesale used to drop it on the floor
        // (the parser and its tests passed while the UI never showed one).
        // Scoped input skips the answer: math is not a scoped source.
        let answer: Option<SugRow> = if input.trim().is_empty() || scope.is_some() {
            None
        } else {
            ferrite_omnibox::instant_math(&input).map(|answer| SugRow {
                url: input.trim().to_owned(),
                title: format!("= {answer}"),
                tab: None,
                kind: "answer".to_owned(),
                provider: None,
            })
        };
        let mut suggestions: Vec<SugRow>;
        suggestions = if scope.is_some() && self.suggestions_enabled {
            // One source at a wider budget (8 rows). Empty scope query
            // lists (open tabs, frecency, recent bookmarks); the global
            // toggle still gates everything.
            match scope {
                Some("tabs") => self
                    .app
                    .tabs
                    .iter()
                    .filter(|t| {
                        needle.is_empty()
                            || t.title.to_lowercase().contains(&needle)
                            || t.url.to_lowercase().contains(&needle)
                    })
                    .take(8)
                    .map(|t| SugRow {
                        url: t.url.clone(),
                        title: t.title.clone(),
                        tab: Some(t.id.to_string()),
                        kind: "tab".to_owned(),
                        provider: None,
                    })
                    .collect(),
                Some("history") => {
                    let here = self
                        .app
                        .tabs
                        .active_tab()
                        .map(|t| t.url.clone())
                        .unwrap_or_default();
                    let rows = if needle.trim().is_empty() {
                        self.app.autocomplete.top(9)
                    } else {
                        self.app.autocomplete.query(&scope_q, 9)
                    };
                    rows.iter()
                        .filter(|s| s.url != here)
                        .take(8)
                        .map(|s| SugRow {
                            url: s.url.clone(),
                            title: s.title.clone(),
                            tab: None,
                            kind: "history".to_owned(),
                            provider: None,
                        })
                        .collect()
                }
                _ => match self.vault.as_ref() {
                    Some(vault) => vault
                        .search_bookmarks(&scope_q, 8)
                        .iter()
                        .filter(|b| !b.url.is_empty())
                        .map(|b| SugRow {
                            url: b.url.clone(),
                            title: if b.title.trim().is_empty() {
                                b.url.clone()
                            } else {
                                b.title.clone()
                            },
                            tab: None,
                            kind: "bookmark".to_owned(),
                            provider: None,
                        })
                        .collect(),
                    None => Vec::new(),
                },
            }
        } else if input.trim().is_empty() {
            if self.suggestions_enabled {
                // The current page is never its own suggestion (echoing the
                // URL you are already on teaches nothing and wastes a row).
                let here = self
                    .app
                    .tabs
                    .active_tab()
                    .map(|t| t.url.clone())
                    .unwrap_or_default();
                self.app
                    .autocomplete
                    .top(6)
                    .iter()
                    .filter(|s| s.url != here)
                    .take(5)
                    .map(|s| SugRow {
                        url: s.url.clone(),
                        title: s.title.clone(),
                        tab: None,
                        kind: "history".to_owned(),
                        provider: None,
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            self.app
                .tabs
                .iter()
                .filter(|t| {
                    t.title.to_lowercase().contains(&needle)
                        || t.url.to_lowercase().contains(&needle)
                })
                .take(3)
                .map(|t| SugRow {
                    url: t.url.clone(),
                    title: t.title.clone(),
                    tab: Some(t.id.to_string()),
                    kind: "tab".to_owned(),
                    provider: None,
                })
                .collect()
        };
        // Suggestions are local-only, but the toggle still kills the
        // history half for users who want a bare bar. Scoped input skips
        // all of this: the branch above already built the single source.
        if scope.is_none() && !input.trim().is_empty() && self.suggestions_enabled {
            let history: Vec<SugRow> = self
                .app
                .autocomplete
                .query(&input, 3)
                .iter()
                .map(|s| SugRow {
                    url: s.url.clone(),
                    title: s.title.clone(),
                    tab: None,
                    kind: "history".to_owned(),
                    provider: None,
                })
                .collect();
            suggestions.extend(dedup_suggestions(&suggestions, history));
            // Bookmarks last (local tree search, fail-soft): deduped
            // against everything above so one URL is never two rows.
            // List budget is 1 answer + 3 tab + 3 history + 2 bookmark
            // + 1 action.
            if let Some(vault) = self.vault.as_ref() {
                let bookmarks: Vec<SugRow> = vault
                    .search_bookmarks(&input, 2)
                    .iter()
                    .filter(|b| !b.url.is_empty())
                    .map(|b| SugRow {
                        url: b.url.clone(),
                        title: if b.title.trim().is_empty() {
                            b.url.clone()
                        } else {
                            b.title.clone()
                        },
                        tab: None,
                        kind: "bookmark".to_owned(),
                        provider: None,
                    })
                    .collect();
                suggestions.extend(dedup_suggestions(&suggestions, bookmarks));
            }
            // Fallthrough row: the typed text as a history search. The URL
            // rides the existing submit path (no chrome changes needed).
            // Kind is `search`, not `history`: the row has no stored entry,
            // so the history × button must not offer to delete it (that
            // delete silently no-ops on a ferrite:// URL).
            let q: String = input.chars().take(100).collect();
            suggestions.push(SugRow {
                url: format!("ferrite://history?q={}", encode_query(&q)),
                title: format!("Search history for “{q}”"),
                tab: None,
                kind: "search".to_owned(),
                provider: None,
            });
            // Remote rows last (subordinate to everything local): only the
            // accepted generation whose snapshot still matches, deduped,
            // then the whole list holds at 10 (remote trimmed first).
            let (_, snapshot, remote) = &self.remote_sugs;
            if snapshot == input.trim() && !remote.is_empty() {
                suggestions.extend(dedup_suggestions(&suggestions, remote.clone()));
            }
            // Tab-to-search affordance first: typed text naming a provider
            // by its template domain offers "Press Tab to search". The row
            // never navigates (empty url); Tab commits it into provider
            // mode, Enter still classifies normally.
            if !input.trim().is_empty()
                && let Some(m) = self.app.omnibox.registry.match_tab_search(&input)
            {
                suggestions.insert(
                    0,
                    SugRow {
                        url: String::new(),
                        title: format!("Press Tab to search {}", m.name),
                        tab: None,
                        kind: "tabsearch".to_owned(),
                        provider: Some(m.id),
                    },
                );
            }
            suggestions.truncate(10);
        }
        // Re-pin the answer above everything (budget: 1 answer + 9 others;
        // remote trims first as before since it sits at the tail).
        if let Some(a) = answer {
            suggestions.insert(0, a);
            suggestions.truncate(10);
        }
        // The toolbar reflects the EFFECTIVE engine: private tabs resolve
        // through the private default when one is set, so showing the
        // regular default there would lie about where Enter searches.
        let private = self.app.tabs.active_tab().is_some_and(|t| t.is_private);
        let current = self.app.omnibox.registry.effective_id(private).to_owned();
        let mut engines: Vec<EngineRow> = self
            .app
            .omnibox
            .registry
            .all()
            .iter()
            .map(|e| EngineRow {
                id: e.id.to_owned(),
                name: e.name.to_owned(),
                current: e.id == current,
            })
            .collect();
        engines.extend(
            self.app
                .omnibox
                .registry
                .custom_engines()
                .iter()
                .map(|e| EngineRow {
                    id: e.id.clone(),
                    name: e.name.clone(),
                    current: e.id == current,
                }),
        );
        let themes: Vec<EngineRow> = ThemePack::ALL
            .iter()
            .map(|p| EngineRow {
                id: p.name().to_owned(),
                name: p.display_name().to_owned(),
                current: *p == self.theme_pack,
            })
            .collect();
        // Permission chip: pending rows for the active tab only (it
        // disappears on switch-away and returns as left). Dead tabs are
        // swept here so closes never leave stale rows.
        let live: std::collections::HashSet<Uuid> = self.app.tabs.iter().map(|t| t.id).collect();
        self.permissions.retain_tabs(&live);
        let perm: Vec<ferrite_chrome::chrome::PermRow> = self
            .app
            .tabs
            .active_id()
            .map(|id| {
                self.permissions
                    .pending_for(id)
                    .iter()
                    .map(|p| ferrite_chrome::chrome::PermRow {
                        kind: p.kind.clone(),
                        label: ferrite_tabs::kind_label(&p.kind).to_owned(),
                        origin: p.origin.clone(),
                        quiet: self
                            .permissions
                            .quiet(&p.origin, ferrite_tabs::now_millis()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        ChromeState {
            tabs,
            groups,
            tabs_vertical: self.tabs_vertical,
            omni_scope: scope.map(|s| s.to_owned()),
            translate_offer: self.app.tabs.active_tab().is_some_and(|t| {
                translate_offer(
                    self.translate_enabled,
                    &t.url,
                    t.page_lang.as_deref(),
                    &self.translate_target,
                ) && !self.translate_unavailable.contains(host_slice(&t.url))
            }),
            workspaces: self
                .app
                .tabs
                .workspace_list()
                .into_iter()
                .map(|(id, name, count)| {
                    let active = self.app.tabs.active_space_id() == Some(id);
                    WsRow {
                        id: id.to_string(),
                        name,
                        count,
                        active,
                    }
                })
                .collect(),
            workspace: self.app.tabs.active_space_name(),
            profiles: self.profile_rows(),
            raw_url,
            disp,
            sec,
            omnibox_text: input,
            suggestions,
            perm,
            zoom_pct: self
                .app
                .tabs
                .active_id()
                .and_then(|id| self.zoom_applied.get(&id))
                .map(|(_, f)| (f * 100.0).round() as u32)
                .unwrap_or(100),
            pip_eligible: self.app.tabs.active_tab().is_some_and(|t| t.pip_eligible),
            pip_active: self.app.tabs.active_tab().is_some_and(|t| t.is_pip_active),
            engines,
            status: {
                // Real verdict data: blocked is ad+tracker+fingerprint+malware+mixed,
                // never the raw request counter. The badge headline stays
                // session-wide; `site_*` below attributes the ACTIVE page so
                // the dropdown can promise "on this site" honestly.
                let (allow, ad, tracker, fingerprint, malware, mixed) =
                    self.privacy.verdict_counts();
                let _ = allow;
                let site_host = self
                    .app
                    .tabs
                    .active_tab()
                    .map(|t| t.url.clone())
                    .and_then(|u| page_host_for_shield(&u));
                let (site_blocked, site_paused) = match site_host.as_deref() {
                    Some(host) => {
                        let (_, ad, tracker, _, malware, mixed) =
                            self.privacy.origin_breakdown(host);
                        (ad + tracker + malware + mixed, self.privacy.is_paused(host))
                    }
                    None => (0, false),
                };
                // Latest in-flight download: a later terminal event for the
                // same id retires it.
                let mut terminal: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                let mut dl_active = false;
                let mut dl_frac = 0.0f32;
                for e in self.download_log.iter().rev() {
                    use ferrite_tabs::DownloadStateKind as S;
                    match e.state {
                        S::Completed | S::Failed | S::Cancelled => {
                            terminal.insert(e.id.as_str());
                        }
                        S::Started | S::Progress | S::Paused | S::Interrupted => {
                            if !terminal.contains(e.id.as_str()) && !dl_active {
                                dl_active = true;
                                dl_frac = match e.bytes_total {
                                    Some(t) if t > 0 => {
                                        (e.bytes_received as f32 / t as f32).clamp(0.0, 1.0)
                                    }
                                    _ => 0.0,
                                };
                            }
                        }
                    }
                    if dl_active {
                        break;
                    }
                }
                let dl_failed = self
                    .download_log
                    .iter()
                    .rev()
                    .find_map(|e| {
                        use ferrite_tabs::DownloadStateKind as S;
                        match e.state {
                            S::Started | S::Progress | S::Paused | S::Interrupted => None,
                            S::Completed | S::Cancelled => Some(false),
                            S::Failed => Some(true),
                        }
                    })
                    .unwrap_or(false);
                StatusRow {
                    tabs: self.app.tabs.len(),
                    proxy_alive: self.proxy.is_some(),
                    blocked: ad + tracker + fingerprint + malware + mixed,
                    blocked_ad: ad,
                    blocked_tracker: tracker,
                    blocked_fingerprinting: fingerprint,
                    blocked_malware: malware,
                    blocked_mixed: mixed,
                    recent_trackers: self.privacy.recent_tracker_hosts(8),
                    fail_open: self.privacy.fail_open_count(),
                    site_blocked,
                    site_paused,
                    downloads: self.download_log.len(),
                    last_download: self
                        .download_log
                        .back()
                        .map(|e| format!("{} {:?}", e.filename, e.state)),
                    last_download_id: self.download_log.back().map(|e| e.id.clone()),
                    show: self.status_visible,
                    dl_active,
                    dl_frac,
                    dl_flash: self
                        .last_dl_done
                        .map(|t| t.elapsed() < Duration::from_secs(4))
                        .unwrap_or(false),
                    dl_failed,
                    console_errors: self
                        .app
                        .tabs
                        .active_tab()
                        .map(|t| t.console_errors)
                        .unwrap_or(0),
                    toast: self.toasts.current_view(),
                }
            },
            accent: match self.app.theme.accent_context {
                // Archive gold is constant across packs. The pack flare
                // covers the standard context.
                AccentContext::Archive => ARCHIVE_GOLD.to_owned(),
                // OS accent follow wins over the pack when resolved (Linux
                // GNOME / FERRITE_ACCENT override). Archive context keeps
                // its constant gold regardless.
                AccentContext::Standard => self
                    .system_accent
                    .clone()
                    .unwrap_or_else(|| self.theme_pack.accent().to_owned()),
            },
            pinned_view,
            starred: match self.app.tabs.active_tab() {
                Some(t) => self
                    .vault
                    .as_ref()
                    .map(|v| v.bookmark_cache.contains_key(&t.url))
                    .unwrap_or(false),
                None => false,
            },
            starred_folder: match self.app.tabs.active_tab() {
                Some(t) => self.vault.as_ref().and_then(|v| v.bookmark_parent(&t.url)),
                None => None,
            },
            themes,
            bar_visible: self.bar_visible(),
            bar_mode: self.bar_mode.clone(),
            panel_open: self.panel_open,
            panel_section: self.panel_section.clone(),
            panel_w: self.panel_w,
            find_open: self.find_open,
            find_query: self.find_query.clone(),
            find_case: self.find_case,
            find_total: self.find_total,
            find_index: self.find_index,
            crash_offer: self.crash_offer,
            crash_tabs: self.crash_tabs,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            bar_items: match self.vault.as_ref() {
                Some(v) => v
                    .list_bookmark_children(None)
                    .into_iter()
                    .take(100)
                    .map(|n| ferrite_chrome::chrome::BarItem {
                        id: n.id,
                        title: n.title,
                        url: n.url,
                        folder: n.is_folder,
                    })
                    .collect(),
                None => Vec::new(),
            },
            folders: match self.vault.as_ref() {
                Some(v) => v
                    .list_folders()
                    .into_iter()
                    .map(|n| EngineRow {
                        id: n.id,
                        name: n.title,
                        current: false,
                    })
                    .collect(),
                None => Vec::new(),
            },
            hover: match &self.hover {
                Some((id, url)) if Some(*id) == self.app.tabs.active_id() => {
                    ferrite_omnibox::display_parts(url)
                        .map(|(sch, sub, dom, path)| ferrite_chrome::chrome::DispParts {
                            sch,
                            sub,
                            dom,
                            path: ferrite_chrome::chrome::mid_truncate(&path, 44, 36),
                        })
                        .unwrap_or_default()
                }
                _ => ferrite_chrome::chrome::DispParts::default(),
            },
            hover_mine: match &self.hover {
                Some((id, _)) => Some(*id) == self.app.tabs.active_id(),
                None => false,
            },
            incognito: self.guest_mode || self.session.is_ephemeral(),
            session_kind: self.session.kind_str().to_owned(),
            private_tab: self.app.tabs.active_tab().is_some_and(|t| t.is_private),
        }
    }

    /// Push chrome state when it changed (or forced, e.g. on page load).
    fn push_chrome(&mut self, force: bool) {
        let mut state = self.chrome_state();
        // Favicons ride only on change: data URLs are KBs each and the
        // client caches them (favCache), so unchanged rows send None.
        // Dead ids prune here (closed tabs must not pin bitmaps).
        for t in state.tabs.iter_mut() {
            match t.favicon.clone() {
                Some(d) if self.favicon_sent.get(&t.id).is_some_and(|s| s == &d) => {
                    t.favicon = None;
                }
                Some(d) => {
                    self.favicon_sent.insert(t.id.clone(), d);
                }
                None => {
                    self.favicon_sent.remove(&t.id);
                }
            }
        }
        let live: std::collections::HashSet<&str> =
            state.tabs.iter().map(|t| t.id.as_str()).collect();
        self.favicon_sent.retain(|id, _| live.contains(id.as_str()));
        let sig = serde_json::to_string(&state).unwrap_or_default();
        if !force && sig == self.chrome_sig {
            return;
        }
        self.chrome_sig = sig;
        // Visibility sweep first: a drifted (visible but inactive) webview
        // would otherwise keep showing a stale page while commands land on
        // the hidden active tab.
        let (shown, hidden) = self.app.tabs.show_only_active();
        if (shown, hidden) != self.last_vis {
            debug!(shown, hidden, "visibility sweep counts changed");
            self.last_vis = (shown, hidden);
        }
        if let Some(chrome) = self.chrome.as_ref()
            && let Err(e) = chrome.evaluate_script(&push_script(&state))
        {
            debug!(error = %e, "chrome push failed");
        }
        self.layout_tabs();
    }

    fn eval_chrome(&self, js: &str) {
        if let Some(chrome) = self.chrome.as_ref()
            && let Err(e) = chrome.evaluate_script(js)
        {
            debug!(error = %e, js, "chrome eval failed");
        }
    }

    /// Push a JSON row list into a list page render hook (`textContent`
    /// rendering keeps vault data inert. `</` stays escaped for the script
    /// string context).
    fn push_list(&self, tab_id: Uuid, hook: &str, items: &[serde_json::Value]) {
        let tab = self.app.tabs.get_tab(tab_id);
        self.push_list_to(tab.and_then(|t| t.webview.as_ref()), hook, items);
    }

    fn push_list_to(
        &self,
        handle: Option<&Rc<dyn WebViewHandle>>,
        hook: &str,
        items: &[serde_json::Value],
    ) {
        let json = serde_json::to_string(items)
            .unwrap_or_else(|_| "[]".to_owned())
            .replace("</", "<\\/");
        if let Some(h) = handle {
            let js = format!("{hook}({json})");
            if let Err(e) = h.evaluate_script(&js) {
                debug!(error = %e, hook, "list push failed");
            }
        }
    }

    /// History rows for a filter string (page list plus re-pushes share it).
    fn history_json(&self, q: &str) -> Vec<serde_json::Value> {
        let q = q.to_lowercase();
        match self.vault.as_ref() {
            Some(vault) => vault
                .query_history(&q, 50)
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "title": e.title,
                        "url": e.url,
                        "visits": e.visit_count,
                        "last": e.last_visit,
                    })
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Normalize a cookie-viewer origin input to a lowercase host. Accepts
    /// a bare host or a full URL. Anything unparseable yields "" (the page
    /// shows "no such origin", never a guess).
    fn cookie_origin_host(input: &str) -> String {
        let t = input.trim();
        if t.is_empty() || t.len() > 253 {
            return String::new();
        }
        let with_scheme = if t.contains("://") {
            t.to_owned()
        } else {
            format!("https://{t}")
        };
        url::Url::parse(&with_scheme)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .unwrap_or_default()
    }

    /// Whether a jar cookie belongs to `host` for per-site display or reset.
    /// Exact host or subdomain of the cookie domain (leading dot ignored).
    /// The authoritative match already happened when the engine stored it.
    fn cookie_domain_matches(host: &str, domain: Option<&str>) -> bool {
        let d = domain
            .unwrap_or("")
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if d.is_empty() {
            return true;
        }
        host == d || host.ends_with(&format!(".{d}"))
    }

    /// Shape one jar cookie for the viewer. Values included: the viewer is
    /// the point (name, domain, expiry), and values never leave the device.
    /// They travel only from the engine to this settings page.
    fn cookie_row(host: &str, c: &wry::cookie::Cookie<'_>) -> serde_json::Value {
        let domain = c.domain().unwrap_or("").to_owned();
        let cookie_host = domain.trim().trim_start_matches('.').to_ascii_lowercase();
        let third = !cookie_host.is_empty()
            && cookie_host != host
            && !host.ends_with(&format!(".{cookie_host}"));
        let expiry = match c.expires() {
            None => "Session".to_owned(),
            Some(wry::cookie::Expiration::Session) => "Session".to_owned(),
            Some(wry::cookie::Expiration::DateTime(dt)) => dt.to_string(),
        };
        serde_json::json!({
            "name": c.name(),
            "value": c.value(),
            "domain": domain,
            "path": c.path().unwrap_or(""),
            "expiry": expiry,
            "secure": c.secure().unwrap_or(false),
            "httponly": c.http_only().unwrap_or(false),
            "samesite": c.same_site().map(|s| s.to_string()).unwrap_or_default(),
            "third": third,
        })
    }

    /// A live webview handle for jar reads and writes. The jar is
    /// profile-scoped, so any live webview sees the same cookies. Active
    /// tab preferred, first live otherwise. `None` means all discarded.
    fn jar_handle(&self) -> Option<Rc<dyn ferrite_tabs::WebViewHandle>> {
        self.app
            .tabs
            .active_tab()
            .and_then(|t| t.webview.clone())
            .or_else(|| self.app.tabs.iter().find_map(|t| t.webview.clone()))
    }

    /// Cookie rows for one origin from the profile jar, capped at 200.
    /// Returns (host, rows). Empty host means the input was unusable.
    fn cookie_rows(&self, origin: &str) -> (String, Vec<serde_json::Value>) {
        let host = Self::cookie_origin_host(origin);
        if host.is_empty() {
            return (String::new(), Vec::new());
        }
        let Some(handle) = self.jar_handle() else {
            return (host, Vec::new());
        };
        let jar = handle.cookies_all().unwrap_or_default();
        let rows = jar
            .iter()
            .filter(|c| Self::cookie_domain_matches(&host, c.domain()))
            .take(200)
            .map(|c| Self::cookie_row(&host, c))
            .collect();
        (host, rows)
    }

    /// Delete the cookies of one origin from the profile jar (scoped reset
    /// with `DeleteCookiesWithDomainAndPath` semantics). Never a blanket wipe.
    /// Returns the number actually deleted.
    fn cookies_clear_site(&self, origin: &str) -> (String, u64) {
        let host = Self::cookie_origin_host(origin);
        if host.is_empty() {
            return (String::new(), 0);
        }
        let Some(handle) = self.jar_handle() else {
            return (host, 0);
        };
        let jar = handle.cookies_all().unwrap_or_default();
        let mut n = 0u64;
        for c in jar
            .iter()
            .filter(|c| Self::cookie_domain_matches(&host, c.domain()))
        {
            match handle.delete_cookie(c) {
                Ok(()) => n += 1,
                Err(e) => debug!(name = %c.name(), error = %e, "site cookie delete failed"),
            }
        }
        (host, n)
    }

    /// Count cookies for one origin (site page display). Same matching
    /// as the clearer above, read-only.
    fn cookies_count_site(&self, origin: &str) -> u64 {
        let host = Self::cookie_origin_host(origin);
        if host.is_empty() {
            return 0;
        }
        let Some(handle) = self.jar_handle() else {
            return 0;
        };
        handle
            .cookies_all()
            .unwrap_or_default()
            .iter()
            .filter(|c| Self::cookie_domain_matches(&host, c.domain()))
            .count() as u64
    }

    /// Delete every cookie in the profile jar ("clear all cookies").
    /// Time ranges do not apply (jar entries carry expiry, not creation
    /// time). The UI says all-time for cookies.
    fn cookies_clear_all(&self) -> u64 {
        let Some(handle) = self.jar_handle() else {
            return 0;
        };
        let jar = handle.cookies_all().unwrap_or_default();
        let mut n = 0u64;
        for c in jar.iter() {
            match handle.delete_cookie(c) {
                Ok(()) => n += 1,
                Err(e) => debug!(name = %c.name(), error = %e, "cookie delete failed"),
            }
        }
        n
    }

    // ── Tabs ──

    /// Close a tab, remembering it for Ctrl+Shift+T (capped at 10).
    fn close_tab_recorded(&mut self, id: Uuid) {
        // A closed tab takes pending script dialogs with it (outstanding
        // deferrals complete as cancelled).
        self.clear_tab_dialogs(id);
        // ... and its PDF viewer sessions (children killed, bytes zeroized).
        self.pdf_sessions.close_tab_sessions(id);
        if let Some(t) = self.app.tabs.get_tab(id).map(|t| {
            (
                t.url.clone(),
                t.title.clone(),
                t.history.clone(),
                t.scroll_position,
                t.is_pinned,
                t.is_private,
            )
        }) {
            let (url, title, history, scroll, pinned, private) = t;
            match self.app.tabs.close_tab(id) {
                Ok(()) => {
                    self.remember_closed(url, title, history, scroll, pinned, private);
                }
                Err(e) => debug!(error = %e, "close tab ignored"),
            }
        }
        // A closed scoped tab takes its find state with it (tick pushes
        // the flag change via the state signature, no push needed here).
        if self.find_open
            && self
                .find_tab
                .is_some_and(|f| self.app.tabs.get_tab(f).is_none())
        {
            self.find_reset();
        }
        self.hide_ctx_menu();
        self.layout_tabs();
    }

    /// Resolve a bubble download action target by id: (filename, url,
    /// state). Vault rows first, then live in-progress log events.
    fn bubble_dl_target(&self, id: &str) -> Option<(String, String, String)> {
        if let Some(r) = self.vault.as_ref().and_then(|v| v.download_by_id(id)) {
            return Some((r.filename, r.url, r.state));
        }
        self.download_log
            .iter()
            .rev()
            .find(|e| e.id == id)
            .map(|e| (e.filename.clone(), e.url.clone(), format!("{:?}", e.state)))
    }

    /// Bubble open/reveal by id. Missing files toast (a silent button
    /// reads as broken); unknown ids only log.
    fn bubble_dl_open(&mut self, id: &str, reveal: bool) {
        match self.bubble_dl_target(id) {
            Some((name, _, _)) if !name.is_empty() && !name.contains(['/', '\\']) => {
                let path = ferrite_tabs::default_download_dir().join(&name);
                if path.is_file() {
                    open_path(&name, reveal);
                } else {
                    self.notify("That file is gone.".to_owned());
                }
            }
            Some(_) => debug!(id, "bubble download ignored: bad filename"),
            None => debug!(id, "bubble download ignored: unknown id"),
        }
    }

    /// Reopen the most recently closed tab with its history, scroll,
    /// and pin. The entry pops only when the tab actually opens: a refused
    /// open (tab limit, invalid URL) keeps it for the next attempt instead
    /// of losing it.
    fn reopen_closed_tab(&mut self) {
        if let Some(entry) = self.closed_tabs.last().cloned() {
            if self.open_restored(&entry).is_some() {
                self.closed_tabs.pop();
            }
        }
    }

    /// Closed stack in display order (most recent first) for the history
    /// dropdown. Pure view over the stack: indices here are what
    /// `ClosedReopen` carries back.
    fn closed_display(&self) -> Vec<(String, String)> {
        self.closed_tabs
            .iter()
            .rev()
            .map(|e| (e.url.clone(), e.title.clone()))
            .collect()
    }

    /// Entry by display index (most-recent-first), shared by peek + take.
    fn closed_at(&self, index: usize) -> Option<&ClosedTab> {
        let pos = self.closed_tabs.len().checked_sub(index)?.checked_sub(1)?;
        self.closed_tabs.get(pos)
    }

    /// Remove one closed entry by display index. Out of range returns
    /// None (stale menu, never a panic).
    fn take_closed_at(&mut self, index: usize) -> Option<ClosedTab> {
        let pos = self.closed_tabs.len().checked_sub(index)?.checked_sub(1)?;
        if pos < self.closed_tabs.len() {
            Some(self.closed_tabs.remove(pos))
        } else {
            // Unreachable (pos < len by construction above).
            None
        }
    }

    /// Drain the closed stack oldest-first for Restore-all. Callers open
    /// in order (newest opens last, ending active) and keep failures.
    fn drain_closed_all(&mut self) -> Vec<ClosedTab> {
        std::mem::take(&mut self.closed_tabs)
    }

    /// Persist a terminal download plus post-process the file. Completed
    /// files get a SHA-256 and a zone marker. Best-effort, uses the real
    /// event path. Executables are flagged dangerous. Session rule: downloads
    /// under a temp session tree (guest `ferrite/guest-*` or private-tab
    /// `ferrite/private-*`) are never recorded to the Vault: the file
    /// itself is the user's explicit artifact, and a history row would be
    /// unwanted persistence. They still appear in the in-memory status strip.
    fn finalize_download(&self, ev: &ferrite_tabs::DownloadEvent) {
        use ferrite_tabs::DownloadStateKind as Ds;
        let path: std::path::PathBuf = ev
            .path
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ferrite_tabs::default_download_dir().join(&ev.filename));
        let (size, hash) = if matches!(ev.state, Ds::Completed) {
            let meta = std::fs::metadata(&path).ok();
            let hash = ferrite_tabs::sha256_file(&path);
            if hash.is_some() && !ferrite_tabs::verify_zone_identifier(&path) {
                ferrite_tabs::mark_zone_identifier(&path);
            }
            (meta.map(|m| m.len()), hash)
        } else {
            (None, None)
        };
        let danger = ferrite_tabs::is_dangerous_ext(&ev.filename);
        if danger && matches!(ev.state, Ds::Completed) {
            warn!(filename = %ev.filename, "downloaded executable: confirm before opening");
        }
        if Self::is_session_temp_download(ev.path.as_deref()) {
            return;
        }
        if let Some(vault) = self.vault.as_ref()
            && let Err(e) = vault.record_download(
                &ev.id,
                &ev.url,
                &ev.filename,
                ev.path.as_deref(),
                size,
                hash.as_deref(),
                &format!("{:?}", ev.state),
                danger,
            )
        {
            debug!(error = %e, "download record failed");
        }
    }

    /// Whether a download path lives under a session temp tree and must never
    /// reach the Vault: `temp/ferrite/guest-*` (incognito) or
    /// `temp/ferrite/private-*` (private tabs). Pure, unit-tested.
    fn is_session_temp_download(path: Option<&str>) -> bool {
        let Some(p) = path else {
            return false;
        };
        let lower = p.replace('\\', "/").to_lowercase();
        lower.contains("/ferrite/guest-") || lower.contains("/ferrite/private-")
    }

    /// Bookmark toggle for the star button and Ctrl+D. Internal pages are
    /// never starred. Delete resolves the id through the URL-keyed cache.
    /// Refusals toast (silent no-ops on the star read as broken chrome).
    fn toggle_star(&mut self, url: String, title: String) {
        let Some(vault) = self.vault.as_ref() else {
            debug!("star ignored: vault unavailable");
            self.notify("Bookmarks need the vault available.".to_owned());
            return;
        };
        if url.trim().is_empty() || url.starts_with("ferrite://") {
            self.notify("Only web pages can be bookmarked.".to_owned());
            return;
        }
        match vault.bookmark_cache.get(&url).map(|e| e.value().0.clone()) {
            Some(id) => {
                if let Err(e) = vault.delete_bookmark(&id) {
                    debug!(error = %e, "bookmark delete failed");
                }
                self.push_bookmarks_tabs();
                self.push_undo(
                    "Bookmark removed".to_owned(),
                    crate::toast::UndoPayload::Bookmarks(vec![id]),
                );
            }
            None => {
                if self.save_bookmark(url, title, None) {
                    self.maybe_autoreveal_bar();
                }
            }
        }
    }

    /// Save helper shared by star, popover, drop, and bookmark-all.
    /// Returns true when a row was inserted (for the auto-reveal rule).
    /// Internal pages and empties are refused, never stored.
    fn save_bookmark(&self, url: String, title: String, parent: Option<String>) -> bool {
        let Some(vault) = self.vault.as_ref() else {
            debug!("bookmark save ignored: vault unavailable");
            return false;
        };
        if url.trim().is_empty() || url.starts_with("ferrite://") {
            return false;
        }
        let title = if title.trim().is_empty() {
            url.clone()
        } else {
            title
        };
        match vault.insert_bookmark(&url, &title, parent.as_deref()) {
            Ok(_) => {
                // Fan-out lives here (not per caller): every save path,
                // star, popover, drop, bookmark-all, ctx-link, refreshes
                // open manager tabs exactly once.
                self.push_bookmarks_tabs();
                true
            }
            Err(e) => {
                debug!(error = %e, "bookmark save failed");
                false
            }
        }
    }

    /// First-save auto-reveal: a hidden bar appears once with the new
    /// bookmark on it. Whatever the user does with the toggle after that
    /// sticks (the `bar_seen` flag records the reveal, not consent).
    fn maybe_autoreveal_bar(&mut self) {
        if self.bar_mode == "never" && !self.bar_seen {
            self.bar_seen = true;
            self.set_bar_mode("always");
        }
    }

    /// Set the bar mode (`always`, `never`, `ntp`), persist, and relayout.
    /// Showing the bar steals 30px from the content slot.
    fn set_bar_mode(&mut self, mode: &str) {
        if !matches!(mode, "always" | "never" | "ntp") {
            debug!(mode, "unknown bar mode ignored");
            return;
        }
        if self.bar_mode != mode {
            self.bar_mode = mode.to_owned();
            persist_setting("bar_mode", serde_json::Value::String(mode.to_owned()));
            persist_setting("bar_seen", serde_json::Value::Bool(self.bar_seen));
            info!(mode, "bookmarks bar mode");
        }
        self.layout_tabs();
        self.push_chrome(true);
    }

    /// Set the vertical tab rail, persist, and relayout. Docking the strip
    /// left steals `VTAB_W_PX` from the content slot (see `content_rect`)
    /// and drops the top chrome to the omnibar row.
    fn set_tabs_vertical(&mut self, vertical: bool) {
        if self.tabs_vertical != vertical {
            self.tabs_vertical = vertical;
            persist_setting("tabs_vertical", serde_json::Value::Bool(vertical));
            info!(vertical, "vertical tabs");
        }
        self.layout_tabs();
        self.push_chrome(true);
    }

    /// Toggle the side panel (rail icon, Ctrl+B), persist, relayout, and
    /// load the remembered section. Closing keeps the section for next time.
    fn set_panel_open(&mut self, open: bool) {
        self.panel_open = open;
        persist_setting("panel_open", serde_json::Value::Bool(open));
        info!(open, "side panel");
        self.layout_tabs();
        if open {
            self.push_panel_section();
        }
        self.push_chrome(true);
    }

    /// Switch panel section, persist, push its list.
    fn set_panel_section(&mut self, id: &str) {
        if !matches!(id, "bookmarks" | "history" | "reading") {
            debug!(id, "unknown panel section ignored");
            return;
        }
        self.panel_section = id.to_owned();
        persist_setting("panel_section", serde_json::Value::String(id.to_owned()));
        if self.panel_open {
            self.push_panel_section();
        }
        self.push_chrome(true);
    }

    /// Resize the panel (clamped), persist, relayout.
    fn set_panel_width(&mut self, w: u32) {
        let w = w.clamp(240, 480);
        if self.panel_w == w {
            return;
        }
        self.panel_w = w;
        persist_setting("panel_w", serde_json::Value::Number(w.into()));
        self.layout_tabs();
        self.push_chrome(true);
    }

    /// Find-in-page: run one engine command on the active tab. Query and
    /// accent travel JSON-quoted inside the script. The command word is a
    /// Rust-side literal, never user text. False = no tab to run on.
    fn find_eval(&self, cmd: &str, query: &str, case: bool) -> bool {
        let accent = self.app.theme.resolved_interactive_accent();
        let js = format!(
            "window.__ferriteFind&&__ferriteFind('{cmd}',{},{},{})",
            serde_json::to_string(query).unwrap_or_default(),
            if case { "true" } else { "false" },
            serde_json::to_string(accent).unwrap_or_default(),
        );
        let Some(tab) = self.app.tabs.active_tab() else {
            return false;
        };
        let Some(h) = tab.webview.as_ref() else {
            return false;
        };
        if let Err(e) = h.evaluate_script(&js) {
            debug!(error = %e, "find eval failed");
            return false;
        }
        true
    }

    /// Drop find flags without touching the page. Navigation and reload
    /// kill the document, so its highlights die on their own.
    fn find_reset(&mut self) {
        self.find_open = false;
        self.find_query.clear();
        self.find_total = 0;
        self.find_index = 0;
        self.find_tab = None;
    }

    /// Open the engine inspector on the active tab (thin launch
    /// layer. No panels, no debug ports, nothing listening).
    fn open_devtools(&mut self) {
        if let Some(tab) = self.app.tabs.active_tab()
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.open_devtools()
        {
            debug!(error = %e, "devtools open failed");
        }
    }

    /// Print the active tab through the OS dialog (the only print path
    /// wry exposes. No print-to-PDF, no settings object, no preview
    /// surface to reuse. That stack wants engine APIs that do not exist).
    /// Refused in guest sessions: the dialog offers Save as PDF, which is
    /// a file write like any other.
    fn print_active(&mut self) {
        if is_ephemeral() {
            self.notify("Printing isn't available in Incognito sessions.".to_owned());
            return;
        }
        let Some(id) = self.app.tabs.active_id() else {
            return;
        };
        // PDF viewer: the document's own permissions gate the dialog.
        // Unrestricted documents (and every non-PDF tab) fall through.
        // Permissions are read live from the child session, not cached:
        // a second copy of truth would drift from the enforcer.
        if let Some(doc) = self.pdf_doc_for_tab(id) {
            let print_ok = self
                .pdf_sessions
                .doc_info(doc)
                .ok()
                .and_then(|v| v.get("perms")?.get("print")?.as_bool())
                .unwrap_or(true);
            if !print_ok {
                self.notify("Printing is restricted by this document's permissions.".to_owned());
                return;
            }
        }
        // One job per webview: the engine throws on concurrent print calls
        // instead of queueing, so the second trigger gets a message here
        // rather than an exception downstream.
        if !self.print_jobs.begin(id, Instant::now()) {
            // The OS dialog reports no result: a dismissed dialog leaves
            // the guard held until navigation or the 5-minute stale
            // eviction. Past 30s, assume dismissal and retry once; a
            // truly concurrent job fails at the engine with a toast
            // instead of wedging Ctrl+P for minutes.
            let retry = self
                .print_jobs
                .held_for(&id, Instant::now())
                .is_some_and(|held| held.as_secs() >= 30);
            if retry {
                self.print_jobs.finish(&id);
            }
            if !retry || !self.print_jobs.begin(id, Instant::now()) {
                self.notify("Printing is already in progress for this tab.".to_owned());
                return;
            }
        }
        // No-printer preflight (known-empty only. Unknown stays silent).
        // The dialog still opens: it offers Save as PDF either way.
        if let Some(note) =
            ferrite_tabs::print::no_printer_notice(&ferrite_tabs::print::enumerate_printers())
        {
            self.notify(note);
        }
        let opened = self
            .app
            .tabs
            .get_tab(id)
            .and_then(|t| t.webview.as_ref())
            .map(|h| h.print())
            .transpose();
        // Success is unknowable (wry exposes no print result), so success
        // stays silent. Only a dialog that never opened gets a toast. The
        // guard releases on navigation commit or stale eviction.
        if !matches!(opened, Ok(Some(()))) {
            self.print_jobs.finish(&id);
            self.notify("Couldn't open the print dialog.".to_owned());
        }
    }

    /// Push a JSON payload into a viewer hook (`window.__ferritePdfX`).
    /// `</` stays escaped for the script string context, same as lists.
    fn push_pdf(&self, tab_id: Uuid, hook: &str, payload: &serde_json::Value) {
        let json = serde_json::to_string(payload)
            .unwrap_or_else(|_| "null".to_owned())
            .replace("</", "<\\/");
        if let Some(h) = self
            .app
            .tabs
            .get_tab(tab_id)
            .and_then(|t| t.webview.as_ref())
        {
            let js = format!("window.{hook}&&window.{hook}({json})");
            if let Err(e) = h.evaluate_script(&js) {
                debug!(error = %e, hook, "pdf push failed");
            }
        }
    }

    /// Session id for the viewer tab, if it owns one. Viewer URLs carry
    /// `?doc=<id>`; sessions additionally record the owning tab, so a
    /// tab that navigated away stops matching even with a stale URL.
    fn pdf_doc_for_tab(&self, tab_id: Uuid) -> Option<Uuid> {
        let url = self.app.tabs.get_tab(tab_id).map(|t| t.url.clone())?;
        if !url.starts_with("ferrite://pdf/") {
            return None;
        }
        let query = url.split('?').nth(1).unwrap_or("");
        for pair in query.split('&') {
            if let Some(id) = pair.strip_prefix("doc=") {
                if let Ok(doc) = id.parse::<Uuid>() {
                    // The session must still exist and belong to this tab:
                    // a restored session URL without a session is a miss.
                    return Some(doc)
                        .filter(|_| self.pdf_sessions.get(doc).is_some_and(|s| s.tab == tab_id));
                }
            }
        }
        None
    }

    /// Parse + authorize a `doc` param: valid UUID, live session, owned
    /// by the posting tab. Anything else is a silent miss (a confused
    /// page gets nothing, a hostile one learns nothing).
    fn pdf_arm_doc(&mut self, tab_id: Uuid, v: &serde_json::Value) -> Option<Uuid> {
        let doc = v
            .get("doc")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<Uuid>().ok())?;
        let owned = self
            .pdf_sessions
            .get_mut(doc)
            .map(|s| s.tab == tab_id)
            .unwrap_or(false);
        owned.then_some(doc)
    }

    /// Parse a page request: doc pairing plus bounded page/scale/query.
    /// Scale arrives as milli units (1.0x = 1000); the renderer
    /// re-validates, so this clamp is UI hygiene, not the guard.
    fn pdf_arm_page(
        &mut self,
        tab_id: Uuid,
        v: &serde_json::Value,
    ) -> Option<(Uuid, usize, u32, Option<String>, bool)> {
        let doc = self.pdf_arm_doc(tab_id, v)?;
        let pages = self.pdf_sessions.get_mut(doc).ok()?.pages;
        let page = v.get("page").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        if pages > 0 && page >= pages {
            return None;
        }
        let scale = v
            .get("scale")
            .and_then(|x| x.as_u64())
            .unwrap_or(1000)
            .clamp(50, 8000) as u32;
        let query = v
            .get("q")
            .and_then(|x| x.as_str())
            .filter(|q| !q.is_empty() && q.len() <= 256)
            .map(|q| q.to_owned());
        let thumb = v.get("thumb").and_then(|x| x.as_bool()).unwrap_or(false);
        Some((doc, page, scale, query, thumb))
    }

    /// Navigate a tab to its viewer URL. The page boots itself with a
    /// `pdf_doc` post; no push races the load.
    fn pdf_navigate(&mut self, tab_id: Uuid, doc: Uuid) {
        if let Some(tab) = self.app.tabs.get_tab_mut(tab_id) {
            let url = format!("ferrite://pdf/?doc={doc}");
            if let Err(e) = tab.load_url(url) {
                debug!(error = %e, "pdf viewer navigation failed");
                self.notify("Couldn't open the PDF viewer.".to_owned());
            }
        }
    }

    /// Open bytes in a viewer tab: session first (fails loudly before
    /// any navigation), navigation second (the page pulls its data).
    fn pdf_open_bytes(
        &mut self,
        tab_id: Uuid,
        title: String,
        source: String,
        bytes: Vec<u8>,
        password: Option<String>,
    ) {
        if self.app.tabs.get_tab(tab_id).is_none() {
            return;
        }
        match self
            .pdf_sessions
            .open_bytes(tab_id, title, source, bytes, password)
        {
            Ok((doc, _, _)) => self.pdf_navigate(tab_id, doc),
            Err(e) => self.notify(format!("Couldn't open PDF: {e}")),
        }
    }

    /// Open a `file://` PDF URL: decode, read with caps, hand to the
    /// session store. No extension trust: magic bytes decide.
    fn pdf_open_local(&mut self, tab_id: Uuid, url: &str) {
        let Some(path) = crate::pdf::decode_file_url(url) else {
            self.notify("Couldn't open PDF: bad file URL.".to_owned());
            return;
        };
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len == 0 || len > crate::pdf::MAX_PDF_BYTES as u64 {
            self.notify("Couldn't open PDF: file length out of bounds.".to_owned());
            return;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                let title = crate::pdf::display_title(url);
                self.pdf_open_bytes(tab_id, title, url.to_owned(), bytes, None);
            }
            Err(e) => {
                debug!(error = %e, "pdf local read failed");
                self.notify("Couldn't open PDF: file unreadable.".to_owned());
            }
        }
    }

    /// Full document bundle for `__ferritePdfDoc`: metadata, permissions,
    /// engine presence, outline, and form state in one push.
    fn pdf_doc_bundle(&mut self, doc: Uuid) -> Result<serde_json::Value, String> {
        let info = self.pdf_sessions.doc_info(doc)?;
        let outline = self.pdf_sessions.doc_outline(doc).unwrap_or_default();
        let fields = self.pdf_sessions.doc_forms(doc).unwrap_or_default();
        let (title, pages, locked) = self
            .pdf_sessions
            .get_mut(doc)
            .map(|s| (s.title.clone(), s.pages, s.locked))?;
        Ok(serde_json::json!({
            "doc": doc.to_string(),
            "title": title,
            "pages": pages,
            "locked": locked,
            "info": info,
            "outline": outline.get("entries").cloned().unwrap_or_default(),
            "fields": fields.get("fields").cloned().unwrap_or_default(),
        }))
    }

    /// One page bundle for `__ferritePdfPage`: PNG data URL plus the
    /// overlays the viewer needs (text lines, links, search matches),
    /// all in page fractions so any zoom just scales CSS. Thumbnails
    /// (`thumb: true`) skip the overlays: one image, no parsing cost.
    fn pdf_page_bundle(
        &mut self,
        doc: Uuid,
        page: usize,
        scale_milli: u32,
        query: Option<String>,
        thumb: bool,
    ) -> Result<serde_json::Value, String> {
        let scale = (scale_milli as f32 / 1000.0).clamp(0.05, 8.0);
        let (w, h, b64) = self.pdf_sessions.render_page_png(doc, page, scale)?;
        if thumb {
            return Ok(serde_json::json!({
                "doc": doc.to_string(),
                "page": page,
                "w": w,
                "h": h,
                "img": format!("data:image/png;base64,{b64}"),
                "thumb": true,
            }));
        }
        let lines = self.pdf_sessions.page_text(doc, page).unwrap_or_default();
        let links = self.pdf_sessions.page_links(doc, page).unwrap_or_default();
        let matches = match query.filter(|q| !q.is_empty() && q.len() <= 256) {
            Some(q) => {
                let case = false;
                self.pdf_sessions
                    .search_all(doc, q, case)
                    .ok()
                    .and_then(|v| {
                        v.get("hits").and_then(|hits| {
                            hits.as_array().and_then(|arr| {
                                arr.iter().find_map(|h| {
                                    (h.get("page").and_then(|p| p.as_u64())? == page as u64)
                                        .then(|| h.get("rects").cloned().unwrap_or_default())
                                })
                            })
                        })
                    })
                    .unwrap_or_default()
            }
            None => serde_json::Value::Null,
        };
        Ok(serde_json::json!({
            "doc": doc.to_string(),
            "page": page,
            "w": w,
            "h": h,
            "img": format!("data:image/png;base64,{b64}"),
            "lines": lines.get("lines").cloned().unwrap_or_default(),
            "links": links.get("links").cloned().unwrap_or_default(),
            "matches": matches,
        }))
    }

    /// Save-a-copy: serialize the (possibly filled) document, land it in
    /// the downloads dir de-duplicated, and register it with the download
    /// manager so the bubble, badge, and history all agree. Explicit user
    /// gesture only: nothing here autosaves.
    fn pdf_save_copy(&mut self, tab_id: Uuid, doc: Uuid) {
        let bytes = match self.pdf_sessions.save_bytes(doc) {
            Ok(b) => b,
            Err(e) => {
                self.notify(format!("Couldn't save PDF: {e}"));
                return;
            }
        };
        let title = self
            .pdf_sessions
            .get_mut(doc)
            .map(|s| s.title.clone())
            .unwrap_or_else(|_| "document.pdf".to_owned());
        let filename = if title.to_ascii_lowercase().ends_with(".pdf") {
            title
        } else {
            format!("{title}.pdf")
        };
        let filename = ferrite_tabs::sanitize_filename(&filename);
        let dir = ferrite_tabs::default_download_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            self.notify("Couldn't save PDF: downloads folder unavailable.".to_owned());
            return;
        }
        let dest = ferrite_tabs::decide_destination(&dir, &filename);
        match std::fs::write(&dest, &bytes) {
            Ok(()) => {
                let source = self
                    .pdf_sessions
                    .get_mut(doc)
                    .map(|s| s.source.clone())
                    .unwrap_or_default();
                let id = self
                    .downloads
                    .start_with_path(&source, &filename, Some(&dest));
                self.downloads
                    .complete_with_path(&id, &source, &filename, Some(&dest));
                self.notify(format!("Saved {filename}"));
                self.push_pdf(
                    tab_id,
                    "__ferritePdfSaved",
                    &serde_json::json!({"doc": doc.to_string(), "file": filename}),
                );
            }
            Err(e) => {
                debug!(error = %e, "pdf save failed");
                self.notify("Couldn't save PDF: write failed.".to_owned());
            }
        }
    }

    /// Recent tracker hosts for PiP ad-aware scoring: the blocklist signal
    /// embedded into the enter script, so an autoplaying ad loses to the
    /// feature video. Capped, evaluated verbatim per PiP request.
    fn pip_tracker_hosts(&self) -> Vec<String> {
        self.privacy.recent_tracker_hosts(32)
    }

    /// Float the active tab best playing video. `silent` skips the failure
    /// toast (background auto-PiP lacks gesture credit). Explicit clicks
    /// report through `pip_error` IPC instead.
    fn pip_enter_active(&mut self, silent: bool) {
        let Some(tab) = self.app.tabs.active_tab() else {
            return;
        };
        let Some(h) = tab.webview.as_ref() else {
            return;
        };
        let js = ferrite_tabs::pip_enter_js(&self.pip_tracker_hosts());
        if let Err(e) = h.evaluate_script(&js) {
            debug!(error = %e, "pip enter failed");
            if !silent {
                self.notify("Picture-in-Picture isn't available here.".to_owned());
            }
        }
    }

    /// Float a specific (usually backgrounded) tab video. The auto-PiP
    /// path after a tab switch. Always silent: see `pip_enter_active`.
    fn pip_enter_tab(&self, id: Uuid) {
        let Some(tab) = self.app.tabs.get_tab(id) else {
            return;
        };
        if !tab.pip_eligible || tab.is_pip_active {
            return;
        }
        let Some(h) = tab.webview.as_ref() else {
            return;
        };
        let js = ferrite_tabs::pip_enter_js(&self.pip_tracker_hosts());
        if let Err(e) = h.evaluate_script(&js) {
            debug!(error = %e, "auto pip enter failed");
        }
    }

    /// Run one menu action by id. Single execution path for the main menu,
    /// the command palette, and any future trigger. A palette row and its
    /// menu row can never desync.
    fn menu_action(&mut self, id: &str) {
        match id {
            "new-tab" => self.open_tab("ferrite://newtab".to_owned()),
            "new-incognito" => self.open_incognito_window(),
            "settings" => {
                self.open_singleton("ferrite://settings", "ferrite://settings".to_owned())
            }
            "settings-privacy" => self.open_singleton(
                "ferrite://settings",
                "ferrite://settings#privacy".to_owned(),
            ),
            "passwords" => self.open_tab("ferrite://passwords".to_owned()),
            "archive" => self.open_tab("ferrite://archive/search".to_owned()),
            "task-manager" => self.open_tab("ferrite://taskmanager".to_owned()),
            "print" => self.print_active(),
            "share" => self.share_active(),
            // QR through the share fallback page (which renders the code):
            // reachable even when the native sheet owns the share button.
            "qr-code" => {
                if let Some(tab) = self.app.tabs.active_tab() {
                    let (url, title) = (tab.url.clone(), tab.title.clone());
                    self.open_share_page(&url, &title);
                }
            }
            // Replay onboarding any time (About links here too). Singleton:
            // replays never stack duplicate wizards.
            "welcome" => self.open_singleton("ferrite://welcome", "ferrite://welcome".to_owned()),
            "palette" => self.toggle_web_tabsearch(),
            "history" => self.open_tab("ferrite://history".to_owned()),
            "downloads" => self.open_tab("ferrite://downloads".to_owned()),
            "bookmarks" => self.open_tab("ferrite://bookmarks".to_owned()),
            "status" => self.status_visible = !self.status_visible,
            "find" => self.open_find_bar(),
            "bookmark-page" => {
                if let Some(tab) = self.app.tabs.active_tab() {
                    let (url, title) = (tab.url.clone(), tab.title.clone());
                    if self.save_bookmark(url, title, None) {
                        self.maybe_autoreveal_bar();
                    }
                }
                self.push_panel_section();
                self.push_bookmarks_tabs();
            }
            "bookmark-all" => {
                self.bookmark_all_tabs();
                self.push_panel_section();
                self.push_bookmarks_tabs();
            }
            "bookmarks-bar" => {
                let next = match self.bar_mode.as_str() {
                    "always" => "ntp",
                    "ntp" => "never",
                    _ => "always",
                };
                self.bar_seen = true;
                persist_setting("bar_seen", serde_json::Value::Bool(true));
                self.set_bar_mode(next);
            }
            "bar-always" => self.set_bar_mode("always"),
            "bar-ntp" => self.set_bar_mode("ntp"),
            "bar-never" => self.set_bar_mode("never"),
            "zoom-in" => self.zoom_step_active(1),
            "zoom-out" => self.zoom_step_active(-1),
            "zoom-reset" => self.zoom_reset_active(),
            "side-panel" => self.set_panel_open(!self.panel_open),
            "vertical-tabs" => self.set_tabs_vertical(!self.tabs_vertical),
            "sleep-active" => self.sleep_active_tab(),
            other if other.starts_with("theme:") => {
                match ThemePack::from_name(other.trim_start_matches("theme:")) {
                    Some(pack) => self.set_theme_pack(pack),
                    None => debug!(menu = other, "unknown theme ignored"),
                }
            }
            other => debug!(menu = other, "menu item without a page - ignored"),
        }
    }

    /// Close the active tab held PiP window (safe no-op otherwise).
    fn pip_exit_active(&self) {
        if let Some(tab) = self.app.tabs.active_tab()
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(ferrite_tabs::pip_exit_js())
        {
            debug!(error = %e, "pip exit failed");
        }
    }

    /// Share the active tab: native sheet where one exists (Windows ShareUI,
    /// macOS picker), the `ferrite://share` fallback page everywhere else.
    fn share_active(&mut self) {
        let Some(tab) = self.app.tabs.active_tab() else {
            return;
        };
        let (id, url, title) = (tab.id, tab.url.clone(), tab.title.clone());
        self.share_tab(id, &url, &title);
    }

    /// Text-fragment link (`#:~:text=...`) for a selection. Whitespace
    /// collapses, the snippet caps at 300 chars on a word boundary, and
    /// any existing fragment is replaced (a URL carries one). http(s)
    /// only, and the selection must hold a real word: bare punctuation
    /// links nowhere. Pure for tests.
    fn fragment_link(url: &str, selection: &str) -> Option<String> {
        let lower = url.trim().to_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return None;
        }
        let base = url.split('#').next().unwrap_or(url);
        let words: Vec<&str> = selection.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }
        let mut snippet = String::new();
        for w in words {
            if w.chars().any(|c| c.is_alphanumeric()) {
                if snippet.len() + w.len() + 1 > 300 {
                    break;
                }
                if !snippet.is_empty() {
                    snippet.push(' ');
                }
                snippet.push_str(w);
            }
        }
        if !snippet.chars().any(|c| c.is_alphanumeric()) {
            return None;
        }
        Some(format!("{base}#:~:text={}", fragment_encode(&snippet)))
    }

    /// Share an arbitrary http(s) URL from `tab_id` (context-menu links
    /// carry no page of their own, so no OG enrichment applies there).
    fn share_tab(&mut self, tab_id: Uuid, url: &str, title: &str) {
        let Some(payload) = ferrite_tabs::share::share_payload(url, title, None) else {
            self.notify("There's nothing shareable here.".to_owned());
            return;
        };
        if !crate::share::system_share_available() {
            self.open_share_page(&payload.url, &payload.title);
            return;
        }
        // Enrich via OG, then dispatch (the tick sweep covers a silent
        // extractor with the basic payload instead of hanging the click).
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && h.evaluate_script(ferrite_tabs::share::og_extract_js())
                .is_ok()
        {
            self.pending_share = Some((tab_id, Instant::now()));
            return;
        }
        // No webview to ask (discarded tab): dispatch unenriched.
        self.native_share(tab_id, None);
    }

    /// Dispatch the native sheet for `tab_id`, enriching with `meta` when
    /// the extractor answered. Rebuilds from the live tab so a racing
    /// navigation shares the current document, not a stale click.
    fn native_share(&mut self, tab_id: Uuid, meta: Option<ferrite_tabs::share::ShareMeta>) {
        let Some(tab) = self.app.tabs.get_tab(tab_id) else {
            return;
        };
        let Some(payload) = ferrite_tabs::share::share_payload(&tab.url, &tab.title, meta.as_ref())
        else {
            self.notify("There's nothing shareable here.".to_owned());
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        crate::share::show_native(payload.into(), &window, &self.notice_tx);
    }

    /// Open the server-rendered fallback sheet for a clean URL + title.
    fn open_share_page(&mut self, url: &str, title: &str) {
        use ferrite_tabs::protocol::encode_query_component;
        self.open_tab(format!(
            "ferrite://share?u={}&t={}",
            encode_query_component(url),
            encode_query_component(title)
        ));
    }

    /// Confirm a share-page copy inside the page (`__ferriteShared` hook).
    fn confirm_shared(&self, tab_id: Uuid, text: &str) {
        let json = serde_json::to_string(text)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!(
                "window.__ferriteShared&&window.__ferriteShared({json})"
            ))
        {
            debug!(error = %e, "share confirm failed");
        }
    }

    /// Full palette snapshot: live tabs, the command registry, settings
    /// entries with deep links, and session command recency. Pushed once on
    /// open, re-pushed from the tick while the tab set changes.
    fn palette_snapshot(&self) -> String {
        let tabs = self.palette_tab_rows();
        let commands: Vec<serde_json::Value> = ferrite_chrome::commands::COMMANDS
            .iter()
            .map(|c| {
                serde_json::json!({
                    "kind": "command",
                    "id": c.id,
                    "title": c.label,
                    "hint": c.hint,
                    "keywords": c.keywords,
                })
            })
            .collect();
        let settings: Vec<serde_json::Value> = ferrite_tabs::protocol::SETTINGS_CATALOGUE
            .iter()
            .map(|s| {
                serde_json::json!({
                    "kind": "setting",
                    "id": s.id,
                    "title": s.label,
                    "keywords": s.keywords,
                    "url": palette_setting_url(s),
                })
            })
            .collect();
        let recent: Vec<&str> = self.palette_recent.iter().map(|s| s.as_str()).collect();
        serde_json::to_string(&serde_json::json!({
            "tabs": tabs,
            "commands": commands,
            "settings": settings,
            "recent": recent,
        }))
        .unwrap_or_else(|_| "{}".to_owned())
        .replace("</", "<\\/")
    }

    /// Live tab rows for the palette (shared by the full snapshot and the
    /// tick tabs-only refresh).
    fn palette_tab_rows(&self) -> Vec<serde_json::Value> {
        let active = self.app.tabs.active_id();
        self.app
            .tabs
            .iter()
            .map(|t| {
                serde_json::json!({
                    "kind": "tab",
                    "id": t.id.to_string(),
                    "title": t.title,
                    "url": t.url,
                    "discarded": t.is_discarded(),
                    "active": Some(t.id) == active,
                })
            })
            .collect()
    }

    /// Push the snapshot into the open overlay and remember its signature
    /// for the tick change detection.
    fn push_palette(&mut self) {
        let json = self.palette_snapshot();
        self.palette_sig = json.clone();
        if let Some(ov) = self.overlay.as_ref()
            && let Err(e) = ov.evaluate_script(&format!("__ferritePalette({json})"))
        {
            debug!(error = %e, "palette push failed");
        }
    }

    /// Run one palette row, then record command recency. Tabs switch (with
    /// restore, never a blank discard). Commands reuse the typed
    /// [`ChromeCommand`] path verbatim. Unknown ids are ignored.
    fn run_palette_row(&mut self, kind: &str, id: &str) {
        match kind {
            "tab" => {
                let idx = id
                    .parse::<Uuid>()
                    .ok()
                    .and_then(|tab_id| self.app.tabs.iter().position(|t| t.id == tab_id));
                if let Some(idx) = idx {
                    // Restore path (not bare switch): discarded tabs
                    // rebuild instead of going blank.
                    self.switch_to_index(idx);
                }
            }
            "setting" => {
                let url = palette_setting_url_by_id(id);
                self.open_singleton("ferrite://settings", url);
            }
            "command" => {
                if let Some(cmd) = ferrite_chrome::commands::palette_command(id) {
                    self.record_palette_use(id);
                    self.handle_chrome_command(cmd);
                } else {
                    debug!(id, "palette unknown command ignored");
                }
            }
            other => debug!(kind = other, "palette unknown kind ignored"),
        }
    }

    /// Record a command run for empty-query recency (MRU front, capped,
    /// deduped. Frequency implicit in survival, no counters to persist).
    fn record_palette_use(&mut self, id: &str) {
        self.palette_recent.retain(|x| x != id);
        self.palette_recent.push_front(id.to_owned());
        while self.palette_recent.len() > 12 {
            self.palette_recent.pop_back();
        }
    }

    /// Open the bar on the active tab, prefilled with the last term.
    fn open_find_bar(&mut self) {
        self.find_tab = self.app.tabs.active_id();
        self.find_query = self.last_find.clone();
        self.find_case = false;
        self.find_total = 0;
        self.find_index = 0;
        self.find_open = true;
        if !self.find_query.is_empty() {
            let q = self.find_query.clone();
            if !self.find_eval("search", &q, false) {
                self.find_reset();
            }
        }
        self.push_chrome(true);
    }

    /// Copy text to the OS clipboard (plain path: links, URLs, usernames).
    /// Empty is a no-op `false`, never an error popup. Secrets never come
    /// here, see `copy_sensitive_with_toast`.
    fn copy_text(&self, text: &str) -> bool {
        crate::clipboard::copy_plain(text)
    }

    /// Flush session block verdicts into the vault daily counter (the
    /// dashboard reads the vault, never these live counters). Delta-based.
    /// Quiet periods write nothing, restarts re-flush nothing.
    fn flush_blocked_day(&mut self) {
        let (_, ad, tracker, fingerprint, malware, mixed) = self.privacy.verdict_counts();
        let total = ad + tracker + fingerprint + malware + mixed;
        if total <= self.last_blocked_total {
            return;
        }
        let Some(vault) = self.vault.as_ref() else {
            return;
        };
        if let Err(e) =
            vault.record_blocked_day(now_millis() / 86_400_000, total - self.last_blocked_total)
        {
            debug!(error = %e, "blocked-day flush failed");
            return;
        }
        self.last_blocked_total = total;
    }

    /// New-tab payload: frecency tiles from autocomplete ranking
    /// (`query_history`, verbatim, one function, two callers), pin and remove
    /// overrides applied, honest block counts, unread reading.
    fn push_newtab_data(&self, tab_id: Uuid) {
        let (tiles, stats, reading) = match self.vault.as_ref() {
            Some(vault) => {
                let overrides = vault.ntp_overrides();
                let removed: std::collections::HashSet<&str> = overrides
                    .iter()
                    .filter(|t| t.removed)
                    .map(|t| t.url.as_str())
                    .collect();
                let pinned_urls: std::collections::HashSet<&str> = overrides
                    .iter()
                    .filter(|t| t.pinned)
                    .map(|t| t.url.as_str())
                    .collect();
                let mut tiles: Vec<serde_json::Value> = overrides
                    .iter()
                    .filter(|t| t.pinned)
                    .map(|t| serde_json::json!({"url": t.url, "title": t.title, "pinned": true}))
                    .collect();
                // "Top" is frecency, not recency: one visit an hour ago
                // must not outrank a daily. Pins (above) always lead and
                // are never duplicated below.
                let mut candidates: Vec<(i64, String, String)> = Vec::new();
                for e in vault.query_history("", 50) {
                    if e.url.starts_with("ferrite://")
                        || removed.contains(e.url.as_str())
                        || pinned_urls.contains(e.url.as_str())
                    {
                        continue;
                    }
                    let score = tile_score(e.visit_count, e.last_visit);
                    candidates.push((score, e.url, e.title.unwrap_or_default()));
                    if candidates.len() >= 50 {
                        break;
                    }
                }
                candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
                for (_, url, title) in candidates.into_iter().take(8 - tiles.len().min(8)) {
                    tiles.push(serde_json::json!({
                        "url": url,
                        "title": title,
                        "pinned": false,
                    }));
                }
                let (today, week, all) = vault.blocked_sums(now_millis() / 86_400_000);
                let reading: Vec<serde_json::Value> = vault
                    .reading_list(50)
                    .into_iter()
                    .filter(|r| !r.is_read)
                    .take(5)
                    .map(|r| serde_json::json!({"id": r.id, "url": r.url, "title": r.title}))
                    .collect();
                (
                    tiles,
                    serde_json::json!({"today": today, "week": week, "all": all}),
                    reading,
                )
            }
            None => (
                Vec::new(),
                serde_json::json!({"today": 0, "week": 0, "all": 0}),
                Vec::new(),
            ),
        };
        let payload = serde_json::json!({
            "tiles": tiles,
            "stats": stats,
            "reading": reading,
            "guest": self.guest_mode,
            // Banner parity for private tabs in user mode: the newtab page
            // cannot see tab flags, so Rust tells it explicitly per push.
            "private": self
                .app
                .tabs
                .get_tab(tab_id)
                .is_some_and(|t| t.is_private),
        });
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteNewTab({json})"))
        {
            debug!(error = %e, "newtab push failed");
        }
    }

    /// Malware interstitial: tabs parked on a freshly-blocked host get the
    /// local warning instead of the dead tunnel. No proceed path in v1,
    /// disabling the protection in Settings is the escape hatch.
    fn show_blocked_pages(&mut self, hosts: &[String]) {
        let targets: Vec<Uuid> = self
            .app
            .tabs
            .iter()
            .filter(|t| !t.url.starts_with("ferrite://"))
            .filter(|t| hosts.iter().any(|h| host_slice(&t.url) == h))
            .map(|t| t.id)
            .collect();
        for id in &targets {
            let url = hosts
                .iter()
                .find(|h| {
                    self.app
                        .tabs
                        .get_tab(*id)
                        .map(|t| host_slice(&t.url) == h.as_str())
                        .unwrap_or(false)
                })
                .cloned()
                .unwrap_or_default();
            if url.is_empty() {
                continue;
            }
            let dest = format!(
                "ferrite://blocked?host={}&at={}",
                encode_query(&url),
                now_millis()
            );
            if let Some(tab) = self.app.tabs.get_tab_mut(*id) {
                if let Err(e) = tab.load_url(dest) {
                    debug!(error = %e, "blocked nav failed");
                } else {
                    // Interstitial replaces the document: pending script
                    // dialogs die with the old page.
                    self.clear_tab_dialogs(*id);
                }
            }
        }
        if !targets.is_empty() {
            self.layout_tabs();
        }
    }

    /// Connection-error interstitial: tabs parked on a host whose tunnel
    /// died get the local error page with a working retry. Matched tabs
    /// hold the host, have a live webview, and navigated recently.
    fn show_error_pages(&mut self, failures: &[(String, String)]) {
        let now = now_millis();
        let mut targets: Vec<(Uuid, String, String)> = Vec::new();
        for (host, reason) in failures {
            for t in self.app.tabs.iter() {
                if !t.url.starts_with("ferrite://")
                    && t.webview.is_some()
                    && host_slice(&t.url) == host.as_str()
                    && now.saturating_sub(t.last_nav_at_ms) < ERROR_REDIRECT_WINDOW_MS
                    && !targets.iter().any(|(id, _, _)| *id == t.id)
                {
                    targets.push((t.id, t.url.clone(), reason.clone()));
                }
            }
        }
        for (id, url, reason) in &targets {
            let dest = format!(
                "ferrite://error?url={}&reason={}&at={}",
                encode_query(url),
                encode_query(reason),
                now_millis()
            );
            if let Some(tab) = self.app.tabs.get_tab_mut(*id) {
                if let Err(e) = tab.load_url(dest) {
                    debug!(error = %e, "error nav failed");
                } else {
                    self.clear_tab_dialogs(*id);
                }
            }
        }
        if !targets.is_empty() {
            self.layout_tabs();
        }
    }

    /// Fan-out: every open manager tab re-queries after a mutation from
    /// any surface (bar, panel, manager itself). Selections that vanished
    /// fall back page-side on the pushed tree.
    fn push_bookmarks_tabs(&self) {
        for id in self.page_tabs("ferrite://bookmarks") {
            let view = self.bm_views.get(&id).cloned().unwrap_or_default();
            self.push_bm_tree(id);
            if view.query.trim().is_empty() {
                self.push_bm_children(id, view.folder.as_deref());
            } else {
                self.push_bm_search(id, &view.query);
            }
            self.push_bm_dups(id);
        }
    }

    fn push_bm_tree(&self, tab_id: Uuid) {
        let items: Vec<serde_json::Value> = match self.vault.as_ref() {
            Some(v) => v
                .bookmark_folders()
                .into_iter()
                .map(|f| serde_json::json!({"id": f.id, "title": f.title, "parent": f.parent}))
                .collect(),
            None => Vec::new(),
        };
        self.push_list(tab_id, "__ferriteBmTree", &items);
    }

    fn push_bm_children(&self, tab_id: Uuid, folder: Option<&str>) {
        let items: Vec<serde_json::Value> = match self.vault.as_ref() {
            Some(v) => v
                .bookmark_details(folder)
                .into_iter()
                .map(|n| {
                    serde_json::json!({
                        "id": n.id, "title": n.title, "url": n.url,
                        "folder": n.is_folder, "added": n.added,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        self.push_list(tab_id, "__ferriteBmChildren", &items);
    }

    fn push_bm_search(&self, tab_id: Uuid, q: &str) {
        let items: Vec<serde_json::Value> = match self.vault.as_ref() {
            Some(v) => v
                .search_bookmarks(q, 100)
                .into_iter()
                .map(|n| {
                    serde_json::json!({
                        "id": n.id, "title": n.title, "url": n.url,
                        "folder": n.is_folder, "added": n.added,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        self.push_list(tab_id, "__ferriteBmChildren", &items);
    }

    fn push_bm_dups(&self, tab_id: Uuid) {
        let items: Vec<serde_json::Value> = match self.vault.as_ref() {
            Some(v) => v
                .duplicate_bookmark_urls()
                .into_iter()
                .map(|g| {
                    serde_json::json!({
                        "url": g.url,
                        "items": g.items.iter().map(|i| serde_json::json!({
                            "id": i.id, "title": i.title, "parent": i.parent,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        self.push_list(tab_id, "__ferriteBmDups", &items);
    }

    /// Hide the content context menu overlay, if any.
    fn hide_ctx_menu(&mut self) {
        // Close the type-ahead channel first: a stale open flag would
        // hijack Enter on a dropdown-less field.
        if let Some(p) = self.ctx_menu.clone() {
            self.af_set_flag(p.tab, false);
        }
        // Focus comes home: the overlay stole keyboard focus on open, and
        // a hidden webview keeps it otherwise, stranding keyboard and
        // screen-reader users. Only while the originating tab is still
        // active. A mid-menu tab switch already moved focus somewhere
        // newer. Best-effort: failure keeps the old behavior, never a log
        // loop.
        if let Some(p) = self.ctx_menu.clone()
            && ferrite_tabs::a11y::should_return_focus(p.tab, self.app.tabs.active_id())
            && let Some(tab) = self.app.tabs.get_tab(p.tab)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.focus()
        {
            debug!(error = %e, "ctx hide refocus failed");
        }
        self.af_open = false;
        self.af_items.clear();
        self.af_filter.clear();
        self.af_hl = None;
        self.ctx_menu = None;
        self.fill_options.clear();
        self.cred_fills.clear();
        self.form_fills.clear();
        self.totp_fills.clear();
        self.af_gen = None;
        self.af_footer = None;
        if let Some(ov) = self.ctx_overlay.as_ref()
            && let Err(e) = ov.set_visible(false)
        {
            debug!(error = %e, "ctx hide failed");
        }
    }

    // ── Script dialogs (intercepted page alert/confirm/prompt/beforeunload) ──

    /// Centered overlay rect for the dialog card (460x340 in the content
    /// slot, clamped into the window). Same coordinate space as the
    /// context menu rect.
    fn dlg_overlay_rect(&self) -> Option<wry::Rect> {
        let window = self.window.as_ref()?;
        let scale = window.scale_factor();
        let (win_w, win_h) = self.window_logical()?;
        let content = self.content_rect()?;
        let (w, h) = (460.0, 340.0);
        let cx = (content.x + (content.w - w) / 2.0).clamp(0.0, (win_w - w).max(0.0));
        let cy = (content.y + (content.h - h) / 2.0).clamp(0.0, (win_h - h).max(0.0));
        ferrite_tabs::logical_to_wry_rect(LogicalRect::new(cx, cy, w, h), scale)
    }

    /// Show the active tab dialog head in the overlay (lazy-create, center,
    /// focus, push). Attribution is IDN-safe, Beforeunload text is fixed.
    /// `count_repeat` arms suppression only for newly arrived heads.
    fn show_dialog_overlay(&mut self, count_repeat: bool) {
        use ferrite_tabs::{DialogKind, origin_key};
        let Some(active) = self.app.tabs.active_id() else {
            return;
        };
        let Some(head) = self.dialogs.head(active).cloned() else {
            return;
        };
        let window = self.window.clone();
        let bounds = self.dlg_overlay_rect();
        let (Some(window), Some(bounds)) = (window.as_ref(), bounds) else {
            return;
        };
        if self.dlg_overlay.is_none() {
            match self
                .app
                .tabs
                .factory()
                .create(window, "ferrite://dialog", Some(bounds))
            {
                Ok(h) => self.dlg_overlay = Some(h),
                Err(e) => {
                    debug!(error = %e, "dialog overlay creation failed");
                    return;
                }
            }
        }
        // Recomputed (not reused): `wry::Rect` is not `Copy`.
        let bounds = self.dlg_overlay_rect();
        let ov = self.dlg_overlay.clone();
        let (Some(ov), Some(bounds)) = (ov.as_ref(), bounds) else {
            return;
        };
        if let Err(e) = ov.set_bounds(bounds) {
            debug!(error = %e, "dialog bounds failed");
        }
        if let Err(e) = ov.set_visible(true) {
            debug!(error = %e, "dialog show failed");
        }
        if let Err(e) = ov.focus() {
            debug!(error = %e, "dialog focus failed");
        }
        let (sch, sub, dom, path) =
            ferrite_omnibox::display_parts(&head.uri).unwrap_or_else(|| {
                let flat: String = head.uri.chars().take(120).collect();
                (String::new(), String::new(), flat, String::new())
            });
        let show_suppress = head.kind != DialogKind::BeforeUnload
            && count_repeat
            && self
                .dialogs
                .record_shown(&origin_key(&head.uri), Instant::now());
        let frame = serde_json::json!({
            "seq": head.seq,
            "kind": head.kind.as_str(),
            "origin": {"sch": sch, "sub": sub, "dom": dom, "path": path},
            "uri": head.uri,
            "message": head.display_text(),
            "default_text": head.default_text,
            "show_suppress": show_suppress,
        });
        let json = serde_json::to_string(&frame)
            .unwrap_or_default()
            .replace("</", "<\\/");
        // Track the frame only once the page actually received it. A
        // failed push leaves no phantom dialog behind (and focus, already
        // moved in, goes back to the tab content).
        if let Err(e) = ov.evaluate_script(&format!("__ferriteDialog({json})")) {
            debug!(error = %e, "dialog push failed");
            self.hide_dialog_overlay();
            return;
        }
        self.dlg_shown = Some((active, head.seq));
    }

    /// Hide the dialog overlay and return keyboard focus to the active
    /// tab content (the overlay had it).
    fn hide_dialog_overlay(&mut self) {
        self.dlg_shown = None;
        if let Some(ov) = self.dlg_overlay.as_ref()
            && let Err(e) = ov.set_visible(false)
        {
            debug!(error = %e, "dialog hide failed");
        }
        if let Some(tab) = self.app.tabs.active_tab()
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.focus()
        {
            debug!(error = %e, "dialog focus-return failed");
        }
    }

    /// Keep the overlay in sync with dialog state: the active tab head
    /// shows, anything else hides. Dialog events and tab switches call this.
    fn sync_dialog_overlay(&mut self) {
        let want = self
            .app
            .tabs
            .active_id()
            .and_then(|id| self.dialogs.head(id).map(|h| (id, h.seq)));
        if want == self.dlg_shown {
            return;
        }
        // A reshow of the same frame (tab switch away and back) must not
        // re-arm the repeat counter. No new dialog arrived.
        let count_repeat = self
            .dlg_shown
            .is_none_or(|(_, seq)| want.is_some_and(|(_, wseq)| wseq != seq));
        if want.is_some() {
            self.show_dialog_overlay(count_repeat);
        } else {
            self.hide_dialog_overlay();
        }
    }

    /// Drop tab dialogs on close, discard, or shell-initiated navigation.
    /// Outstanding deferrals complete as cancelled, the queue clears.
    fn clear_tab_dialogs(&mut self, tab_id: Uuid) {
        #[cfg(target_os = "windows")]
        let mut cancelled = 0;
        #[cfg(target_os = "windows")]
        if let Some(sink) = self.dialog_sink.as_ref() {
            cancelled = sink.cancel_tab(tab_id);
            if cancelled > 0 {
                debug!(tab_id = %tab_id, cancelled, "cancelled tab dialogs");
            }
        }
        let dropped = self.dialogs.drop_tab(tab_id);
        #[cfg(target_os = "windows")]
        if cancelled == 0 && dropped.is_empty() {
            return;
        }
        #[cfg(not(target_os = "windows"))]
        if dropped.is_empty() {
            return;
        }
        self.sync_dialog_overlay();
        self.push_chrome(true);
    }

    /// Intercepted dialog arrived (the glue holds the deferral under
    /// `seq`). Suppressed origins and queue overflow complete immediately
    /// as cancelled. Active tab shows, background tabs badge.
    #[cfg(target_os = "windows")]
    fn on_dialog_opened(&mut self, ev: ferrite_tabs::DialogOpened) {
        use ferrite_tabs::DialogPush;
        // Dead-tab events (closed between interception and drain, or the
        // overlay's own random id) complete immediately. There is no tab
        // to show them on and no queue that could drain them.
        if self.app.tabs.get_tab(ev.req.tab_id).is_none() {
            if let Some(sink) = self.dialog_sink.as_ref() {
                sink.complete(ev.req.seq, false, None);
            }
            return;
        }
        let seq = ev.req.seq;
        match self.dialogs.push(ev.req) {
            DialogPush::AutoCancelled => {
                if let Some(sink) = self.dialog_sink.as_ref() {
                    sink.complete(seq, false, None);
                }
            }
            DialogPush::Shown | DialogPush::Queued => {
                self.sync_dialog_overlay();
                self.push_chrome(true);
            }
        }
    }

    /// Overlay response for the shown frame. Applies only when `seq`
    /// matches the visible head. Stale or forged responses are ignored
    /// (replays can only echo the open frame, seq is random per frame).
    fn dialog_respond(&mut self, seq: u64, _accepted: bool, _text: String, suppress: bool) {
        #[cfg(target_os = "windows")]
        use ferrite_tabs::DialogKind;
        use ferrite_tabs::origin_key;
        let Some((tab_id, shown)) = self.dlg_shown else {
            debug!("dialog response with no shown frame ignored");
            return;
        };
        if seq != shown {
            debug!("stale dialog response ignored");
            self.sync_dialog_overlay();
            self.push_chrome(true);
            return;
        }
        let Some(head) = self.dialogs.head(tab_id).cloned().filter(|h| h.seq == seq) else {
            debug!("dialog response for missing head ignored");
            self.sync_dialog_overlay();
            self.push_chrome(true);
            return;
        };
        if suppress {
            let origin = origin_key(&head.uri);
            self.dialogs.set_suppressed(&origin, true);
            #[cfg(target_os = "windows")]
            if let Some(sink) = self.dialog_sink.as_ref() {
                sink.set_suppressed(&origin, true);
            }
        }
        self.dialogs.pop_head(tab_id);
        // Prompt text travels only for accepted prompts. Every other
        // kind ignores response text (there is nowhere for it to go).
        #[cfg(target_os = "windows")]
        let result_text = if _accepted && head.kind == DialogKind::Prompt {
            Some(
                _text
                    .chars()
                    .take(ferrite_tabs::DIALOG_MESSAGE_CAP)
                    .collect::<String>(),
            )
        } else {
            None
        };
        #[cfg(target_os = "windows")]
        if let Some(sink) = self.dialog_sink.as_ref()
            && !sink.complete(seq, _accepted, result_text.as_deref())
        {
            debug!("dialog deferral already gone");
        }
        self.sync_dialog_overlay();
        self.push_chrome(true);
    }

    fn autofill_store(&self) -> Option<ferrite_credentials::AutofillStore> {
        self.vault
            .as_ref()
            .map(|v| ferrite_credentials::AutofillStore::new(v.clone()))
    }

    /// Fill rows for a right-click in a detected address or payment field.
    /// Explicit gesture on a visible field, so the menu doubles as the
    /// picker. `cc-csc` never maps.
    fn fill_rows(&mut self, p: &CtxPayload) -> Vec<serde_json::Value> {
        self.fill_options.clear();
        let hit = self.fill_hit(p.tab, p.x, p.y);
        let Some(f) = hit else {
            return Vec::new();
        };
        self.fill_rows_for(p.tab, &f)
    }

    /// Geometry half of [`Self::fill_rows`]: the address or payment field
    /// under a viewport point, if any.
    fn fill_hit(&self, tab: Uuid, x: f64, y: f64) -> Option<ScanField> {
        self.scan_cache.get(&tab).and_then(|fs| {
            fs.iter()
                // Password/TOTP fields never offer fills (kind gate before
                // geometry. The ac mapping below decides parts).
                .filter(|f| f.kind == "address" || f.kind == "payment")
                .find(|f| {
                    x >= f.x - 2.0 && x <= f.x + f.w + 2.0 && y >= f.y - 2.0 && y <= f.y + f.h + 2.0
                })
                .cloned()
        })
    }

    /// Stable id preferred. Name fallback is Debug-quoted (page-controlled
    /// text cannot break out of the selector). Unidentifiable fields get
    /// no rows rather than a guessed target.
    fn field_selector(f: &ScanField) -> Option<String> {
        if !f.id.is_empty() {
            Some(format!("#{}", f.id))
        } else if !f.name.is_empty() {
            Some(format!("[name={:?}]", f.name))
        } else {
            None
        }
    }

    /// Address and card rows for a known field (shared by the right-click menu
    /// and the focus dropdown, same records, same single-evaluate fill).
    fn fill_rows_for(&mut self, tab: Uuid, f: &ScanField) -> Vec<serde_json::Value> {
        let Some((kind, comp)) = ferrite_credentials::ac_component(&f.ac) else {
            return Vec::new();
        };
        if matches!(kind, ferrite_credentials::AutofillKind::Address) && !self.autofill_address {
            return Vec::new();
        }
        if matches!(kind, ferrite_credentials::AutofillKind::Card) && !self.autofill_payment {
            return Vec::new();
        }
        let Some(selector) = Self::field_selector(f) else {
            return Vec::new();
        };
        let (Some(store), Some(mut key)) = (self.autofill_store(), Self::cred_key()) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for rec in store.list(kind, &key) {
            let label = match kind {
                ferrite_credentials::AutofillKind::Address => rec.label.clone(),
                ferrite_credentials::AutofillKind::Card => {
                    let last4 = serde_json::from_str::<serde_json::Value>(&rec.fields_json)
                        .ok()
                        .and_then(|v| v.get("pan")?.as_str().map(|s| s.to_owned()))
                        .map(|pan| {
                            let n = pan.len();
                            if n > 4 { pan[n - 4..].to_owned() } else { pan }
                        })
                        .unwrap_or_default();
                    format!("{} ••{}", rec.label, last4)
                }
            };
            self.fill_options.push(PendingFill {
                record_id: rec.id.clone(),
                comp: comp.to_owned(),
                selector: selector.clone(),
                tab,
            });
            let idx = self.fill_options.len() - 1;
            rows.push(serde_json::json!({
                "id": format!("ctx-fill:{idx}"),
                "label": format!("Fill {label}"),
            }));
            if rows.len() >= 8 {
                break;
            }
        }
        key.zeroize();
        rows
    }

    /// Focus dropdown for a focused field (overlay webview, page, runner).
    /// Exactly one source per field, never a blend: vault logins (generator
    /// on signup), profile cards and addresses, per-origin form history.
    fn af_dropdown(&mut self, tab: Uuid, f: &ScanField) {
        self.fill_options.clear();
        self.cred_fills.clear();
        self.form_fills.clear();
        self.af_gen = None;
        self.af_footer = None;
        let origin = self
            .app
            .tabs
            .get_tab(tab)
            .map(|t| t.url.clone())
            .and_then(|u| ferrite_tabs::origin_of(&u));
        let mut items = match f.kind.as_str() {
            "password" | "username" => self.af_cred_rows(tab, f, origin),
            "address" | "payment" => {
                let rows = self.fill_rows_for(tab, f);
                if !rows.is_empty() {
                    self.af_footer = Some("ferrite://autofill".to_owned());
                }
                rows
            }
            // TOTP codes fill from the vault pair (username labels the row).
            "totp" => self.af_totp_rows(tab, f, origin),
            _ => self.af_form_rows(tab, f, origin),
        };
        if items.is_empty() {
            return;
        }
        if let Some(url) = self.af_footer.clone() {
            let label = if url.contains("passwords") {
                "Manage passwords..."
            } else {
                "Manage addresses..."
            };
            items.push(serde_json::json!({"id": "af-manage", "label": label}));
        }
        // Below the field, flipped above it when the bottom would run
        // off-screen. Width follows the field, clamped.
        let w = f.w.clamp(250.0, 420.0);
        let h = items.len() as f64 * 31.0 + 16.0;
        let y = match (self.content_rect(), self.window_logical()) {
            (Some(slot), Some((_, win_h))) => {
                flip_y(
                    slot.y + f.y + f.h + 4.0,
                    slot.y + f.y - 4.0,
                    h,
                    slot.y,
                    win_h,
                ) - slot.y
            }
            _ => f.y + f.h + 4.0,
        };
        // Field keeps focus (type-ahead + caret survive). Highlight is
        // visual-only on open, keyboard arrives via forwarded posts.
        self.af_open = true;
        self.af_filter = String::new();
        self.af_hl = Some(0);
        self.overlay_show(f.x, y, w, h, &items, false, Some(0));
        self.af_items = items;
        self.af_set_flag(tab, true);
        self.ctx_menu = Some(CtxPayload {
            tab,
            x: f.x,
            y: f.y,
            ..Default::default()
        });
    }

    /// Credential rows for a focused login field. Signup shapes
    /// (`new-password` or a second password field in the scan) flip to
    /// the generator row instead of saved logins.
    fn af_cred_rows(
        &mut self,
        tab: Uuid,
        f: &ScanField,
        origin: Option<String>,
    ) -> Vec<serde_json::Value> {
        let Some(origin) = origin else {
            return Vec::new();
        };
        let Some(store) = self.cred_store() else {
            return Vec::new();
        };
        if Self::cred_key().is_none() {
            return Vec::new();
        }
        let self_sel = Self::field_selector(f);
        let sib = |kind: &str| {
            self.scan_cache.get(&tab).and_then(|fs| {
                fs.iter()
                    .filter(|x| x.kind == kind)
                    .find_map(Self::field_selector)
            })
        };
        let pass_sel = if f.kind == "password" {
            self_sel.clone()
        } else {
            sib("password")
        };
        let user_sel = if f.kind == "username" {
            self_sel.clone()
        } else {
            sib("username")
        };
        let signup = f.ac == "new-password"
            || self
                .scan_cache
                .get(&tab)
                .map(|fs| fs.iter().filter(|x| x.kind == "password").count())
                .unwrap_or(0)
                >= 2;
        let creds = store.get_for_origin(&origin);
        // Signup, or a login with nothing saved: offer generation (needs
        // a password target). Otherwise the saved logins, pair-filled.
        // ponytail: username-only fill when no password field resolves
        // is skipped. Half a login is worse than none. The focused
        // password field is the common shape and always resolves.
        if signup || creds.is_empty() {
            if let Some(sel) = pass_sel {
                self.af_gen = Some((tab, sel));
                self.af_footer = Some("ferrite://passwords".to_owned());
                return vec![serde_json::json!({
                    "id": "af-gen",
                    "label": "Suggest a strong password",
                })];
            }
            return Vec::new();
        }
        let Some(pass_sel) = pass_sel else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for rec in creds.iter().take(5) {
            self.cred_fills.push(PendingCredFill {
                cred_id: rec.id.clone(),
                origin: origin.clone(),
                user_selector: user_sel.clone(),
                pass_selector: pass_sel.clone(),
                tab,
            });
            let idx = self.cred_fills.len() - 1;
            rows.push(serde_json::json!({
                "id": format!("af-cred:{idx}"),
                "label": format!("Login as {}", rec.username),
            }));
        }
        if !rows.is_empty() {
            self.af_footer = Some("ferrite://passwords".to_owned());
        }
        rows
    }

    /// TOTP code rows for a one-time-code field: vault pairs under the
    /// tab origin, labeled by username. The code generates at pick time
    /// (runner), never stored in the row.
    fn af_totp_rows(
        &mut self,
        tab: Uuid,
        f: &ScanField,
        origin: Option<String>,
    ) -> Vec<serde_json::Value> {
        let (Some(origin), Some(store)) = (origin, self.cred_store()) else {
            return Vec::new();
        };
        if Self::cred_key().is_none() {
            return Vec::new();
        }
        let Some(selector) = Self::field_selector(f) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for (rec, _sec) in store.totp_for_origin(&origin).iter().take(5) {
            self.totp_fills.push(PendingTotpFill {
                cred_id: rec.id.clone(),
                origin: origin.clone(),
                selector: selector.clone(),
                tab,
            });
            let idx = self.totp_fills.len() - 1;
            rows.push(serde_json::json!({
                "id": format!("af-totp:{idx}"),
                "label": format!("Fill code for {}", rec.username),
            }));
        }
        if !rows.is_empty() {
            self.af_footer = Some("ferrite://passwords".to_owned());
        }
        rows
    }

    /// Form-history rows for an unrecognized field: past values for this
    /// origin plus field key, most-used first. No footer (no manager page).
    fn af_form_rows(
        &mut self,
        tab: Uuid,
        f: &ScanField,
        origin: Option<String>,
    ) -> Vec<serde_json::Value> {
        if !self.autofill_form {
            return Vec::new();
        }
        let (Some(origin), Some(key), Some(selector), Some(vault)) = (
            origin,
            Self::form_field_key(&f.ac, &f.name, &f.id),
            Self::field_selector(f),
            self.vault.as_ref(),
        ) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for value in vault.form_values(&origin, &key, 5) {
            self.form_fills.push(PendingFormFill {
                selector: selector.clone(),
                value: value.clone(),
                tab,
            });
            let idx = self.form_fills.len() - 1;
            let shown: String = value.chars().take(60).collect();
            rows.push(serde_json::json!({
                "id": format!("af-form:{idx}"),
                "label": shown,
            }));
        }
        rows
    }

    /// Field key for form history: autocomplete attr, else name, else id.
    fn form_field_key(ac: &str, name: &str, id: &str) -> Option<String> {
        [ac, name, id]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .map(|s| s.to_owned())
    }

    /// Sensitive field-key fragments: never recorded to form history.
    /// Over-exclusion is deliberate (a missed "accesscode" beats a stored
    /// SSN). The credential and card stores are the right homes.
    fn sensitive_field_key(key: &str) -> bool {
        const FRAGS: &[&str] = &[
            "pass", "ssn", "social", "card", "cc", "cvc", "cvv", "pin", "secret", "token", "auth",
            "bank", "routing", "account", "iban", "expir", "csc", "cvd", "cvn",
        ];
        let k = key.to_lowercase();
        FRAGS.iter().any(|frag| k.contains(frag))
    }

    /// One field component out of a decrypted record blob.
    fn fill_value(fields_json: &str, comp: &str) -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(fields_json).ok()?;
        let s = v.get(comp)?.as_str()?;
        if s.trim().is_empty() {
            return None;
        }
        Some(s.to_owned())
    }

    /// Tell a content page whether the type-ahead channel is open (field
    /// keydown and input posts only fire while set, one evaluate per open
    /// and close, no stream). Best-effort: a gone webview just misses it.
    fn af_set_flag(&self, tab: Uuid, open: bool) {
        if let Some(t) = self.app.tabs.get_tab(tab)
            && let Some(h) = t.webview.as_ref()
            && let Err(e) = h.evaluate_script(if open {
                "window.__ferriteAfOpen=true;"
            } else {
                "window.__ferriteAfOpen=false;"
            })
        {
            debug!(error = %e, "af flag failed");
        }
    }

    /// Visible dropdown rows under the type-ahead filter (case-insensitive
    /// label match, the footer row pins regardless). Pure for tests.
    fn af_visible(items: &[serde_json::Value], filter: &str) -> Vec<serde_json::Value> {
        let needle = filter.to_lowercase();
        items
            .iter()
            .filter(|it| {
                it.get("id").and_then(|s| s.as_str()) == Some("af-manage")
                    || it
                        .get("label")
                        .and_then(|s| s.as_str())
                        .map(|l| l.to_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// Re-push the filtered rows plus highlight without moving bounds or
    /// focus (the field keeps both while typing).
    fn af_repush(&mut self) {
        let visible = Self::af_visible(&self.af_items, &self.af_filter);
        let hl = self.af_hl.unwrap_or(0);
        let json = serde_json::to_string(&visible)
            .unwrap_or_else(|_| "[]".to_owned())
            .replace("</", "<\\/");
        if let Some(ov) = self.ctx_overlay.clone()
            && let Err(e) = ov.evaluate_script(&format!("__ferriteCtxMenu({json},{hl})"))
        {
            debug!(error = %e, "af repush failed");
        }
    }

    /// Cookies for one URL from the shared tab store (name and value pairs
    /// only). Lets worker refetches ride the tab session instead of
    /// arriving logged-out.
    fn tab_cookies(&self, tab: Uuid, url: &str) -> Vec<(String, String)> {
        self.app
            .tabs
            .get_tab(tab)
            .and_then(|t| t.webview.as_ref())
            .and_then(|h| h.cookies_for_url(url).ok())
            .unwrap_or_default()
    }

    /// Guest forced-download dir: the temp dir while ephemeral (native
    /// pickers are skipped everywhere when this is Some). `None` in user
    /// mode: dialogs and Downloads as usual.
    fn forced_download_dir(&self) -> Option<std::path::PathBuf> {
        self.guest_profile
            .as_ref()
            .map(crate::profiles::guest_downloads_dir)
    }

    /// Save-As dispatcher: http(s) refetches with tab cookies. Data URLs
    /// decode locally, blob URLs rescue through the page. Anything else
    /// is refused. Page-controlled schemes stay out of the fetcher.
    /// Guest forces the temp dir (no picker by design, see forced dir).
    fn save_as(&mut self, tab: Uuid, url: &str) {
        let force = self.forced_download_dir();
        if saveable_url(url) {
            let cookies = self.tab_cookies(tab, url);
            crate::filedlg::spawn_save_url(
                url.to_owned(),
                cookies,
                self.save_last_dir.clone(),
                force,
                self.notice_tx.clone(),
            );
            return;
        }
        let lower = url.trim().to_lowercase();
        if lower.starts_with("data:") {
            crate::filedlg::spawn_save_data_url(
                url.to_owned(),
                self.save_last_dir.clone(),
                force,
                self.notice_tx.clone(),
            );
        } else if lower.starts_with("blob:") {
            self.blob_save_kickoff(tab, url);
        } else {
            debug!("save-as refused: non-fetchable scheme");
        }
    }

    /// Start a blob rescue: page fetches its own object URL and streams
    /// base64 slices back (36864 raw bytes per post, under the IPC cap).
    fn blob_save_kickoff(&mut self, tab: Uuid, url: &str) {
        self.blob_seq += 1;
        let id = format!("b{}", self.blob_seq);
        self.blob_asm.insert(
            (tab, id.clone()),
            (Vec::new(), 0, std::time::Instant::now()),
        );
        let js = format!("__ferriteSaveBlob({url:?},{id:?})");
        let ok = self
            .app
            .tabs
            .get_tab(tab)
            .and_then(|t| t.webview.as_ref())
            .and_then(|h| h.evaluate_script(&js).ok())
            .is_some();
        if !ok {
            self.blob_asm.remove(&(tab, id));
            self.notify("Couldn't save file: page unreachable".to_owned());
        }
    }
    /// Window-relative, so no screen-coordinate translation is needed.
    /// Just the content-slot origin plus a clamp into the window.
    /// Flip lives with the callers (`flip_y`), not here.
    fn ctx_overlay_rect(&self, x: f64, y: f64, w: f64, h: f64) -> Option<wry::Rect> {
        let window = self.window.as_ref()?;
        let scale = window.scale_factor();
        let (win_w, win_h) = self.window_logical()?;
        let top = self.content_rect()?.y;
        let cx = x.clamp(0.0, (win_w - w).max(0.0));
        let cy = (top + y).clamp(0.0, (win_h - h).max(0.0));
        ferrite_tabs::logical_to_wry_rect(LogicalRect::new(cx, cy, w, h), scale)
    }

    /// Menu rows for a content right-click. Only actions with live backends
    /// appear. No window, print, or save-as rows (those backends do not exist).
    fn ctx_items(&self, p: &CtxPayload) -> Vec<serde_json::Value> {
        let mut items = Vec::new();
        let mut row = |id: &str, label: String| {
            items.push(serde_json::json!({"id": id, "label": label}));
        };
        if !p.href.is_empty() {
            row("ctx-open-tab", "Open link in new tab".to_owned());
            row("ctx-copy-link", "Copy link address".to_owned());
            row("ctx-share-link", "Share link...".to_owned());
            row("ctx-bookmark-link", "Bookmark this link".to_owned());
            row("ctx-save-link", "Save link as...".to_owned());
            if !host_slice(&p.href).is_empty() {
                row("ctx-visits", "View visits from this site".to_owned());
            }
            row("ctx-view-source", "View Page Source".to_owned());
        } else if !p.src.is_empty() {
            row("ctx-open-img", "Open image in new tab".to_owned());
            row("ctx-copy-img", "Copy image address".to_owned());
            row("ctx-save-img", "Save image as...".to_owned());
            row("ctx-view-source", "View Page Source".to_owned());
        } else if !p.selection.trim().is_empty() {
            // Misspelling kind: one word selected inside an editable field
            // belongs to the native text-edit menu (suggestions, Add to
            // Dictionary, Ignore). Multi-word selections keep Ferrite's
            // search + copy rows.
            if p.editable && ferrite_privacy::spelling_single_word(&p.selection).is_some() {
                return Vec::new();
            }
            let q: String = p.selection.chars().take(40).collect();
            row("ctx-search", format!("Search the web for “{q}”"));
            row("ctx-copy-sel", "Copy".to_owned());
            // Link-to-text needs a real page URL (fragments ride http(s)
            // only) and a non-trivial selection; single words copy fine.
            if self.translate_enabled {
                row("ctx-translate-sel", "Translate selection".to_owned());
            }
            if Self::fragment_link(
                &self
                    .app
                    .tabs
                    .get_tab(p.tab)
                    .map(|t| t.url.clone())
                    .unwrap_or_default(),
                &p.selection,
            )
            .is_some()
            {
                row("ctx-fragment-link", "Copy link to selected text".to_owned());
            }
        } else {
            // Caret inside an editable field with no selection: the JS
            // reporter already stands down (native menu shows). This is
            // the backstop for forged or racing posts.
            if p.editable {
                return Vec::new();
            }
            let (back, fwd) = match self.app.tabs.active_tab() {
                Some(t) => (t.can_go_back(), t.can_go_forward()),
                None => (false, false),
            };
            if back {
                row("ctx-back", "Back".to_owned());
            }
            if fwd {
                row("ctx-forward", "Forward".to_owned());
            }
            row("ctx-reload", "Reload".to_owned());
            row("ctx-share-page", "Share this page...".to_owned());
            if matches!(self.fullscreen, FullscreenMode::Borderless) {
                row("ctx-exit-fullscreen", "Exit fullscreen".to_owned());
            }
            row("ctx-inspect", "Inspect".to_owned());
            row("ctx-view-source", "View Page Source".to_owned());
            row("ctx-print", "Print".to_owned());
            if self.translate_enabled {
                row("ctx-translate-page", "Translate page".to_owned());
            }
        }
        items
    }

    /// Show the content context menu: size from the row count, clamp into
    /// the window, push rows into the lazily-created overlay (tabsearch
    /// precedent, chrome DOM cannot overlay content in z-order).
    fn show_ctx_menu(&mut self, p: CtxPayload) {
        let mut items = self.fill_rows(&p);
        items.extend(self.ctx_items(&p));
        if items.is_empty() {
            return;
        }
        // 31px rows + padding. Width fixed. Flips above the cursor
        // near the bottom edge (same helper as the field anchor).
        let (w, h) = (250.0, items.len() as f64 * 31.0 + 16.0);
        let y = match (self.content_rect(), self.window_logical()) {
            (Some(slot), Some((_, win_h))) => {
                flip_y(slot.y + p.y, slot.y + p.y, h, slot.y, win_h) - slot.y
            }
            _ => p.y,
        };
        self.overlay_show(p.x, y, w, h, &items, true, None);
        let tab = p.tab;
        self.ctx_menu = Some(p);
        // A context menu replaces any dropdown: close the type-ahead
        // channel with it (stale key posts are ignored shell-side anyway,
        // but the flag must not hijack Enter on a menu-less field).
        self.af_open = false;
        self.af_set_flag(tab, false);
    }

    /// Overlay mechanics shared by the context menu and the focus dropdown:
    /// same webview, page, and runner, only rows and anchor differ. `focus`
    /// moves keyboard focus into the overlay, the dropdown keeps field focus.
    fn overlay_show(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        items: &[serde_json::Value],
        focus: bool,
        hl: Option<usize>,
    ) {
        let window = self.window.clone();
        let bounds = self.ctx_overlay_rect(x, y, w, h);
        let (Some(window), Some(bounds)) = (window.as_ref(), bounds) else {
            return;
        };
        if self.ctx_overlay.is_none() {
            match self
                .app
                .tabs
                .factory()
                .create(window, "ferrite://ctxmenu", Some(bounds))
            {
                Ok(h) => self.ctx_overlay = Some(h),
                Err(e) => {
                    debug!(error = %e, "ctx overlay creation failed");
                    return;
                }
            }
        }
        // Recomputed (not reused): `wry::Rect` is not `Copy`.
        let bounds = self.ctx_overlay_rect(x, y, w, h);
        let ov = self.ctx_overlay.clone();
        if let (Some(ov), Some(bounds)) = (ov.as_ref(), bounds) {
            if let Err(e) = ov.set_bounds(bounds) {
                debug!(error = %e, "ctx bounds failed");
            }
            if let Err(e) = ov.set_visible(true) {
                debug!(error = %e, "ctx show failed");
            }
            if focus && let Err(e) = ov.focus() {
                debug!(error = %e, "ctx focus failed");
            }
            let json = serde_json::to_string(&items)
                .unwrap_or_else(|_| "[]".to_owned())
                .replace("</", "<\\/");
            let hl_js = hl.map(|i| i.to_string()).unwrap_or("undefined".to_owned());
            if let Err(e) = ov.evaluate_script(&format!("__ferriteCtxMenu({json},{hl_js})")) {
                debug!(error = %e, "ctx push failed");
            }
        }
    }

    /// Run one overlay menu action against the stored payload, then hide and
    /// refocus the page. Unknown ids are ignored: a page guessing ids on
    /// this shared channel can only replay the open menu actions.
    fn activate_ctx(&mut self, id: &str) {
        let Some(p) = self.ctx_menu.clone() else {
            return;
        };
        // Snapshot offers BEFORE hide: hide clears the option vecs, so
        // reading them after would no-op every fill (right-click Fill
        // included. This ordering is the fix, not a refactor).
        let fill = self.fill_options.clone();
        let cred = self.cred_fills.clone();
        let form = self.form_fills.clone();
        let totp = self.totp_fills.clone();
        let gen_target = self.af_gen.clone();
        let footer = self.af_footer.clone();
        self.hide_ctx_menu();
        match id {
            "ctx-open-tab" if !p.href.is_empty() => self.open_tab(p.href),
            "ctx-copy-link" => {
                self.copy_with_toast(&p.href, "Link");
            }
            "ctx-share-link" if !p.href.is_empty() => {
                let (tab, href) = (p.tab, p.href.clone());
                self.share_tab(tab, &href, "");
            }
            "ctx-share-page" => self.share_active(),
            "ctx-visits" => {
                // Filtered list first (the safe companion to delete-site):
                // host substring matches are exactly what the filter does.
                let host = host_slice(&p.href).to_owned();
                if !host.is_empty() {
                    self.open_tab(format!("ferrite://history?site={}", encode_query(&host)));
                }
            }
            "ctx-bookmark-link" => {
                if !p.href.is_empty() {
                    self.save_bookmark(p.href.clone(), String::new(), None);
                    self.push_chrome(true);
                    self.push_panel_section();
                }
            }
            "ctx-open-img" if !p.src.is_empty() => self.open_tab(p.src),
            "ctx-copy-img" => {
                self.copy_with_toast(&p.src, "Image link");
            }
            // Save Link/Image As: fetch + native save dialog, fully on a
            // worker (fetch, dialog, write. The loop only passed the URL).
            // http(s) refetches with tab cookies. Data: decodes locally;
            // blob: rescues through the page (object URLs never leave it).
            "ctx-save-link" if !p.href.is_empty() => self.save_as(p.tab, &p.href),
            "ctx-save-img" if !p.src.is_empty() => self.save_as(p.tab, &p.src),
            "ctx-search" => {
                // Deliberate single action (not ambient traffic): may leave.
                let q = p.selection.trim().to_owned();
                if !q.is_empty() {
                    match self.app.omnibox.url_for_input(&q) {
                        Ok(u) => self.open_tab(u),
                        Err(e) => debug!(error = %e, "ctx search classify failed"),
                    }
                }
            }
            "ctx-copy-sel" => {
                self.copy_with_toast(p.selection.trim(), "Selection");
            }
            "ctx-fragment-link" => {
                let url = self
                    .app
                    .tabs
                    .get_tab(p.tab)
                    .map(|t| t.url.clone())
                    .unwrap_or_default();
                match Self::fragment_link(&url, &p.selection) {
                    Some(link) => {
                        self.copy_with_toast(&link, "Link");
                    }
                    None => self.notify("Couldn't link that selection.".to_owned()),
                }
            }
            "ctx-translate-sel" => {
                if self.translate_enabled {
                    self.run_translate(p.tab, ferrite_tabs::translate::TranslateMode::Selection);
                }
            }
            "ctx-translate-page" => {
                if self.translate_enabled {
                    self.run_translate(p.tab, ferrite_tabs::translate::TranslateMode::Page);
                }
            }
            "ctx-back" => {
                let mut moved = false;
                if let Some(tab) = self.app.tabs.active_tab_mut()
                    && let Some(url) = tab.go_back()
                {
                    if let Err(e) = tab.load_traversal(url) {
                        debug!(error = %e, "ctx back failed");
                    } else {
                        moved = true;
                    }
                }
                if moved {
                    if let Some(id) = self.app.tabs.active_id() {
                        self.clear_tab_dialogs(id);
                    }
                    self.find_reset();
                }
            }
            "ctx-forward" => {
                let mut moved = false;
                if let Some(tab) = self.app.tabs.active_tab_mut()
                    && let Some(url) = tab.go_forward()
                {
                    if let Err(e) = tab.load_traversal(url) {
                        debug!(error = %e, "ctx forward failed");
                    } else {
                        moved = true;
                    }
                }
                if moved {
                    if let Some(id) = self.app.tabs.active_id() {
                        self.clear_tab_dialogs(id);
                    }
                    self.find_reset();
                }
            }
            "ctx-reload" => {
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    if let Err(e) = tab.reload() {
                        debug!(error = %e, "ctx reload failed");
                    } else {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                        self.find_reset();
                    }
                }
            }
            "ctx-exit-fullscreen" => {
                if matches!(self.fullscreen, FullscreenMode::Borderless) {
                    let window = self.window.clone();
                    if let Some(window) = window.as_ref() {
                        self.toggle_fullscreen(window);
                    }
                }
            }
            "ctx-inspect" => self.open_devtools(),
            "ctx-print" => self.print_active(),
            "ctx-view-source" => {
                // Source of the link, image, or page itself (in that order).
                let target = if !p.href.is_empty() {
                    p.href.clone()
                } else if !p.src.is_empty() {
                    p.src.clone()
                } else {
                    self.app
                        .tabs
                        .get_tab(p.tab)
                        .map(|t| t.url.clone())
                        .unwrap_or_default()
                };
                if !target.is_empty() {
                    self.open_tab(format!("ferrite://viewsource?u={}", encode_query(&target)));
                }
            }
            id if id.starts_with("ctx-fill:") => {
                // Release path: the decrypted value goes straight into the
                // field via one evaluate (never parked in page JS). The tab
                // must still be active. No mis-fill after a switch.
                let idx: usize = id
                    .trim_start_matches("ctx-fill:")
                    .parse()
                    .unwrap_or(usize::MAX);
                let opt = fill.get(idx).cloned();
                if let Some(f) = opt
                    && Some(f.tab) == self.app.tabs.active_id()
                    && let (Some(store), Some(mut key)) = (self.autofill_store(), Self::cred_key())
                {
                    if let Some(rec) = store.get(&f.record_id, &key) {
                        if let Some(val) = Self::fill_value(&rec.fields_json, &f.comp) {
                            let js = ferrite_credentials::injection::injection_js(&f.selector, val);
                            if let Some(tab) = self.app.tabs.get_tab(f.tab)
                                && let Some(h) = tab.webview.as_ref()
                                && let Err(e) = h.evaluate_script(&js)
                            {
                                debug!(error = %e, "fill inject failed");
                            }
                        }
                    }
                    key.zeroize();
                }
            }
            id if id.starts_with("af-cred:") => {
                // Credential pair fill: username + password together, one
                // evaluate each, straight into the fields (never parked in
                // page JS). Tab must still be active.
                let idx: usize = id
                    .trim_start_matches("af-cred:")
                    .parse()
                    .unwrap_or(usize::MAX);
                let opt = cred.get(idx).cloned();
                if let Some(f) = opt
                    && Some(f.tab) == self.app.tabs.active_id()
                    && let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key())
                {
                    if let Some(rec) = store
                        .get_for_origin(&f.origin)
                        .into_iter()
                        .find(|r| r.id == f.cred_id)
                    {
                        let username = rec.username.clone();
                        if let Some(pw) = rec.decrypt_password(&key) {
                            let js =
                                ferrite_credentials::injection::injection_js(&f.pass_selector, pw);
                            if let Some(tab) = self.app.tabs.get_tab(f.tab)
                                && let Some(h) = tab.webview.as_ref()
                                && let Err(e) = h.evaluate_script(&js)
                            {
                                debug!(error = %e, "cred pass inject failed");
                            }
                        }
                        if let Some(sel) = f.user_selector {
                            let js = ferrite_credentials::injection::injection_js(&sel, username);
                            if let Some(tab) = self.app.tabs.get_tab(f.tab)
                                && let Some(h) = tab.webview.as_ref()
                                && let Err(e) = h.evaluate_script(&js)
                            {
                                debug!(error = %e, "cred user inject failed");
                            }
                        }
                    }
                    key.zeroize();
                }
            }
            id if id.starts_with("af-totp:") => {
                // TOTP code fill: generated at pick time (codes rotate,
                // a stored code would be stale). Straight into the field.
                let idx: usize = id
                    .trim_start_matches("af-totp:")
                    .parse()
                    .unwrap_or(usize::MAX);
                let opt = totp.get(idx).cloned();
                if let Some(f) = opt
                    && Some(f.tab) == self.app.tabs.active_id()
                    && let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key())
                {
                    if let Some(code) = store
                        .totp_for_origin(&f.origin)
                        .into_iter()
                        .find(|(r, _)| r.id == f.cred_id)
                        .and_then(|(_, s)| s.current_code(&key))
                    {
                        let js = ferrite_credentials::injection::injection_js(&f.selector, code);
                        if let Some(tab) = self.app.tabs.get_tab(f.tab)
                            && let Some(h) = tab.webview.as_ref()
                            && let Err(e) = h.evaluate_script(&js)
                        {
                            debug!(error = %e, "totp inject failed");
                        }
                    }
                    key.zeroize();
                }
            }
            id if id.starts_with("af-form:") => {
                // Form-history fill: plain recorded value into its field.
                let idx: usize = id
                    .trim_start_matches("af-form:")
                    .parse()
                    .unwrap_or(usize::MAX);
                let opt = form.get(idx).cloned();
                if let Some(f) = opt
                    && Some(f.tab) == self.app.tabs.active_id()
                    && let Some(tab) = self.app.tabs.get_tab(f.tab)
                    && let Some(h) = tab.webview.as_ref()
                {
                    let js = ferrite_credentials::injection::injection_js(&f.selector, f.value);
                    if let Err(e) = h.evaluate_script(&js) {
                        debug!(error = %e, "form fill inject failed");
                    }
                }
            }
            "af-gen" => {
                // Generated password into the pending target (signup /
                // change-password field). Not saved anywhere. The site
                // stores it on submit. The manager holds saved logins.
                if let Some((tab_id, sel)) = gen_target
                    && Some(tab_id) == self.app.tabs.active_id()
                    && let Some(tab) = self.app.tabs.get_tab(tab_id)
                    && let Some(h) = tab.webview.as_ref()
                {
                    let pw = ferrite_credentials::generate_password(
                        self.gen_length,
                        true,
                        true,
                        true,
                        true,
                    );
                    let js = ferrite_credentials::injection::injection_js(&sel, pw.value);
                    if let Err(e) = h.evaluate_script(&js) {
                        debug!(error = %e, "gen inject failed");
                    }
                }
            }
            "af-manage" => {
                if let Some(url) = footer {
                    self.open_tab(url);
                }
            }
            _ => debug!(id, "unknown ctx item ignored"),
        }
        // Return focus to the page (the overlay had it).
        if let Some(tab) = self.app.tabs.active_tab()
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.focus()
        {
            debug!(error = %e, "ctx refocus failed");
        }
    }

    /// Push the active section list into chrome via its hook. The panel
    /// refreshes on open, on section switch, and after any mutation that
    /// touches its data. Never on the steady-state push path.
    fn push_panel_section(&self) {
        if !self.panel_open {
            return;
        }
        let chrome = self.chrome.as_ref();
        match self.panel_section.as_str() {
            "bookmarks" => {
                let items: Vec<serde_json::Value> = match self.vault.as_ref() {
                    Some(v) => v
                        .list_bookmark_children(None)
                        .into_iter()
                        .take(100)
                        .map(|n| {
                            serde_json::json!({
                                "id": n.id, "title": n.title, "url": n.url,
                                "folder": n.is_folder,
                            })
                        })
                        .collect(),
                    None => Vec::new(),
                };
                self.push_list_to(chrome, "__ferritePanelBookmarks", &items);
            }
            "history" => {
                let items: Vec<serde_json::Value> =
                    self.history_json("").into_iter().take(30).collect();
                self.push_list_to(chrome, "__ferritePanelHistory", &items);
            }
            "reading" => self.push_reading_list(),
            _ => {}
        }
    }

    /// Reading list rows for the panel hook (unread first, newest first).
    fn push_reading_list(&self) {
        let items: Vec<serde_json::Value> = match self.vault.as_ref() {
            Some(v) => v
                .reading_list(100)
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id, "url": r.url, "title": r.title,
                        "added": r.added_at, "read": r.is_read,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        self.push_list_to(self.chrome.as_ref(), "__ferritePanelReading", &items);
    }

    /// Save every open tab into a fresh bar folder (one bookmark each).
    /// Tabs without usable URLs are skipped, never stored as junk rows.
    fn bookmark_all_tabs(&mut self) {
        let tabs: Vec<(String, String)> = self
            .app
            .tabs
            .iter()
            .filter(|t| !t.url.trim().is_empty() && !t.url.starts_with("ferrite://"))
            .map(|t| (t.url.clone(), t.title.clone()))
            .collect();
        if tabs.is_empty() {
            return;
        }
        let Some(vault) = self.vault.as_ref() else {
            debug!("bookmark-all ignored: vault unavailable");
            self.notify("Bookmarks need the vault available.".to_owned());
            return;
        };
        let name = format!("{} tabs", tabs.len());
        let folder = match vault.add_folder(&name, None) {
            Ok(id) => Some(id),
            Err(e) => {
                debug!(error = %e, "bookmark-all folder failed");
                None
            }
        };
        let mut saved = 0;
        for (url, title) in tabs {
            if self.save_bookmark(url, title, folder.clone()) {
                saved += 1;
            }
        }
        info!(saved, "bookmarked all tabs");
        self.maybe_autoreveal_bar();
    }

    /// Switch the active DESIGN pack: persist, swap the live chrome tokens,
    /// re-theme privileged pages without reload, and push (state carries
    /// pack list plus accent for the checkmarks). Never touches vault,
    /// session, history, or downloads: open documents keep their DOM;
    /// only a `<style id="ferrite-theme-live">` block is upserted in our
    /// own `ferrite://` pages, and the factory block (future loads) is
    /// swapped. Untrusted web content is never evaluated into.
    fn set_theme_pack(&mut self, pack: ThemePack) {
        if self.theme_pack == pack {
            return;
        }
        self.theme_pack = pack;
        persist_setting(
            "chrome_pack",
            serde_json::Value::String(pack.name().to_owned()),
        );
        self.apply_pack_live(pack);
        info!(pack = pack.name(), "theme pack switched");
        self.push_chrome(true);
    }

    /// Live side of a pack change without persistence: frame, factory block
    /// (future loads), chrome tokens, and open `ferrite://` documents.
    /// Shared by the toggle, settings import, and reset paths so all three
    /// paint identically. Never reloads, never drops tab state.
    fn apply_pack_live(&mut self, pack: ThemePack) {
        self.theme_pack = pack;
        if let Some(window) = self.window.as_ref() {
            let window: &Window = window;
            apply_frame_theme(window, pack);
        }
        // Future navigations: swap the shared factory block first so a
        // navigation racing the switch still lands on the new pack.
        let live_css = full_theme_css(&self.app.theme, pack);
        self.app.tabs.factory().set_theme_css(live_css.clone());
        let css = serde_json::to_string(pack.css()).unwrap_or_default();
        self.eval_chrome(&format!("__ferriteTheme({css})"));
        // Open documents: upsert the live style block in our own pages
        // only. Failures are per-tab and debug-logged; the switch itself
        // never fails, never reloads, never drops state.
        let js = ferrite_tabs::live_theme_script(&live_css);
        for t in self.app.tabs.iter() {
            if !t.url.starts_with("ferrite://") {
                continue;
            }
            if let Some(h) = t.webview.as_ref()
                && let Err(e) = h.evaluate_script(&js)
            {
                debug!(error = %e, "live theme push failed (tab keeps boot theme until reload)");
            }
        }
    }

    /// Smart reload shared by the toolbar button, `Ctrl+R`, and the menu:
    /// a loading tab stops instead (matches the Stop tooltip and the Esc
    /// behavior). Only a real reload kills the document, so only it resets
    /// find state and clears pending script dialogs.
    fn reload_or_stop_active(&mut self) {
        if let Some(tab) = self.app.tabs.active_tab_mut() {
            let loading = tab.is_loading;
            let r = if loading { tab.stop() } else { tab.reload() };
            if let Err(e) = r {
                debug!(error = %e, loading, "reload/stop failed");
            } else if !loading {
                let id = tab.id;
                self.clear_tab_dialogs(id);
                self.find_reset();
            }
        }
    }

    /// Open a tab, logging refusals (limit, invalid URL) instead of failing
    /// silently and leaving the user on a dead button. Explicit new tabs
    /// (not restores, not programmatic loads) take omnibox focus: typing
    /// must work the moment the tab exists, every time, not just at boot.
    fn open_tab(&mut self, url: String) {
        let focus_bar = url == "ferrite://newtab" || url.trim().is_empty();
        let window = self.window.clone();
        let bounds = self.content_wry();
        if let Some(window) = window.as_ref() {
            if let Err(e) = self.app.tabs.new_tab(Some(window), Some(url), bounds) {
                debug!(error = %e, "new tab refused");
            } else if focus_bar {
                self.eval_chrome("__ferriteFocusOmnibox()");
            }
        }
        self.layout_tabs();
    }

    /// Open a closed entry with its recorded state: history replaces
    /// wholesale (the open navigation itself must not append), scroll
    /// re-applies through the suppressed-commit path, pin restores.
    /// Returns the new tab id; `None` (tab limit, invalid URL, headless)
    /// keeps the entry on the stack for the next attempt instead of
    /// losing it.
    fn open_restored(&mut self, closed: &ClosedTab) -> Option<Uuid> {
        let window = self.window.clone();
        let bounds = self.content_wry();
        if let Some(window) = window.as_ref() {
            match self
                .app
                .tabs
                .new_tab(Some(window), Some(closed.url.clone()), bounds)
            {
                Ok(id) => {
                    // The new tab activates on open: graft recorded state
                    // before first paint. Guarded by id in case a future
                    // new_tab stops activating.
                    if self.app.tabs.active_tab().is_some_and(|t| t.id == id)
                        && let Some(tab) = self.app.tabs.active_tab_mut()
                    {
                        Self::graft_closed_state(tab, closed);
                    }
                    self.layout_tabs();
                    return Some(id);
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// Graft recorded state onto a freshly opened tab. Pure state copy
    /// for tests; wiring lives in `open_restored`.
    fn graft_closed_state(tab: &mut TabState, closed: &ClosedTab) {
        tab.history = closed.history.clone();
        tab.scroll_position = closed.scroll;
        tab.suppress_next_history = true;
        tab.title = closed.title.clone();
        if closed.pinned && !tab.is_pinned {
            tab.toggle_pinned();
        }
    }

    /// Open a new incognito window: a separate `--incognito` process with
    /// its own lock, temp engine profile, and memory stores. Fire-and-toast:
    /// spawn failure is the only local failure mode and it toasts.
    fn open_incognito_window(&mut self) {
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ferrite"));
        match std::process::Command::new(&exe).arg("--incognito").spawn() {
            Ok(_) => {
                self.notify("Incognito window opened.".to_owned());
            }
            Err(e) => {
                debug!(error = %e, "incognito spawn failed");
                self.notify("Couldn't open an incognito window.".to_owned());
            }
        }
        self.push_chrome(true);
    }

    /// Private tab: incognito engine store (never the profile disk cache),
    /// never persisted, never recorded. Refusals toast like every other
    /// user-invoked creation path (a silent no-op would read as broken).
    fn open_private_tab(&mut self, url: String) {
        let window = self.window.clone();
        let bounds = self.content_wry();
        if let Some(window) = window.as_ref() {
            if let Err(e) = self
                .app
                .tabs
                .new_private_tab(Some(window), Some(url), bounds)
            {
                debug!(error = %e, "new private tab refused");
                self.notify("Couldn't open a private tab.".to_owned());
            }
        }
        self.layout_tabs();
        self.push_chrome(true);
    }

    fn switch_to_index(&mut self, idx: usize) {
        let prev = self.app.tabs.active_id();
        let window = self.window.clone();
        if let Some(window) = window.as_ref() {
            let bounds = self.content_wry();
            if let Err(e) = self.app.tabs.switch_tab_with_window(window, idx, bounds) {
                debug!(error = %e, "tab switch failed");
            }
            self.app.sync_theme_from_tabs();
        }
        // Per-tab scoping: the bar never follows across tabs. Clear the
        // stale highlight where it was, then drop the flags.
        if self.find_open && self.app.tabs.active_id() != prev {
            if let Some(id) = prev
                && let Some(t) = self.app.tabs.get_tab(id)
                && let Some(h) = t.webview.as_ref()
                && let Err(e) = h.evaluate_script("window.__ferriteFind&&__ferriteFind('clear')")
            {
                debug!(error = %e, "find clear on switch failed");
            }
            self.find_reset();
            self.push_chrome(true);
        }
        self.hide_ctx_menu();
        // Auto-PiP (opt-in only): the tab left behind was playing video;
        // float it silently so the handoff needs no click. Failures stay
        // silent by design (background tabs may lack gesture credit).
        if self.pip_auto
            && self.app.tabs.active_id() != prev
            && let Some(id) = prev
        {
            self.pip_enter_tab(id);
        }
        // Returning to an open new tab refreshes it: tiles and stats may
        // have moved while it sat in the background (visits elsewhere,
        // pins from another new tab). One push, only when landing on one.
        if let Some(id) = self.app.tabs.active_id()
            && self
                .app
                .tabs
                .get_tab(id)
                .is_some_and(|t| t.url == "ferrite://newtab")
        {
            self.push_newtab_data(id);
        }
        self.layout_tabs();
        self.sync_dialog_overlay();
    }

    /// Load the session file as discarded tabs (count returned).
    /// Missing files yield zero; corrupt files quarantine aside with one
    /// toast (tabs regenerate from history). Never a panic.
    fn restore_session_tabs(&mut self) -> usize {
        let (doc, quarantined) = self.session_doc();
        if quarantined {
            self.notify(
                "Couldn't restore tabs: the saved session was unreadable. Your history still has them."
                    .to_owned(),
            );
        }
        doc.map(|d| self.app.tabs.load_json(&d)).unwrap_or(0)
    }

    /// Peek the session for UI (counts, restore rows): decrypted document,
    /// migration replayed, quarantine toasted once here (this is where the
    /// restore UI learns the session is gone).
    fn peek_session_docs(&mut self) -> (u32, Vec<(String, String)>, usize) {
        let (doc, quarantined) = self.session_doc();
        if quarantined {
            self.notify(
                "Couldn't restore tabs: the saved session was unreadable. Your history still has them."
                    .to_owned(),
            );
        }
        parse_session_doc(doc.as_deref())
    }

    /// Session document for restore/peek: decrypted file, else one-time
    /// legacy plaintext (migration read). Keyless boots get the legacy
    /// read only; nothing is ever written unencrypted (see save paths).
    /// Returns the document plus whether a corrupt file was quarantined.
    fn session_doc(&self) -> (Option<String>, bool) {
        let mut key = Self::cred_key();
        let loaded = crate::session::load_session(key.as_deref());
        if let Some(k) = key.as_mut() {
            k.zeroize();
        }
        (loaded.json, loaded.quarantined)
    }

    /// Write the encrypted session file (AES-GCM, atomic replace). Skipped
    /// without a device key or in ephemeral mode: an unencrypted session
    /// is never written, even when the manager is dirty.
    fn save_session_file(&self) -> Result<(), String> {
        if is_ephemeral() {
            return Err("ephemeral session".to_owned());
        }
        let json = self
            .app
            .tabs
            .save_json()
            .map_err(|e| format!("serialize: {e:?}"))?;
        let mut key = Self::cred_key();
        let out = match key.as_deref() {
            Some(k) => crate::session::save_session(&json, k),
            None => Err("no device key".to_owned()),
        };
        if let Some(k) = key.as_mut() {
            k.zeroize();
        }
        out
    }

    /// Restore page-picked session URLs as discarded tabs (the control
    /// the infobar cannot offer). Titles come from a fresh peek. Unknown
    /// URLs fall back to themselves. Returns tabs restored.
    fn restore_chosen(&mut self, urls: &[String]) -> usize {
        let (_, tabs, _) = self.peek_session_docs();
        let by_url: std::collections::HashMap<&str, &str> =
            tabs.iter().map(|(u, t)| (u.as_str(), t.as_str())).collect();
        let before = self.app.tabs.iter().count();
        for url in urls.iter().take(200) {
            if url.is_empty() || url.len() > 2048 {
                continue;
            }
            let title = by_url.get(url.as_str()).copied().unwrap_or(url.as_str());
            if self
                .app
                .tabs
                .new_discarded_tab(url.clone(), title.to_owned(), false)
                .is_err()
            {
                break;
            }
        }
        let restored = self.app.tabs.iter().count().saturating_sub(before);
        if restored > 0 {
            self.switch_to_index(before);
        }
        restored
    }

    /// Rebuild the webview for the restored active tab.
    fn activate_restored_tab(&mut self) {
        if let Some(idx) = self.app.tabs.active_index() {
            let window = self.window.clone();
            if let Some(window) = window.as_ref() {
                let bounds = self.content_wry();
                if let Err(e) = self.app.tabs.switch_tab_with_window(window, idx, bounds) {
                    debug!(error = %e, "session restore switch failed");
                }
            }
        }
    }

    /// Singleton opener for stateful internal pages (settings): focus the
    /// existing tab instead of duplicating it. A different fragment is a
    /// real navigation (history plus omnibox reflect it, like any load).
    fn open_singleton(&mut self, base: &str, url: String) {
        let found = self
            .app
            .tabs
            .iter()
            .position(|t| singleton_match(&t.url, base));
        if let Some(idx) = found {
            self.switch_to_index(idx);
            let cur = self
                .app
                .tabs
                .active_tab()
                .map(|t| t.url.clone())
                .unwrap_or_default();
            if cur != url {
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    if let Err(e) = tab.load_url(url) {
                        debug!(error = %e, "singleton navigation failed");
                    } else {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                    }
                }
                self.layout_tabs();
            }
        } else {
            self.open_tab(url);
        }
    }

    // ── Tab-search overlay (pure HTML page, unchanged protocol) ──

    /// Toggle the HTML overlay. Created lazily on first open so a failed
    /// build never affects startup. Afterwards show, hide, plus focus prove
    /// the chrome-webview to content-webview focus routing.
    fn toggle_web_tabsearch(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if self.overlay_open {
            self.overlay_open = false;
            Self::close_web_tabsearch(&mut self.overlay, &mut self.app.tabs);
            return;
        }
        let bounds = self.content_wry();
        if self.overlay.is_none() {
            match self
                .app
                .tabs
                .factory()
                .create(&window, "ferrite://tabsearch", bounds)
            {
                Ok(h) => self.overlay = Some(h),
                Err(e) => {
                    warn!(error = %e, "tabsearch overlay creation failed");
                    return;
                }
            }
        }
        if let Some(ov) = self.overlay.as_ref() {
            if let Some(b) = bounds {
                if let Err(e) = ov.set_bounds(b) {
                    debug!(error = %e, "overlay bounds failed");
                }
            }
            if let Err(e) = ov.set_visible(true) {
                debug!(error = %e, "overlay show failed");
            }
            if let Err(e) = ov.focus() {
                warn!(error = %e, "tabsearch overlay focus failed");
            }
            // Page also calls this on load. Re-invoking resets the query
            // and restarts the entrance animation on every open.
            if let Err(e) = ov.evaluate_script("__ferriteShow()") {
                debug!(error = %e, "overlay show script failed");
            }
        }
        self.overlay_open = true;
    }

    /// Hide the overlay and return keyboard focus to the active tab
    /// content webview (falling back to whatever holds it if none).
    fn close_web_tabsearch(
        overlay: &mut Option<Rc<dyn WebViewHandle>>,
        tabs: &mut ferrite_tabs::TabManager,
    ) {
        if let Some(ov) = overlay.as_ref()
            && let Err(e) = ov.set_visible(false)
        {
            debug!(error = %e, "overlay hide failed");
        }
        if let Some(tab) = tabs.active_tab()
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.focus()
        {
            debug!(error = %e, "content refocus failed");
        }
    }

    // ── Omnibox ──

    /// Persist the user custom engine list.
    fn persist_customs(&self) {
        let value = serde_json::to_value(self.app.omnibox.registry.custom_engines())
            .unwrap_or(serde_json::Value::Null);
        persist_setting("engines_custom", value);
    }

    /// Apply a settings document to live state: every present key is
    /// validated exactly like its boot read and takes effect immediately:
    /// layout, engine, tabs, and relay session included. No persistence
    /// here (callers own the disk write) and no reloads. The one
    /// destructive transition is the autofill-form wipe, identical to the
    /// toggle itself; everything else only flips in-memory flags.
    /// Unknown keys are ignored so newer files never break older builds.
    /// `proxy_enabled` is restart-gated (boot plumbing only): it applies
    /// to the engine but moves no traffic until the next boot, and the
    /// caller is told when it flips.
    fn apply_settings_doc(&mut self, v: &serde_json::Value) {
        if let Some(name) = v.get("chrome_pack").and_then(|x| x.as_str())
            && let Some(pack) = ThemePack::from_name(name)
        {
            self.apply_pack_live(pack);
        }
        if let Some(customs) = v.get("engines_custom").and_then(|x| x.as_array()) {
            self.app.omnibox.registry.customs_clear();
            for c in customs {
                let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let tpl = c.get("template").and_then(|x| x.as_str()).unwrap_or("");
                let _ = self.app.omnibox.registry.add_custom(name, tpl);
            }
        }
        if let Some(id) = v.get("omnibox_engine").and_then(|x| x.as_str()) {
            let _ = self.app.omnibox.registry.set_current(id);
        }
        if let Some(id) = v.get("omnibox_engine_private").and_then(|x| x.as_str()) {
            if id.is_empty() {
                self.app.omnibox.registry.clear_private();
            } else if self.app.omnibox.registry.set_private(id).is_err() {
                debug!(engine = %id, "unknown private engine ignored");
                clear_setting("omnibox_engine_private");
            }
        }
        if let Some(home) = v
            .get("home_url")
            .and_then(|x| x.as_str())
            .filter(|u| valid_home_url(u))
        {
            self.home_url = home.to_owned();
        }
        if let Some(sug) = v.get("suggestions").and_then(|x| x.as_bool()) {
            self.suggestions_enabled = sug;
        }
        if let Some(remote) = v.get("suggestions_remote").and_then(|x| x.as_bool()) {
            self.remote_enabled = remote;
        }
        if let Some(restore) = v.get("restore_always").and_then(|x| x.as_bool()) {
            self.restore_always = restore;
        }
        if let Some(mode) = v.get("startup_mode").and_then(|x| x.as_str()) {
            self.startup_mode = clean_startup_mode(mode).to_owned();
        }
        if let Some(urls) = v.get("startup_urls").and_then(|x| x.as_array()) {
            let cleaned: Vec<String> = urls
                .iter()
                .filter_map(|x| x.as_str())
                .filter_map(|u| {
                    let u = u.trim();
                    if valid_home_url(u) {
                        Some(u.to_owned())
                    } else {
                        None
                    }
                })
                .take(MAX_STARTUP_URLS)
                .collect();
            self.startup_urls = cleaned;
        }
        if let Some(picker) = v.get("profile_picker").and_then(|x| x.as_bool()) {
            self.profile_picker = picker;
        }
        if let Some(days) = v.get("history_retention_days").and_then(|x| x.as_u64()) {
            self.history_retention_days = if days == 0 { 0 } else { days.clamp(7, 3650) };
        }
        if let Some(minutes) = v.get("sleep_timeout_min").and_then(|x| x.as_u64()) {
            let minutes = minutes.min(24 * 60);
            self.sleep_timeout_min = minutes;
            self.app
                .tabs
                .set_idle_timeout(std::time::Duration::from_secs(minutes * 60));
        }
        if let Some(exceptions) = v.get("sleep_exceptions").and_then(|x| x.as_array()) {
            let hosts: Vec<String> = exceptions
                .iter()
                .filter_map(|x| x.as_str())
                .filter_map(clean_sleep_host)
                .collect();
            self.sleep_exceptions = hosts.clone();
            self.app.tabs.set_sleep_exceptions(hosts);
        }
        if let Some(done) = v.get("onboarding_completed").and_then(|x| x.as_bool()) {
            self.onboarding_completed = done;
        }
        if let Some(wrap) = v.get("viewsource_wrap").and_then(|x| x.as_bool()) {
            self.viewsource_wrap = wrap;
        }
        if let Some(a) = v.get("autofill_address").and_then(|x| x.as_bool()) {
            self.autofill_address = a;
        }
        if let Some(p) = v.get("autofill_payment").and_then(|x| x.as_bool()) {
            self.autofill_payment = p;
        }
        // Same destructive edge as the toggle: turning form history off
        // wipes what's stored. Turning it on never deletes.
        if let Some(form) = v.get("autofill_form").and_then(|x| x.as_bool()) {
            if !form
                && self.autofill_form
                && let Some(vault) = self.vault.as_ref()
                && let Err(e) = vault.clear_form_history()
            {
                debug!(error = %e, "form history wipe failed");
            }
            self.autofill_form = form;
        }
        if let Some(e) = v.get("translate_enabled").and_then(|x| x.as_bool()) {
            self.translate_enabled = e;
        }
        if let Some(t) = v.get("translate_target").and_then(|x| x.as_str()) {
            self.translate_target = ferrite_tabs::translate::normalize_target(t);
        }
        if let Some(size) = v.get("reader_size").and_then(|x| x.as_i64()) {
            self.reader_size = size.clamp(87, 150) as i32;
        }
        if let Some(serif) = v.get("reader_serif").and_then(|x| x.as_bool()) {
            self.reader_serif = serif;
        }
        if let Some(wide) = v.get("reader_wide").and_then(|x| x.as_bool()) {
            self.reader_wide = wide;
        }
        if let Some(theme) = v
            .get("reader_theme")
            .and_then(|x| x.as_str())
            .filter(|s| matches!(*s, "light" | "sepia" | "dark"))
        {
            self.reader_theme = theme.to_owned();
        }
        if let Some(mode) = v
            .get("bar_mode")
            .and_then(|x| x.as_str())
            .filter(|m| matches!(*m, "always" | "never" | "ntp"))
        {
            self.bar_mode = mode.to_owned();
        }
        if let Some(seen) = v.get("bar_seen").and_then(|x| x.as_bool()) {
            self.bar_seen = seen;
        }
        if let Some(vertical) = v.get("tabs_vertical").and_then(|x| x.as_bool()) {
            self.tabs_vertical = vertical;
        }
        if let Some(open) = v.get("panel_open").and_then(|x| x.as_bool()) {
            self.panel_open = open;
        }
        if let Some(section) = v
            .get("panel_section")
            .and_then(|x| x.as_str())
            .filter(|s| matches!(*s, "bookmarks" | "history" | "reading"))
        {
            self.panel_section = section.to_owned();
        }
        if let Some(w) = v.get("panel_w").and_then(|x| x.as_u64()) {
            self.panel_w = w.clamp(240, 480) as u32;
        }
        if let Some(ask) = v.get("ask_downloads").and_then(|x| x.as_bool()) {
            self.ask_downloads
                .store(ask, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(pip) = v.get("pip_auto").and_then(|x| x.as_bool()) {
            self.pip_auto = pip;
        }
        if let Some(global) = v.get("notifications_global").and_then(|x| x.as_bool()) {
            self.permissions.set_notifications_global(global);
        }
        if let Some(dir) = v
            .get("save_last_dir")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            self.save_last_dir = Some(std::path::PathBuf::from(dir));
        }
        // Operator TLS pins: env wins (boot rule); settings apply live.
        if std::env::var("FERRITE_TLS_PINS").is_err()
            && let Some(src) = v.get("tls_pins").and_then(|x| x.as_str())
        {
            let (pins, skipped) = ferrite_proxy::parse_pin_list(src);
            if !pins.is_empty() || skipped > 0 {
                ferrite_proxy::set_tls_pins(pins);
            }
        }
        // Legacy permission grants: same one-way migration as boot.
        if let Some(grants) = v.get("perm_grants").and_then(|x| x.as_array()) {
            let pairs: Vec<(String, String)> = grants
                .iter()
                .filter_map(|g| {
                    let o = g.get("origin")?.as_str()?.to_owned();
                    let k = g.get("kind")?.as_str()?.to_owned();
                    Some((o, k))
                })
                .collect();
            if !pairs.is_empty() {
                self.permissions
                    .load_always(pairs, ferrite_tabs::now_millis());
                clear_setting("perm_grants");
            }
        }
        // Off-LAN relay: attach to the live session (file already holds
        // the URLs; empty clears back to LAN-only).
        if let Some(urls) = v.get("relay_urls").and_then(|x| x.as_array()) {
            let joined = urls
                .iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let cleaned = clean_relay_urls(&joined);
            if let (Some(session), Some(t)) = (self.sync_session.as_ref(), self.sync_trust.as_ref())
            {
                if cleaned.is_empty() {
                    session.clear_relay();
                } else {
                    session.set_relay(cleaned, &t.this.identity);
                }
            }
        }
        // Privacy engine: merge present keys over live settings so absent
        // keys keep their values (a partial doc must never clobber). The
        // engine validates DoH and normalizes the API key on import.
        const ENGINE_KEYS: [&str; 10] = [
            "adblock_enabled",
            "gpc_enabled",
            "realtime_lookup_enabled",
            "https_upgrade_enabled",
            "proxy_enabled",
            "doh_endpoint",
            "realtime_api_key",
            "paused_hosts",
            "spellcheck_enabled",
            "spellcheck_langs",
        ];
        if ENGINE_KEYS.iter().any(|k| v.get(k).is_some()) {
            let mut merged =
                serde_json::to_value(self.privacy.settings()).unwrap_or(serde_json::Value::Null);
            if let (Some(dst), Some(src)) = (merged.as_object_mut(), v.as_object()) {
                for k in ENGINE_KEYS {
                    let Some(val) = src.get(k) else {
                        continue;
                    };
                    // Invalid DoH never overwrites: refused with words, live
                    // kept. Empty clears (command semantics); valid applies.
                    if k == "doh_endpoint"
                        && let Some(ep) = val.as_str()
                        && !ep.is_empty()
                        && (ferrite_proxy::DnsConfig {
                            doh_endpoint: Some(ep.to_owned()),
                        })
                        .endpoint_problem()
                        .is_some()
                    {
                        self.notify("Imported DoH endpoint refused; keeping current.".to_owned());
                        continue;
                    }
                    dst.insert(k.to_owned(), val.clone());
                }
            }
            let proxy_before = self.privacy.settings().proxy_enabled;
            self.privacy.import_settings_json(&merged.to_string());
            if self.privacy.settings().proxy_enabled != proxy_before {
                self.notify("Proxy mode changes apply after restart.".to_owned());
            }
            self.apply_spellcheck_all();
        }
        self.layout_tabs();
        if self.panel_open {
            self.push_panel_section();
        }
        self.push_chrome(true);
    }

    /// Reset every preference to boot defaults, live. The sealed file is
    /// already deleted by the caller; this performs zero disk writes so the
    /// reset cannot recreate what it just removed; the next persisting
    /// toggle starts a fresh file. Vault, history, downloads, and
    /// permission grants are untouched: preferences only, never user data.
    fn reset_to_defaults(&mut self) {
        let defaults = serde_json::json!({
            "chrome_pack": "tenebra",
            "suggestions": true,
            "suggestions_remote": false,
            "restore_always": false,
            "startup_mode": "restore",
            "startup_urls": [],
            "profile_picker": true,
            "history_retention_days": 90,
            "tabs_vertical": false,
            "sleep_timeout_min": 30,
            "sleep_exceptions": [],
            "onboarding_completed": false,
            "viewsource_wrap": false,
            "autofill_address": true,
            "autofill_payment": true,
            "autofill_form": true,
            "translate_enabled": true,
            "translate_target": ferrite_tabs::translate::DEFAULT_TARGET,
            "reader_size": 100,
            "reader_serif": false,
            "reader_wide": false,
            "reader_theme": "light",
            "home_url": "ferrite://newtab",
            "bar_mode": "never",
            "bar_seen": false,
            "panel_open": false,
            "panel_section": "bookmarks",
            "panel_w": 320,
            "ask_downloads": false,
            "pip_auto": false,
            "omnibox_engine": "duckduckgo",
            "engines_custom": [],
            "relay_urls": [],
            "notifications_global": true,
        });
        self.apply_settings_doc(&defaults);
        // Privacy engine back to compiled defaults (the doc above carries
        // no engine keys by design: engine reset is explicit, not a side
        // effect of absent keys).
        self.privacy.import_settings_json(
            &serde_json::to_string(&ferrite_privacy::PrivacySettings::default())
                .unwrap_or_default(),
        );
        self.apply_spellcheck_all();
        // Off-LAN relay off live (sealed URLs already deleted).
        if let Some(session) = self.sync_session.as_ref() {
            session.clear_relay();
        }
        // Private engine follows the regular default again.
        self.app.omnibox.registry.clear_private();
    }

    /// Start one background update cycle (no-op while a worker runs).
    /// Manual checks (About button) bypass the hourly throttle and toast
    /// their failures. Scheduled failures stay in the debug log.
    fn spawn_update_check(&mut self, manual: bool) {
        use crate::update::UpdatePhase;
        match self.update_phase {
            UpdatePhase::Checking | UpdatePhase::Downloading { .. } => {
                if manual {
                    self.notify("Already checking for updates.".to_owned());
                }
                return;
            }
            _ => {}
        }
        self.update_phase = UpdatePhase::Checking;
        self.manual_update_check = manual;
        if !manual {
            self.last_update_check = Instant::now();
        }
        let tx = self.update_tx.clone();
        let url = crate::update::manifest_url();
        let channel = crate::update::update_channel();
        let current = env!("CARGO_PKG_VERSION").to_owned();
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ferrite"));
        std::thread::Builder::new()
            .name("ferrite-update".to_owned())
            .spawn(move || {
                crate::update::run_update_cycle(&url, &channel, &current, &exe, tx, manual);
            })
            .ok();
        self.push_update_about();
    }

    /// Drain finished worker events into phase + toasts + About pushes.
    fn drain_update_events(&mut self) {
        use crate::update::{UpdateEvent, UpdateOutcome, UpdatePhase};
        while let Ok(ev) = self.update_rx.try_recv() {
            match ev {
                UpdateEvent::Progress { downloaded, total } => {
                    self.update_phase = UpdatePhase::Downloading { downloaded, total };
                    self.push_update_about();
                }
                UpdateEvent::Done { manual, result } => {
                    self.manual_update_check = false;
                    match result {
                        Ok(UpdateOutcome::Staged { version, security }) => {
                            self.update_phase = UpdatePhase::Ready {
                                version: version.clone(),
                                security,
                                reminded_at: Instant::now(),
                            };
                            self.notify(crate::update::staged_toast(&version, security));
                        }
                        Ok(UpdateOutcome::RolledBack { version }) => {
                            // A staged rollback means the running build is
                            // bad: escalate reminders like a security fix.
                            self.update_phase = UpdatePhase::Ready {
                                version: version.clone(),
                                security: true,
                                reminded_at: Instant::now(),
                            };
                            self.notify(format!(
                                "Rolled back to {version}. Restart Ferrite to apply it."
                            ));
                        }
                        Ok(UpdateOutcome::UpToDate) => {
                            self.update_phase = UpdatePhase::UpToDate;
                            if manual {
                                self.notify("Ferrite is up to date.".to_owned());
                            }
                        }
                        Err(e) => {
                            self.update_phase = UpdatePhase::Failed(e.clone());
                            if manual {
                                self.notify(format!("Update check failed: {e}"));
                            } else {
                                debug!(error = %e, "background update cycle failed");
                            }
                        }
                    }
                    self.push_update_about();
                }
            }
        }
    }

    /// Hourly scheduled check plus the security-update reminder escalation.
    /// Routine staged updates toast once (at staging). Security updates
    /// re-toast every 30 minutes until applied or superseded.
    fn tick_update(&mut self) {
        use crate::update::{SECURITY_REMINDER_INTERVAL, UPDATE_CHECK_INTERVAL, UpdatePhase};
        self.drain_update_events();
        if self.last_update_check.elapsed() >= UPDATE_CHECK_INTERVAL {
            self.last_update_check = Instant::now();
            self.spawn_update_check(false);
        }
        if let UpdatePhase::Ready {
            version,
            security,
            reminded_at,
        } = &self.update_phase
            && *security
            && reminded_at.elapsed() >= SECURITY_REMINDER_INTERVAL
        {
            let version = version.clone();
            self.update_phase = UpdatePhase::Ready {
                version: version.clone(),
                security: true,
                reminded_at: Instant::now(),
            };
            self.notify(crate::update::staged_toast(&version, true));
        }
    }

    /// Push update phase JSON into open About pages (`__ferriteUpdate`).
    fn push_update_about(&self) {
        let payload = crate::update::phase_json(&self.update_phase);
        let json = serde_json::to_string(&payload).unwrap_or_default();
        for id in self.page_tabs("ferrite://about") {
            if let Some(tab) = self.app.tabs.get_tab(id)
                && let Some(h) = tab.webview.as_ref()
                && let Err(e) = h.evaluate_script(&format!(
                    "window.__ferriteUpdate && window.__ferriteUpdate({json})"
                ))
            {
                debug!(error = %e, "update push failed");
            }
        }
    }

    /// Push the full settings envelope (privacy plus pack plus engines plus customs
    /// plus suggestions) into a settings page `__ferriteSetSettings` hook.
    /// Privacy settings with secrets redacted for page state. The Safe
    /// Browsing key must never linger in any webview: the page gets only
    /// a set flag, the value travels one way (page → shell post).
    fn masked_privacy_for_push(&self) -> serde_json::Value {
        let mut privacy =
            serde_json::to_value(self.privacy.settings()).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = privacy.as_object_mut() {
            let has_key = obj
                .get("realtime_api_key")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty());
            obj.insert(
                "realtime_api_key_set".to_owned(),
                serde_json::Value::Bool(has_key),
            );
            obj.insert("realtime_api_key".to_owned(), serde_json::Value::Null);
        }
        privacy
    }

    fn push_settings(&self, tab_id: Uuid) {
        let current = self.app.omnibox.registry.current_id().to_owned();
        let private = self.app.omnibox.registry.private_id().map(str::to_owned);
        let mut engines: Vec<serde_json::Value> = ferrite_omnibox::ENGINES
            .iter()
            .map(|e| serde_json::json!({"id": e.id, "name": e.name, "current": e.id == current}))
            .collect();
        engines.extend(
            self.app.omnibox.registry.custom_engines().iter().map(
                |e| serde_json::json!({"id": e.id, "name": e.name, "current": e.id == current}),
            ),
        );
        let packs: Vec<serde_json::Value> = ThemePack::ALL
            .iter()
            .map(|p| {
                serde_json::json!({"id": p.name(), "name": p.display_name(), "current": *p == self.theme_pack})
            })
            .collect();
        let payload = serde_json::json!({
            "privacy": self.masked_privacy_for_push(),
            "suggestions": self.suggestions_enabled,
            "suggestions_remote": self.remote_enabled,
            "restore_always": self.restore_always,
            "startup_mode": self.startup_mode,
            "startup_urls": self.startup_urls,
            "profile_picker": self.profile_picker,
            "history_retention_days": self.history_retention_days,
            "tabs_vertical": self.tabs_vertical,
            "sleep_timeout_min": self.sleep_timeout_min,
            "sleep_exceptions": self.sleep_exceptions,
            "autofill_address": self.autofill_address,
            "autofill_payment": self.autofill_payment,
            "autofill_form": self.autofill_form,
            "notifications_global": self.permissions.notifications_global(),
            "translate_enabled": self.translate_enabled,
            "translate_target": self.translate_target,
            "ask_downloads": self
                .ask_downloads
                .load(std::sync::atomic::Ordering::Relaxed),
            "pip_auto": self.pip_auto,
            "home_url": self.home_url,
            "engines": engines,
            "private_engine": private,
            "packs": packs,
            "customs": self.app.omnibox.registry.custom_engines(),
        });
        let json = serde_json::to_string(&payload).unwrap_or_default();
        let js = format!("window.__ferriteSetSettings && window.__ferriteSetSettings({json})");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&js)
        {
            debug!(error = %e, "settings push failed");
        }
    }

    /// Push current spellcheck prefs into one live tab. Documents without
    /// the installer (privileged pages carry no init scripts) no-op via
    /// the guarded apply call. Never an error, never a log.
    fn apply_spellcheck_prefs(&self, tab_id: Uuid) {
        let s = self.privacy.settings();
        let js = ferrite_privacy::spellcheck_apply_js(s.spellcheck_enabled, &s.spellcheck_langs);
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&js)
        {
            debug!(error = %e, "spellcheck apply failed");
        }
    }

    /// Persist the privacy engine's fields into the sealed settings file,
    /// preserving shell-owned keys (merge, not overwrite: the old engine
    /// file writer clobbered them on every toggle). Same key and ephemeral
    /// discipline as every other settings write.
    fn persist_privacy_settings(&self) {
        if is_ephemeral() {
            tracing::debug!("privacy settings persist refused: ephemeral session");
            return;
        }
        let exported = self.privacy.export_settings_json();
        let Ok(fields) = serde_json::from_str::<serde_json::Value>(&exported) else {
            return;
        };
        let mut keyring = Self::cred_key();
        let mut doc = crate::settings_file::read_settings_value(keyring.as_deref());
        if let (Some(dst), Some(src)) = (doc.as_object_mut(), fields.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let ok = match keyring.as_deref() {
            Some(k) => crate::settings_file::write_settings_value(&doc, k),
            None => false,
        };
        if let Some(k) = keyring.as_mut() {
            k.zeroize();
        }
        if !ok {
            tracing::debug!("privacy settings persist deferred: no device key");
        }
    }

    /// Push current spellcheck prefs into every live tab (toggle or
    /// language change takes effect immediately, no relaunch).
    fn apply_spellcheck_all(&self) {
        let s = self.privacy.settings();
        let js = ferrite_privacy::spellcheck_apply_js(s.spellcheck_enabled, &s.spellcheck_langs);
        for t in self.app.tabs.iter() {
            if let Some(h) = t.webview.as_ref()
                && let Err(e) = h.evaluate_script(&js)
            {
                debug!(error = %e, "spellcheck apply failed");
            }
        }
    }

    /// Fetch worker for dictionaries missing from the bundle dir, for the
    /// active languages only. Called once on first tick and on explicit
    /// language changes. No-op when nothing is missing, and never in
    /// ephemeral mode (no network surprise, no disk writes).
    fn provision_missing_dicts(&self) {
        if is_ephemeral() {
            return;
        }
        let langs = self.privacy.settings().spellcheck_langs;
        let missing =
            ferrite_privacy::missing_dicts_in(&ferrite_privacy::dict_bundle_dir(), &langs);
        if missing.is_empty() {
            return;
        }
        crate::dicts::spawn_dict_fetch(missing, self.dict_tx.clone());
    }

    /// Push the exclusion list into a settings page `__ferriteExclusions`
    /// hook (called on load and after every add/remove. The settings
    /// equivalent of the list-page fan-out).
    fn push_exclusions(&self, tab_id: Uuid) {
        let items: Vec<serde_json::Value> = match self.vault.as_ref() {
            Some(v) => v
                .list_excluded_origins()
                .into_iter()
                .map(|o| serde_json::json!(o))
                .collect(),
            None => Vec::new(),
        };
        self.push_list(tab_id, "__ferriteExclusions", &items);
    }

    /// Push one origin's stored data into the site page `__ferriteSite`
    /// hook. Every figure comes from a validated canonical origin (never
    /// the raw page param): usernames only for logins (never secrets),
    /// live engine counts for blocks.
    fn push_site(&self, tab_id: Uuid, origin: &str) {
        let host = site_origin_host(origin);
        let (visits, last) = self
            .vault
            .as_ref()
            .and_then(|v| v.history_origin_stats(host).ok())
            .unwrap_or((0, 0));
        let logins: Vec<String> = self
            .cred_store()
            .map(|s| {
                s.all()
                    .into_iter()
                    .filter(|r| {
                        r.url
                            .split("://")
                            .nth(1)
                            .unwrap_or(&r.url)
                            .split(['/', '?', '#'])
                            .next()
                            .unwrap_or("")
                            .eq_ignore_ascii_case(host)
                    })
                    .map(|r| r.username)
                    .collect()
            })
            .unwrap_or_default();
        let perms: Vec<String> = self
            .permissions
            .always_grants()
            .into_iter()
            .filter(|(o, _)| o == origin)
            .map(|(_, k)| ferrite_tabs::permissions::kind_label(&k).to_owned())
            .collect();
        let zoom = self
            .vault
            .as_ref()
            .and_then(|v| v.zoom_for(origin))
            .map(|f| (f * 100.0).round() as u32);
        let blocked = {
            let (_, ad, tracker, _, malware, mixed) = self.privacy.origin_breakdown(host);
            ad + tracker + malware + mixed
        };
        let payload = serde_json::json!({
            "origin": origin,
            "secure": origin.starts_with("https://"),
            "visits": visits,
            "last_visit": last,
            "logins": logins,
            "perms": perms,
            "zoom": zoom,
            "cookies": self.cookies_count_site(origin),
            "forms": self
                .vault
                .as_ref()
                .map(|v| v.count_form_history_origin(origin))
                .unwrap_or(0),
            "blocked": blocked,
        });
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!(
                "window.__ferriteSite&&window.__ferriteSite({json})"
            ))
        {
            debug!(error = %e, "site push failed");
        }
    }

    /// Forget one origin: history, logins, permission grants, zoom, and
    /// cookies. Bookmarks and download records stay (user-managed, not
    /// site residue). Counts toast; the page re-pushes zeros.
    fn forget_site(&mut self, tab_id: Uuid, origin: &str) {
        let host = site_origin_host(origin).to_owned();
        let mut h = 0u64;
        if let Some(v) = self.vault.as_ref() {
            h = v.delete_history_origin(&host).unwrap_or(0);
            self.app.autocomplete.clear();
        }
        let mut p = 0u64;
        if let Some(store) = self.cred_store() {
            let ids: Vec<String> = store
                .all()
                .into_iter()
                .filter(|r| {
                    r.url
                        .split("://")
                        .nth(1)
                        .unwrap_or(&r.url)
                        .split(['/', '?', '#'])
                        .next()
                        .unwrap_or("")
                        .eq_ignore_ascii_case(&host)
                })
                .map(|r| r.id)
                .collect();
            for id in ids {
                if store.delete(&id).is_ok() {
                    p += 1;
                }
            }
        }
        let mut g = 0u64;
        for (o, kind) in self.permissions.always_grants() {
            if o == origin {
                self.permissions.revoke_always(&o, &kind);
                g += 1;
            }
        }
        if g > 0 {
            self.persist_perm_snapshot();
        }
        if let Some(v) = self.vault.as_ref() {
            let _ = v.set_zoom(origin, 1.0);
        }
        let mut f = 0u64;
        if let Some(v) = self.vault.as_ref() {
            f = v.delete_form_history_origin(origin).unwrap_or(0);
        }
        let (_, k) = self.cookies_clear_site(origin);
        self.push_site(tab_id, origin);
        self.notify(format!(
            "Forgot {origin}: {h} history entries, {p} logins, {g} permission grants, {k} cookies, {f} form entries."
        ));
    }

    /// Push the profile list into the boot picker's `__ferriteProfiles`
    /// hook (called on page load). Same rows as the avatar popover, so
    /// the two surfaces cannot disagree.
    fn push_profiles(&self, tab_id: Uuid) {
        let items: Vec<serde_json::Value> = self
            .profile_rows()
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "name": r.name,
                    "initial": r.initial,
                    "current": r.current,
                })
            })
            .collect();
        self.push_list(tab_id, "__ferriteProfiles", &items);
    }

    /// Close the picker tab by URL (no id is tracked: exactly one picker
    /// exists while pending, like the gate's decision tab).
    fn close_picker_tab(&mut self) {
        // Resolve first (borrow ends), then mutate: let-chains would
        // hold the iterator across the close.
        let id = self
            .app
            .tabs
            .iter()
            .find(|t| t.url == "ferrite://profiles")
            .map(|t| t.id);
        if let Some(id) = id
            && self.app.tabs.close_tab(id).is_ok()
        {
            self.layout_tabs();
        }
    }

    /// Profile picker owns first paint when several profiles exist, no
    /// `--profile` picked one, the setting is on, and this isn't an
    /// ephemeral launch. The boot gate owns paint while blocked; Retry
    /// resolves straight into `boot_tabs` without re-asking.
    fn should_pick_profile(&self) -> bool {
        !self.config.incognito
            && !self.guest_mode
            && self.config.profile.is_none()
            && self.profile_picker
            && !crate::profiles::list().is_empty()
    }

    /// Show a transient failure notice (toast, 5s). Latest wins. Success
    /// stays silent (the dialog closing is the feedback).
    fn notify(&mut self, text: String) {
        self.toasts
            .push(crate::toast::ToastItem::failure(text), Instant::now());
        self.push_chrome(true);
    }

    /// Push an undoable-delete toast and stage its rows under the frame
    /// seq. The Undo action restores only while this staging is live
    /// (visible head, unexpired). See [`crate::toast::PendingUndo`].
    fn push_undo(&mut self, text: String, payload: crate::toast::UndoPayload) {
        use std::time::Duration;
        let now = Instant::now();
        let seq = self
            .toasts
            .push_undo(crate::toast::ToastItem::undoable(text), now);
        self.pending_undo = Some(crate::toast::PendingUndo {
            seq,
            payload,
            expires: now + Duration::from_secs(6),
        });
        self.sync_undo_deadline(now);
        self.push_chrome(true);
    }

    /// Plain delete confirmation when the delete outgrew undo staging.
    /// Confirms, never promises a restore.
    fn push_delete_done(&mut self, text: String) {
        self.toasts
            .push(crate::toast::ToastItem::delete_done(text), Instant::now());
        self.push_chrome(true);
    }

    /// Clipboard confirmation (the generalized "Copied" primitive).
    fn push_copied(&mut self, text: &str) {
        self.toasts
            .push(crate::toast::ToastItem::clipboard(text), Instant::now());
        self.push_chrome(true);
    }

    /// Copy text with a toast receipt. Returns whether the OS accepted it
    /// (empty input is a no-op `false`, never an error popup).
    fn copy_with_toast(&mut self, text: &str, what: &str) -> bool {
        if !self.copy_text(text) {
            self.notify(format!("Couldn't copy {what}"));
            return false;
        }
        self.push_copied(&format!("{what} copied to clipboard"));
        true
    }

    /// Sensitive copy with a toast receipt (vault passwords, card PANs, TOTP
    /// codes). Windows opt-outs plus 45s auto-clear. The secret is decrypted
    /// Rust-side from a vault id, the page only sends the id.
    fn copy_sensitive_with_toast(&mut self, secret: &str, what: &str) -> bool {
        if !crate::clipboard::copy_sensitive(secret) {
            self.notify(format!("Couldn't copy {what}"));
            return false;
        }
        self.push_copied(&format!(
            "{what} copied; clears in {}s",
            crate::clipboard::sensitive_clear_secs()
        ));
        true
    }

    /// Refresh the undo staging deadline while its toast is the visible
    /// head (a queued-behind undo must not expire before it is shown).
    /// Drop staging whose toast left the system. Its action is dead.
    fn sync_undo_deadline(&mut self, now: Instant) {
        use crate::toast::ToastActionKind;
        use std::time::Duration;
        let keeps = self
            .pending_undo
            .as_ref()
            .is_some_and(|u| self.toasts.contains_seq(u.seq));
        if !keeps {
            self.pending_undo = None;
            return;
        }
        if let Some(undo) = self.pending_undo.as_mut()
            && self.toasts.current_seq() == Some(undo.seq)
            && matches!(self.toasts.current_action(), Some(ToastActionKind::Undo))
        {
            undo.expires = now + Duration::from_secs(6);
        }
    }

    /// Restore staged rows behind a live Undo action, then refresh every
    /// surface that shows the affected data. Stale clicks (superseded or
    /// expired staging) retire the toast and restore nothing.
    fn undo_delete(&mut self, seq: u64) {
        let now = Instant::now();
        let live = self
            .pending_undo
            .as_ref()
            .is_some_and(|u| u.is_live(seq, now));
        if !live {
            self.toasts.dismiss(seq, now);
            self.push_chrome(true);
            return;
        }
        let undo = match self.pending_undo.take() {
            Some(undo) => undo,
            None => {
                self.toasts.dismiss(seq, now);
                self.push_chrome(true);
                return;
            }
        };
        self.restore_undo(undo.payload);
        self.toasts.dismiss(seq, now);
        // No second toast: the reappearing rows are the confirmation.
        self.push_chrome(true);
    }

    /// Re-insert staged rows verbatim and re-push affected views.
    fn restore_undo(&mut self, payload: crate::toast::UndoPayload) {
        use crate::toast::UndoPayload as P;
        let Some(vault) = self.vault.as_ref() else {
            debug!("undo ignored: vault unavailable");
            return;
        };
        match payload {
            P::Bookmarks(ids) => {
                for id in &ids {
                    if let Err(e) = vault.restore_bookmark(id) {
                        debug!(error = %e, "bookmark undo failed");
                    }
                }
                self.push_panel_section();
                self.push_bookmarks_tabs();
            }
            P::History(rows) => {
                if let Err(e) = vault.restore_history_rows(&rows) {
                    debug!(error = %e, "history undo failed");
                }
                self.refresh_history_views();
            }
            P::Downloads(rows) => {
                for r in &rows {
                    if let Err(e) = vault.restore_download_row(r) {
                        debug!(error = %e, "download undo failed");
                    }
                }
                self.refresh_downloads_views();
            }
            P::Reading(rows) => {
                for r in &rows {
                    if let Err(e) = vault.restore_reading_row(r) {
                        debug!(error = %e, "reading undo failed");
                    }
                }
                self.push_panel_section();
            }
            P::ClearData { history, downloads } => {
                if let Err(e) = vault.restore_history_rows(&history) {
                    debug!(error = %e, "clear-data history undo failed");
                }
                for r in &downloads {
                    if let Err(e) = vault.restore_download_row(r) {
                        debug!(error = %e, "clear-data download undo failed");
                    }
                }
                self.refresh_history_views();
                self.refresh_downloads_views();
            }
        }
    }

    /// Re-push the history page in every tab showing it.
    fn refresh_history_views(&self) {
        for (tab_id, q) in self.history_queries.iter() {
            let items = self.history_json(q);
            self.push_list(*tab_id, "__ferriteHistory", &items);
        }
    }

    /// Re-push the downloads page in every tab showing it.
    fn refresh_downloads_views(&self) {
        for (tab_id, (q, status)) in self.dl_queries.iter() {
            let items = self.downloads_json(q, status);
            self.push_list(*tab_id, "__ferriteDownloads", &items);
        }
    }

    /// Candidate gate for password-save prompts (pure, unit-tested).
    /// Origin must be http(s) with no whitespace. Username 1 to 256 chars,
    /// password 1 to 4096 chars. Caps mirror the scanner and the bridge.
    fn valid_save_candidate(origin: &str, username: &str, password: &str) -> bool {
        let o = origin.trim();
        let lower = o.to_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return false;
        }
        if o.len() > 512 || o.contains(char::is_whitespace) {
            return false;
        }
        let (un, pw) = (username.chars().count(), password.chars().count());
        (1..=256).contains(&un) && (1..=4096).contains(&pw)
    }

    /// Offer to save a submitted login: stage the secret, show the Save toast.
    /// Skips silently with no vault or key, with an existing pair, or when
    /// already prompted this session.
    fn push_save_candidate(&mut self, origin: String, username: String, password: Vec<u8>) {
        use std::time::Duration;
        let password_str = match String::from_utf8(password) {
            Ok(s) => s,
            Err(_) => {
                debug!("save candidate refused: password not UTF-8");
                return;
            }
        };
        if !Self::valid_save_candidate(&origin, &username, &password_str) {
            return;
        }
        let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key()) else {
            debug!("save candidate held: vault or key unavailable");
            return;
        };
        // Existing pair (any password) reuses the prompt as an update
        // offer. Never a silent overwrite, never a duplicate row.
        let existing = store
            .get_for_origin(&origin)
            .into_iter()
            .find(|r| r.username == username)
            .map(|r| r.id);
        // Single nag per session per pair (saved or dismissed). The
        // passwords page edits credentials deliberately afterwards.
        if !self
            .save_prompted
            .insert((origin.clone(), username.clone()))
        {
            key.zeroize();
            return;
        }
        if self.save_prompted.len() >= 64 {
            self.save_prompted.clear();
        }
        key.zeroize();
        let now = Instant::now();
        let item = crate::toast::ToastItem::save_login(&origin, &username, existing.is_some());
        let seq = self.toasts.push_save(item, now);
        self.pending_save = Some(crate::toast::PendingSave {
            seq,
            origin,
            username,
            password: password_str.into_bytes(),
            expires: now + Duration::from_secs(8),
        });
        self.sync_save_deadline(now);
        self.push_chrome(true);
    }

    /// Refresh the save staging deadline while its toast is the visible
    /// head. Drop staging whose toast left the system. `Drop` zeroizes
    /// the secret, so forgetting here is the secure path, not a leak.
    fn sync_save_deadline(&mut self, now: Instant) {
        use crate::toast::ToastActionKind;
        let keeps = self
            .pending_save
            .as_ref()
            .is_some_and(|s| self.toasts.contains_seq(s.seq));
        if !keeps {
            self.pending_save = None;
            return;
        }
        if let Some(save) = self.pending_save.as_mut()
            && self.toasts.current_seq() == Some(save.seq)
            && matches!(
                self.toasts.current_action(),
                Some(ToastActionKind::SaveLogin)
            )
        {
            save.expires = now + Duration::from_secs(8);
        }
    }

    /// Encrypt and store the staged credential behind a live Save action,
    /// then refresh every surface that shows the data. Stale clicks retire
    /// the toast and store nothing. Existing pairs replace, never duplicate.
    fn save_staged_login(&mut self, seq: u64) {
        use zeroize::Zeroize;
        let now = Instant::now();
        let live = self
            .pending_save
            .as_ref()
            .is_some_and(|s| s.is_live(seq, now));
        if !live {
            self.toasts.dismiss(seq, now);
            self.push_chrome(true);
            return;
        }
        let mut staged = match self.pending_save.take() {
            Some(s) => s,
            None => {
                self.toasts.dismiss(seq, now);
                self.push_chrome(true);
                return;
            }
        };
        // Staging is consumed here. The taken buffer zeroizes below after
        // encrypting (and `Drop` covers every early return past this point
        // for the staging itself. This copy is the only one left).
        let mut password = staged.take_password();
        let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key()) else {
            password.zeroize();
            self.toasts.dismiss(seq, now);
            self.push_chrome(true);
            return;
        };
        let mut password_str = match String::from_utf8(std::mem::take(&mut password)) {
            Ok(s) => s,
            Err(_) => {
                key.zeroize();
                self.toasts.dismiss(seq, now);
                self.push_chrome(true);
                return;
            }
        };
        let saved = (|| {
            let rec = ferrite_credentials::CredentialRecord::new(
                &staged.origin,
                &staged.username,
                &password_str,
                &key,
            )
            .map_err(|e| e.to_string())?;
            if let Some(old) = store
                .get_for_origin(&staged.origin)
                .into_iter()
                .find(|r| r.username == staged.username)
            {
                let _ = store.delete(&old.id);
            }
            store.save(&rec).map_err(|e| e.to_string())
        })();
        password_str.zeroize();
        key.zeroize();
        self.toasts.dismiss(seq, now);
        match saved {
            Ok(()) => {
                for id in self.page_tabs("ferrite://passwords") {
                    self.push_passwords(id);
                }
                self.toasts.push(
                    crate::toast::ToastItem::saved("Login saved"),
                    Instant::now(),
                );
            }
            Err(e) => {
                debug!(error = %e, "staged login save failed");
                self.toasts.push(
                    crate::toast::ToastItem::failure("Couldn't save login".to_owned()),
                    Instant::now(),
                );
            }
        }
        self.push_chrome(true);
    }

    /// Toast action button (Undo, Open, Save). Stale seqs are ignored.
    /// Never applied to a newer toast.
    fn toast_action(&mut self, seq: u64) {
        use crate::toast::ToastActionKind as A;
        match self.toasts.action_for(seq) {
            Some(A::Undo) => self.undo_delete(seq),
            Some(A::SaveLogin) => self.save_staged_login(seq),
            Some(A::OpenDownloads { id }) => {
                let url = match id {
                    Some(i) => format!("ferrite://downloads?id={}", encode_query(&i)),
                    None => "ferrite://downloads".to_owned(),
                };
                self.toasts.dismiss(seq, Instant::now());
                self.navigate_to_toast(url);
            }
            None => {}
        }
    }

    /// Navigate for a toast Open action (mirrors the chrome Navigate arm:
    /// active tab loads, otherwise a new tab opens).
    fn navigate_to_toast(&mut self, url: String) {
        if let Some(tab) = self.app.tabs.active_tab_mut() {
            if let Err(e) = tab.load_url(url) {
                debug!(error = %e, "toast open failed");
            }
        } else {
            self.open_tab(url);
        }
        self.find_reset();
        self.layout_tabs();
        self.push_chrome(true);
    }

    /// Persist the whole broker snapshot to Vault. Single writer, the snapshot
    /// is the entire truth, so no row can drift. No-op without a persistent
    /// Vault (session memory still enforces, it just does not survive).
    fn persist_perm_snapshot(&self) {
        let Some(v) = self.vault.as_ref() else {
            return;
        };
        if !v.is_persistent() {
            return;
        }
        let snap = self.permissions.export_snapshot(ferrite_tabs::now_millis());
        let rows: Vec<ferrite_vault::vault::SitePermissionRow> = snap
            .into_iter()
            .map(|g| ferrite_vault::vault::SitePermissionRow {
                origin: g.origin,
                kind: g.kind,
                source: g.source.as_str().to_owned(),
                granted_at: g.granted_at_ms,
                expires_at: g.expires_at_ms,
            })
            .collect();
        if let Err(e) = v.replace_permission_grants(&rows) {
            debug!(error = %e, "permission snapshot persist failed");
        }
    }

    /// Push stored rows into a settings page `__ferritePermGrants`
    /// hook (called on load and after every grant or revoke). User grants
    /// and active auto-blocks both list. Revoking either is the reset.
    fn push_perm_grants(&self, tab_id: Uuid) {
        let now = ferrite_tabs::now_millis();
        let items: Vec<serde_json::Value> = self
            .permissions
            .export_snapshot(now)
            .iter()
            .map(|g| {
                serde_json::json!({
                    "origin": g.origin,
                    "kind": g.kind,
                    "label": ferrite_tabs::kind_label(&g.kind),
                    "source": g.source.as_str(),
                    "expires_at_ms": g.expires_at_ms,
                })
            })
            .collect();
        self.push_list(tab_id, "__ferritePermGrants", &items);
    }

    /// Push the trust graph into the settings page `__ferriteSyncDevices`
    /// hook (called on load and after every pairing decision, revocation,
    /// category toggle, or applied sync round). This device leads (flagged
    /// `is_self`), peers follow newest-first, revoked rows list as revoked
    /// (tombstones with history, never silently dropped), and a pending
    /// manual-pairing review rides along when one is open. Live mesh state
    /// (transport, discovered peers, their live tabs, categories, last
    /// round) rides in the same push so one hook owns the whole section.
    /// Without a vault the page gets `{unavailable:true}` instead.
    /// Off-LAN relay URLs from sealed settings (the `relay_urls` array).
    /// Empty/absent means LAN-only (the default): no relay is dialed,
    /// nothing leaves the local network for sync. Ephemeral sessions
    /// read no files, so they always resolve LAN-only.
    fn relay_urls_from_settings() -> Vec<String> {
        let mut key = Self::cred_key();
        let doc = crate::settings_file::read_settings_value(key.as_deref());
        if let Some(k) = key.as_mut() {
            k.zeroize();
        }
        doc.get("relay_urls")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .map(|joined| clean_relay_urls(&joined))
            .unwrap_or_default()
    }

    fn push_sync_devices(&self, tab_id: Uuid) {
        // Category rows for the toggles (TrustedDevices is mandatory and
        // renders locked-on; History stays opt-in by default).
        const CATEGORIES: &[(&str, &str)] = &[
            ("Bookmarks", "Bookmarks"),
            ("History", "History"),
            ("Credentials", "Passwords"),
            ("Autofill", "Autofill data"),
            ("Totp", "Authenticator codes"),
            ("Archive", "Archive snapshots"),
        ];
        let payload = match self.sync_trust.as_ref() {
            None => serde_json::json!({ "unavailable": true }),
            Some(t) => {
                let mut peers: Vec<serde_json::Value> = t
                    .store
                    .all()
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "id": d.device_id,
                            "name": d.device_name,
                            "fp": fingerprint_pubkey(&d.public_key),
                            "first_paired_at": d.first_paired_at,
                            "revoked": d.is_revoked,
                            "is_self": d.device_id == t.this.identity.device_id.to_string(),
                        })
                    })
                    .collect();
                sort_device_rows(&mut peers);
                let cats: Vec<serde_json::Value> = CATEGORIES
                    .iter()
                    .map(|(id, label)| {
                        let on = self
                            .sync_settings
                            .read()
                            .map(|s| {
                                s.is_enabled(match *id {
                                    "Bookmarks" => ferrite_sync::SyncCategory::Bookmarks,
                                    "History" => ferrite_sync::SyncCategory::History,
                                    "Credentials" => ferrite_sync::SyncCategory::Credentials,
                                    "Autofill" => ferrite_sync::SyncCategory::Autofill,
                                    "Totp" => ferrite_sync::SyncCategory::Totp,
                                    _ => ferrite_sync::SyncCategory::Archive,
                                })
                            })
                            .unwrap_or(false);
                        serde_json::json!({ "id": id, "label": label, "on": on })
                    })
                    .collect();
                let (transport, discovered, livetabs, last_round) = match self.sync_session.as_ref()
                {
                    None => (
                        if self.session.is_ephemeral() {
                            "off-incognito"
                        } else {
                            "off"
                        },
                        Vec::new(),
                        Vec::new(),
                        serde_json::Value::Null,
                    ),
                    Some(session) => (
                        "lan",
                        session
                            .peers()
                            .iter()
                            .map(|d| {
                                serde_json::json!({
                                    "id": d.device_id,
                                    "name": d.device_name,
                                    "fp": d.fingerprint,
                                    "last_seen_ms": d.last_seen_ms,
                                })
                            })
                            .collect(),
                        session
                            .live_tabs()
                            .iter()
                            .map(|a| match a {
                                ferrite_sync::LiveTabAction::PushTab {
                                    url, source_device, ..
                                } => {
                                    serde_json::json!({
                                        "url": url,
                                        "source": source_device,
                                    })
                                }
                            })
                            .collect(),
                        session.last_round().map_or(serde_json::Value::Null, |r| {
                            // Bulk catch-up without protocol changes: two
                            // consecutive non-empty rounds close together
                            // means convergence is still working through
                            // backlog. The streak lives here (Cell: this
                            // push takes &self), never on the wire.
                            let prev = self.sync_streak.get();
                            let catching = catching_up(
                                Some(prev).filter(|_| prev.1 > 0),
                                r.actions_received,
                                r.at_ms,
                            );
                            self.sync_streak.set((r.actions_received, r.at_ms));
                            serde_json::json!({
                                "peers_seen": r.peers_seen,
                                "peers_synced": r.peers_synced,
                                "actions_received": r.actions_received,
                                "deferred": r.deferred,
                                "at_ms": r.at_ms,
                                "catching_up": catching,
                            })
                        }),
                    ),
                };
                // Relay state, always from sealed settings (configured)
                // plus the live session (connected): the UI shows what
                // will be dialed and whether it is live, never secrets.
                let relay_live = self
                    .sync_session
                    .as_ref()
                    .and_then(|s| s.relay_status())
                    .map(|s| s.connected)
                    .unwrap_or(false);
                let relay = serde_json::json!({
                    "urls": Self::relay_urls_from_settings(),
                    "connected": relay_live,
                });
                serde_json::json!({
                    "self_id": t.this.identity.device_id.to_string(),
                    "self_name": t.this.name,
                    "self_fp": t.this.identity.fingerprint(),
                    "devices": peers,
                    "categories": cats,
                    "transport": transport,
                    "discovered": discovered,
                    "livetabs": livetabs,
                    "last_round": last_round,
                    "relay": relay,
                    "pending": self.pending_pair.as_ref().map(|p| serde_json::json!({
                        "id": p.device_id,
                        "name": p.device_name,
                        "fp": fingerprint_pubkey(&p.public_key),
                    })),
                })
            }
        };
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteSyncDevices({json})"))
        {
            debug!(error = %e, "sync devices push failed");
        }
    }

    /// Push a pairing code (or clear it) into the settings page
    /// `__ferriteSyncCode` hook. The code is public by design (device id +
    /// public key for manual exchange); approval still binds the key.
    /// `qr` carries the same code as server-rendered SVG for phone-camera
    /// scanning (rects only, no scripts), or null when it cannot encode.
    /// `expires_at_ms` lets the page retire the code on its own clock.
    fn push_sync_code(&self, tab_id: Uuid, code: Option<&str>, expires_at_ms: Option<i64>) {
        let json = sync_code_payload(code, expires_at_ms);
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteSyncCode({json})"))
        {
            debug!(error = %e, "sync code push failed");
        }
    }

    /// Push detected import sources (plus OS, for the platform-honest default
    /// browser copy) into the welcome page.
    fn push_welcome_detect(&self, tab_id: Uuid) {
        let items: Vec<serde_json::Value> = detect_browsers()
            .into_iter()
            .map(|s| serde_json::json!({"id": s.id, "label": s.label}))
            .collect();
        // Preferred source first is a client concern; the payload names
        // the OS default browser id (or null) so the badge is honest.
        let payload = serde_json::json!({
            "sources": items,
            "preferred": default_browser_id(),
            "os": std::env::consts::OS,
        });
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteWelcomeDetect({json})"))
        {
            debug!(error = %e, "welcome detect push failed");
        }
    }

    /// Push one import outcome back to the welcome page. Counts are exact
    /// (imported, skipped-identical, conflicts-kept). `label` is the human
    /// source name shown in the result line ("Chrome history", never the
    /// internal id): raw ids in user-facing text read as placeholder copy.
    /// `note` carries one honest caveat at most (recency gap, review
    /// pointer), never markup.
    fn push_welcome_result(
        &self,
        tab_id: Uuid,
        label: &str,
        imported: u64,
        skipped: u64,
        conflicts: u64,
        note: Option<String>,
    ) {
        let payload = serde_json::json!({
            "kind": label, "imported": imported,
            "skipped": skipped, "conflicts": conflicts,
            "note": note.unwrap_or_default(),
        });
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteWelcomeResult({json})"))
        {
            debug!(error = %e, "welcome result push failed");
        }
    }

    /// Detected-source import into the vault core (bookmarks/history):
    /// kind maps to the `import_from_path` core (`safari-*` have their
    /// own readers; `*-bookmarks` sniff Chromium JSON; the rest share the
    /// Chrome-then-Firefox history path).
    fn import_welcome_detected(&mut self, tab_id: Uuid, src: &ImportSource, kind: &str) {
        let import_kind = if kind == "safari-bookmarks" {
            "safari-bookmarks"
        } else if kind == "safari-history" {
            "safari-history"
        } else if kind.ends_with("-bookmarks") {
            "bookmarks"
        } else {
            "history"
        };
        let (report, mut note) = match self.vault.as_ref() {
            Some(vault) => {
                match import_from_path(vault, import_kind, std::path::Path::new(&src.path)) {
                    Ok(r) => (r, self.source_running_note(kind)),
                    Err(e) => (
                        ferrite_vault::import::ImportReport::default(),
                        Some(e.to_string()),
                    ),
                }
            }
            None => (
                ferrite_vault::import::ImportReport::default(),
                Some("Vault unavailable.".to_owned()),
            ),
        };
        if report.merged > 0 {
            let merge = format!(
                "{} visits merged with entries you already had.",
                report.merged
            );
            note = Some(match note {
                Some(n) => format!("{n} {merge}"),
                None => merge,
            });
        }
        info!(kind, ?report, "welcome import");
        self.push_welcome_result(
            tab_id,
            src.label,
            report.imported,
            report.skipped(),
            0,
            note,
        );
    }

    /// Welcome engine import: site searches from a Chromium Web Data file
    /// into custom engines (template validation inside `add_custom`),
    /// then persisted. Counts added vs skipped for the result row.
    fn import_welcome_engines(&mut self, tab_id: Uuid, src: &ImportSource) {
        let note = self.source_running_note(src.id);
        let engines = match ferrite_vault::import::read_chrome_engines(&src.path) {
            Ok(e) => e,
            Err(e) => {
                self.push_welcome_result(tab_id, src.label, 0, 0, 0, Some(e.to_string()));
                return;
            }
        };
        let (mut added, mut skipped) = (0, 0);
        for eng in engines {
            match self
                .app
                .omnibox
                .registry
                .add_custom(&eng.name, &eng.template)
            {
                Ok(_) => added += 1,
                Err(_) => skipped += 1,
            }
        }
        if added > 0 {
            self.persist_customs();
        }
        info!(kind = src.id, added, skipped, "welcome engine import");
        self.push_welcome_result(tab_id, src.label, added, skipped, 0, note);
    }

    /// Welcome address import: Chromium autofill profiles into the
    /// address book (explicit one-click consent; cards/CVCs never read).
    /// Dedupes on name+street+postal+country; counts saved vs skipped.
    fn import_welcome_addresses(&mut self, tab_id: Uuid, src: &ImportSource) {
        let note = self.source_running_note(src.id);
        let addrs = match ferrite_vault::import::read_chrome_addresses(&src.path) {
            Ok(a) => a,
            Err(e) => {
                self.push_welcome_result(tab_id, src.label, 0, 0, 0, Some(e.to_string()));
                return;
            }
        };
        let (store, mut key) = match (self.autofill_store(), Self::cred_key()) {
            (Some(s), Some(k)) => (s, k),
            _ => {
                self.push_welcome_result(
                    tab_id,
                    src.label,
                    0,
                    0,
                    0,
                    Some("Vault unavailable.".to_owned()),
                );
                return;
            }
        };
        let mut known: std::collections::HashSet<String> = store
            .list(ferrite_credentials::AutofillKind::Address, &key)
            .into_iter()
            .filter_map(|r| {
                serde_json::from_str::<ferrite_credentials::AddressFields>(&r.fields_json).ok()
            })
            .map(|f| address_dedupe_key(&f.name, &f.street, &f.postal, &f.country))
            .collect();
        let (mut added, mut skipped) = (0, 0);
        for a in addrs {
            let fields = address_fields_from_chrome(&a);
            let key_str = address_dedupe_key(
                &fields.name,
                &fields.street,
                &fields.postal,
                &fields.country,
            );
            if known.contains(&key_str) {
                skipped += 1;
                continue;
            }
            match store.save_address(&fields, &key) {
                Ok(_) => {
                    added += 1;
                    known.insert(key_str);
                }
                Err(_) => skipped += 1,
            }
        }
        // Scrub the key like every other holder (in-place zeroize, same
        // discipline as the autofill_add_address arm).
        key.zeroize();
        info!(kind = src.id, added, skipped, "welcome address import");
        self.push_welcome_result(tab_id, src.label, added, skipped, 0, note);
    }

    /// Recency caveat for a detected-source import, if the owning browser
    /// is currently running (its DB copy may miss the last few minutes).
    /// One sentence or `None`. The temp-copy read itself is always consistent.
    fn source_running_note(&mut self, kind: &str) -> Option<String> {
        let names: &[&str] = if kind.starts_with("chrome") {
            &["chrome", "chrome.exe"]
        } else if kind.starts_with("edge") {
            &["msedge", "msedge.exe"]
        } else if kind.starts_with("firefox") {
            &["firefox", "firefox.exe"]
        } else if kind.starts_with("safari") {
            &["Safari"]
        } else {
            return None;
        };
        self.sys
            .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let running = names.iter().any(|n| {
            self.sys
                .processes_by_name(std::ffi::OsStr::new(n))
                .next()
                .is_some()
        });
        running.then(|| {
            "That browser is still running. The last few minutes may be missing. Close it and re-import for the rest.".to_owned()
        })
    }

    /// Portable "export everything": one tarball with the per-category
    /// files (bookmarks HTML, passwords CSV, settings JSON). Plaintext
    /// by nature, the page confirms the cost before posting.
    fn build_portable_tar(&self) -> Result<Vec<u8>, String> {
        let vault = self.vault.as_ref().ok_or("Vault unavailable")?;
        let html = ferrite_vault::import::export_bookmarks_netscape(vault);
        let mut rows: Vec<(String, String, String)> = Vec::new();
        if let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key()) {
            for rec in store.all() {
                if let Some(pw) = rec.decrypt_password(&key) {
                    rows.push((rec.url.clone(), rec.username.clone(), pw));
                }
            }
            key.zeroize();
        }
        let csv = ferrite_credentials::export_csv(&rows);
        for (_, _, pw) in rows.iter_mut() {
            pw.zeroize();
        }
        // Explicit user export: the sealed document decrypted for the
        // tarball (same plaintext-by-consent class as the CSV above).
        let mut export_key = Self::cred_key();
        let settings = crate::settings_file::read_settings_value(export_key.as_deref());
        if let Some(k) = export_key.as_mut() {
            k.zeroize();
        }
        let settings = serde_json::to_string_pretty(&settings).unwrap_or_else(|_| "{}".to_owned());
        crate::export::write_tar(&[
            ("ferrite-bookmarks.html", html.as_bytes()),
            ("ferrite-passwords.csv", csv.as_bytes()),
            ("ferrite-settings.json", settings.as_bytes()),
        ])
    }

    /// Encrypted native backup for a new device: full-fidelity envelope
    /// sealed with the passphrase. Plaintext lives only in memory and is
    /// zeroized before return. TOTP is excluded (no runtime write path).
    fn build_sealed_backup(&self, passphrase: &str) -> Result<Vec<u8>, String> {
        use ferrite_sync::backup as backup_crypto;
        let vault = self.vault.as_ref().ok_or("Vault unavailable")?;
        let mut passwords: Vec<(String, String, String)> = Vec::new();
        let mut cards: Vec<serde_json::Value> = Vec::new();
        let mut addresses: Vec<serde_json::Value> = Vec::new();
        if let (Some(cstore), Some(mut key)) = (self.cred_store(), Self::cred_key()) {
            for rec in cstore.all().iter().take(10_000) {
                if let Some(pw) = rec.decrypt_password(&key) {
                    passwords.push((rec.url.clone(), rec.username.clone(), pw));
                }
            }
            key.zeroize();
        }
        if let (Some(astore), Some(mut key)) = (self.autofill_store(), Self::cred_key()) {
            use ferrite_credentials::AutofillKind;
            for kind in [AutofillKind::Card, AutofillKind::Address] {
                for r in astore.list(kind, &key).iter().take(5_000) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.fields_json) {
                        if kind == AutofillKind::Card {
                            cards.push(v);
                        } else {
                            addresses.push(v);
                        }
                    }
                }
            }
            key.zeroize();
        }
        let history: Vec<crate::export::HistoryRow> = vault
            .export_history_rows(20_000)
            .into_iter()
            .map(|e| crate::export::HistoryRow {
                url: e.url,
                title: e.title.unwrap_or_default(),
                visited_ms: Some(e.last_visit),
                visits: e.visit_count as i64,
            })
            .collect();
        let mut export_key = Self::cred_key();
        let settings = crate::settings_file::read_settings_value(export_key.as_deref());
        if let Some(k) = export_key.as_mut() {
            k.zeroize();
        }
        let mut parts = crate::export::EnvelopeParts {
            created_ms: ferrite_tabs::now_millis(),
            bookmarks_html: ferrite_vault::import::export_bookmarks_netscape(vault),
            passwords,
            history,
            cards,
            addresses,
            settings,
        };
        let envelope = crate::export::build_envelope(&parts);
        for (_, _, pw) in parts.passwords.iter_mut() {
            pw.zeroize();
        }
        let mut raw = serde_json::to_vec(&envelope).map_err(|e| format!("envelope encode: {e}"))?;
        let sealed = backup_crypto::seal_backup_file(&raw, passphrase);
        raw.zeroize();
        sealed
    }

    /// Restore a sealed backup into the live stores, reusing the import
    /// write paths. Conflicts queue on the passwords page. Settings apply
    /// last so data lands even if prefs reject.
    fn apply_backup_envelope(
        &mut self,
        tab_id: Uuid,
        env: &serde_json::Value,
    ) -> Result<RestoreCounts, String> {
        use ferrite_credentials::{AutofillKind, ImportVerdict};
        let vault = self.vault.clone().ok_or("Vault unavailable")?;
        let mut c = RestoreCounts::default();
        if let Some(html) = env.get("bookmarks_html").and_then(|x| x.as_str()) {
            let tmp = std::env::temp_dir().join(format!(
                "ferrite-restore-{}-bookmarks.html",
                std::process::id()
            ));
            std::fs::write(&tmp, html).map_err(|e| format!("restore scratch: {e}"))?;
            // Hierarchical HTML restores folders verbatim (same parser as
            // file imports, duplicates skipped, counts exact).
            let report = ferrite_vault::import::import_netscape_bookmarks(
                tmp.to_str().unwrap_or(""),
                &vault,
            )
            .map_err(|e| format!("backup bookmarks unreadable: {e}"))?;
            c.bookmarks = report.imported;
            c.skipped += report.skipped();
            let _ = std::fs::remove_file(&tmp);
        }
        if let Some(arr) = env.get("history").and_then(|x| x.as_array()) {
            for it in arr.iter().take(100_000) {
                if let Some(url) = it.get("url").and_then(|x| x.as_str()) {
                    let title = it.get("title").and_then(|x| x.as_str());
                    // New envelopes carry the original visit; old ones
                    // (url+title only) land with "now", as before.
                    let visited = it.get("visited_ms").and_then(|x| x.as_i64());
                    let visits = it.get("visits").and_then(|x| x.as_i64()).unwrap_or(1);
                    match vault.import_history_row(url, title, "link", visited, visits) {
                        Ok(true) => c.history += 1,
                        Ok(false) => c.skipped += 1,
                        Err(_) => c.skipped += 1,
                    }
                }
            }
        }
        if let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key()) {
            let split = crate::export::split_envelope(env)?;
            let (rows, pws) = (split.rows, split.secrets);
            for ((url, username), pw) in rows.iter().zip(pws.iter()).take(10_000) {
                match ferrite_credentials::CredentialRecord::new(url, username, pw, &key) {
                    Ok(rec) => match store.import_row(&rec, &key) {
                        ImportVerdict::Imported => c.passwords += 1,
                        ImportVerdict::SkippedIdentical => c.skipped += 1,
                        ImportVerdict::ConflictKeptExisting => {
                            // ponytail: queue capped. Beyond it conflicts
                            // count as skipped rather than growing memory
                            // without bound. Raise with a resolve-page
                            // that pages instead of listing all at once.
                            if self.pw_conflicts.len() < 500 {
                                self.pw_conflicts.push(PendingPasswordConflict {
                                    origin: url.clone(),
                                    username: username.clone(),
                                    password: pw.clone(),
                                });
                                c.conflicts += 1;
                            } else {
                                c.skipped += 1;
                            }
                        }
                        ImportVerdict::Failed => c.skipped += 1,
                    },
                    Err(_) => c.skipped += 1,
                }
            }
            // Autofill: exact-JSON dedupe, then the validated save paths.
            if let (Some(astore), Some(mut akey)) = (self.autofill_store(), Self::cred_key()) {
                let known: std::collections::HashSet<String> =
                    [AutofillKind::Card, AutofillKind::Address]
                        .into_iter()
                        .flat_map(|k| astore.list(k, &akey))
                        .map(|r| r.fields_json)
                        .collect();
                if let Some(arr) = env.get("cards").and_then(|x| x.as_array()) {
                    for cv in arr {
                        let s = serde_json::to_string(cv).unwrap_or_default();
                        if known.contains(&s) {
                            continue;
                        }
                        if let Ok(mut f) =
                            serde_json::from_value::<ferrite_credentials::CardFields>(cv.clone())
                        {
                            if astore.save_card(&mut f, &akey).is_ok() {
                                c.cards += 1;
                            }
                        }
                    }
                }
                if let Some(arr) = env.get("addresses").and_then(|x| x.as_array()) {
                    for av in arr {
                        let s = serde_json::to_string(av).unwrap_or_default();
                        if known.contains(&s) {
                            continue;
                        }
                        if let Ok(f) =
                            serde_json::from_value::<ferrite_credentials::AddressFields>(av.clone())
                        {
                            if astore.save_address(&f, &akey).is_ok() {
                                c.addresses += 1;
                            }
                        }
                    }
                }
                akey.zeroize();
            }
            key.zeroize();
        }
        if let Some(settings) = env.get("settings") {
            c.settings = self.write_and_apply_settings(tab_id, settings);
        }
        for id in self.page_tabs("ferrite://passwords") {
            self.push_passwords(id);
        }
        Ok(c)
    }

    /// Validate plus persist plus apply a settings object (shared by the JSON
    /// import post and backup restore). Imported keys merge over the sealed
    /// document; absent keys keep their stored values, so a partial file
    /// can never wipe unrelated preferences. Plaintext imports are a
    /// user-directed migration into the encrypted store. Ephemeral
    /// sessions apply in memory without persisting.
    fn write_and_apply_settings(&mut self, tab_id: Uuid, value: &serde_json::Value) -> bool {
        if !value.is_object() {
            return false;
        }
        if is_ephemeral() {
            self.apply_settings_doc(value);
            self.push_settings(tab_id);
            return true;
        }
        let mut keyring = Self::cred_key();
        let ok = match keyring.as_deref() {
            Some(k) => {
                let mut doc = crate::settings_file::read_settings_value(Some(k));
                merge_settings_doc(&mut doc, value);
                crate::settings_file::write_settings_value(&doc, k)
            }
            None => false,
        };
        if let Some(k) = keyring.as_mut() {
            k.zeroize();
        }
        if ok {
            self.apply_settings_doc(value);
            self.push_settings(tab_id);
        }
        ok
    }

    /// Restore one sealed backup file: open, verify, apply, report. The
    /// passphrase copy is for the caller to zeroize.
    fn restore_backup_file(&mut self, path: &std::path::Path, passphrase: &str, tab_id: Uuid) {
        use ferrite_sync::backup as backup_crypto;
        let mut raw = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.notify(format!("Couldn't read backup: {e}"));
                return;
            }
        };
        let plain = match backup_crypto::open_backup_file(&raw, passphrase) {
            Ok(p) => p,
            Err(e) => {
                raw.zeroize();
                self.notify(format!("Couldn't open backup: {e}"));
                return;
            }
        };
        raw.zeroize();
        let env: serde_json::Value = match serde_json::from_slice(&plain) {
            Ok(v) => v,
            Err(_) => {
                self.notify("Backup contents are corrupt.".to_owned());
                return;
            }
        };
        match self.apply_backup_envelope(tab_id, &env) {
            Ok(c) => self.notify(format!(
                "Backup restored: {} bookmarks, {} history, {} passwords ({} already saved, {} need a choice at ferrite://passwords), {} cards, {} addresses{}.",
                c.bookmarks,
                c.history,
                c.passwords,
                c.skipped,
                c.conflicts,
                c.cards,
                c.addresses,
                if c.settings { ", settings applied" } else { "" },
            )),
            Err(e) => self.notify(format!("Couldn't restore backup: {e}")),
        }
    }

    /// Credentials key: the keyring master key, loaded per operation and
    /// zeroized after use, never retained in shell state.
    // ponytail: same trust boundary as the vault today. Dedicated credentials
    // key, master-password wrap, lock model, and re-auth gates are tracked.
    fn cred_key() -> Option<Vec<u8>> {
        ferrite_vault::keyring::load_master_key().ok()?
    }

    fn cred_store(&self) -> Option<ferrite_credentials::CredentialStore> {
        self.vault
            .as_ref()
            .map(|v| ferrite_credentials::CredentialStore::new(v.clone()))
    }

    /// Push both record lists to a manager tab. Cards travel masked
    /// (network plus last4 plus name plus expiry). Full PANs only move at fill
    /// time, straight into the field.
    fn push_autofill(&self, tab_id: Uuid) {
        use ferrite_credentials::AutofillKind;
        let (addresses, cards) = match (self.autofill_store(), Self::cred_key()) {
            (Some(store), Some(mut key)) => {
                let a: Vec<serde_json::Value> = store
                    .list(AutofillKind::Address, &key)
                    .into_iter()
                    .map(|r| {
                        let fields: serde_json::Value =
                            serde_json::from_str(&r.fields_json).unwrap_or(serde_json::Value::Null);
                        serde_json::json!({
                            "id": r.id, "label": r.label,
                            "added": r.added, "fields": fields,
                        })
                    })
                    .collect();
                let c: Vec<serde_json::Value> = store
                    .list(AutofillKind::Card, &key)
                    .into_iter()
                    .map(|r| {
                        let f: serde_json::Value =
                            serde_json::from_str(&r.fields_json).unwrap_or(serde_json::Value::Null);
                        let pan = f.get("pan").and_then(|x| x.as_str()).unwrap_or("");
                        let n = pan.len();
                        serde_json::json!({
                            "id": r.id, "label": r.label, "added": r.added,
                            "network": f.get("network").and_then(|x| x.as_str()).unwrap_or(""),
                            "last4": if n > 4 { &pan[n - 4..] } else { pan },
                            "name": f.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                            "expiry": f.get("expiry").and_then(|x| x.as_str()).unwrap_or(""),
                        })
                    })
                    .collect();
                key.zeroize();
                (a, c)
            }
            _ => (Vec::new(), Vec::new()),
        };
        let payload = serde_json::json!({"addresses": addresses, "cards": cards});
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteAutofill({json})"))
        {
            debug!(error = %e, "autofill push failed");
        }
    }

    /// Fan-out: open manager tabs re-list after any add/delete.
    fn push_autofill_tabs(&self) {
        for id in self.page_tabs("ferrite://autofill") {
            self.push_autofill(id);
        }
    }

    fn push_taskmanager(&self, tab_id: Uuid) {
        let payload = self.taskmanager_json();
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteTaskman({json})"))
        {
            debug!(error = %e, "taskmanager push failed");
        }
    }

    /// Re-run the viewer fetch for `tab_id` when its target host is `host`
    /// (post trust or revoke refresh). Host must match the own viewer
    /// target of the tab. Never a caller-supplied fetch of an arbitrary origin.
    fn refresh_cert_report(&mut self, tab_id: Uuid, host: &str) {
        let target = self
            .app
            .tabs
            .get_tab(tab_id)
            .map(|t| t.url.clone())
            .and_then(|u| {
                let q = u.strip_prefix("ferrite://certs")?.trim_start_matches('?');
                ferrite_tabs::protocol::decode_query_param(q, "url")
            })
            .and_then(|u| crate::cert::https_host(&u))
            .filter(|(h, _)| h == host);
        if let Some((h, port)) = target {
            self.cert_pending.insert(tab_id);
            let now = ferrite_tabs::now_millis();
            let exceptions = self
                .vault
                .as_ref()
                .and_then(|vault| vault.list_cert_exceptions().ok())
                .unwrap_or_default()
                .into_iter()
                .map(|r| crate::cert::ExceptionEntry {
                    host: r.host,
                    sha256: r.sha256,
                    expires_at: r.expires_at,
                })
                .collect();
            crate::cert::spawn_cert_fetch(h, port, tab_id, self.cert_tx.clone(), exceptions, now);
        }
    }

    /// Blocked-page target host of this tab (`?host=` of ferrite://blocked).
    /// Trust and report actions must match it. Same anti-spoof shape as the
    /// cert viewer fetched-report check.
    fn blocked_target_host(&self, tab_id: Uuid) -> Option<String> {
        self.app
            .tabs
            .get_tab(tab_id)
            .map(|t| t.url.clone())
            .and_then(|u| {
                let q = u.strip_prefix("ferrite://blocked")?.trim_start_matches('?');
                ferrite_tabs::protocol::decode_query_param(q, "host")
            })
            .filter(|h| !h.is_empty())
    }

    /// Push interstitial detail for `host`: trigger-aware copy data,
    /// override state, and report history. Absent or expired detail renders
    /// the confirmed-list copy, never a stale suspicion.
    fn push_blocked_detail(&self, tab_id: Uuid, host: &str) {
        let now = ferrite_tabs::now_millis();
        let (why, matched, action) = match self.privacy.block_detail(host, now) {
            Some(ferrite_privacy::BlockWhy::Lookalike { matched }) => {
                ("lookalike", matched, String::new())
            }
            Some(ferrite_privacy::BlockWhy::FormRisk { action_host }) => {
                ("form", String::new(), action_host)
            }
            _ => ("malware", String::new(), String::new()),
        };
        let (trusted, reported) = self
            .vault
            .as_ref()
            .map(|v| {
                (
                    v.list_threat_exceptions()
                        .map(|hs| hs.iter().any(|h| h == host))
                        .unwrap_or(false),
                    v.threat_report_count(host) > 0,
                )
            })
            .unwrap_or((false, false));
        let json = serde_json::json!({
            "host": host,
            "why": why,
            "matched": matched,
            "action": action,
            "trusted": trusted,
            "reported": reported,
        })
        .to_string()
        .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteBlocked({json})"))
        {
            debug!(error = %e, "blocked detail push failed");
        }
    }

    /// Push one cert report into its viewer tab.
    fn push_cert(&self, tab_id: Uuid, rep: &crate::cert::CertReport) {
        let json = serde_json::to_string(rep)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteCerts({json})"))
        {
            debug!(error = %e, "cert push failed");
        }
    }

    /// Push one view-source report (escaped blob + wrap pref) into its tab.
    fn push_viewsource(&self, tab_id: Uuid, rep: &crate::viewsource::VsReport) {
        let json = serde_json::to_string(&serde_json::json!({
            "url": rep.url,
            "content_type": rep.content_type,
            "truncated": rep.truncated,
            "escaped": rep.escaped,
            "error": rep.error,
            "wrap": self.viewsource_wrap,
        }))
        .unwrap_or_default()
        .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteViewSource({json})"))
        {
            debug!(error = %e, "viewsource push failed");
        }
    }

    /// Fire one on-device translation run in a tab. The page-local engine
    /// does the work. Rust passes only the validated target, and only for
    /// the active tab (stale or background triggers are refused).
    fn run_translate(&self, tab: Uuid, mode: ferrite_tabs::translate::TranslateMode) {
        if Some(tab) != self.app.tabs.active_id() {
            return;
        }
        // Hosts that proved incapable this session don't get another
        // late failure: the offer is already suppressed alongside this.
        if self
            .app
            .tabs
            .get_tab(tab)
            .map(|t| host_slice(&t.url).to_owned())
            .is_some_and(|h| self.translate_unavailable.contains(&h))
        {
            return;
        }
        let eval = ferrite_tabs::translate::translate_eval(mode, &self.translate_target);
        if let Some(t) = self.app.tabs.get_tab(tab)
            && let Some(h) = t.webview.as_ref()
            && let Err(e) = h.evaluate_script(&eval)
        {
            debug!(error = %e, "translate trigger failed");
        }
    }

    /// Push one reader article (plus current typography prefs, so the page
    /// needs no extra round trip).
    fn push_reader(&self, tab_id: Uuid, article: &ReaderArticle) {
        let payload = serde_json::json!({
            "title": article.title,
            "byline": article.byline,
            "site": article.site,
            "minutes": article.minutes,
            "html": article.html,
            "prefs": {
                "size": self.reader_size,
                "serif": self.reader_serif,
                "wide": self.reader_wide,
                "theme": self.reader_theme,
                "translate_target": self.translate_target,
            },
        });
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteReader({json})"))
        {
            debug!(error = %e, "reader push failed");
        }
    }

    /// Cache an article by source URL (capped, oldest evicted first).
    fn cache_reader(&mut self, src: String, article: ReaderArticle) {
        self.reader_cache.retain(|(s, _)| s != &src);
        self.reader_cache.push_back((src, article));
        while self.reader_cache.len() > 5 {
            self.reader_cache.pop_front();
        }
    }

    /// Open the reader on a cached article (new tab, activated).
    fn open_reader(&mut self, src: &str) {
        self.open_tab(format!("ferrite://reader?src={}", encode_query(src)));
    }

    /// Extraction metadata: start assembly, or show the empty state for
    /// non-article pages (an explicit gesture always gets an answer).
    fn handle_reader_result(&mut self, tab_id: Uuid, v: &serde_json::Value) {
        let Some(asm) = self.reader_pending.remove(&tab_id) else {
            debug!(tab_id = %tab_id, "ipc: unsolicited reader result ignored");
            return;
        };
        if !v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
            let src = asm.src.clone();
            self.cache_reader(src.clone(), ReaderArticle::default());
            self.open_reader(&src);
            return;
        }
        let total = v
            .get("chunks")
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            .min(64) as usize;
        if total == 0 {
            let src = asm.src.clone();
            self.cache_reader(src.clone(), ReaderArticle::default());
            self.open_reader(&src);
            return;
        }
        let length = v.get("length").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        self.reader_pending.insert(
            tab_id,
            ReaderAssembly {
                title: capped_field(v, "title", 200),
                byline: capped_field(v, "byline", 200),
                site: capped_field(v, "site", 200),
                length,
                total,
                src: asm.src,
                parts: vec![None; total],
            },
        );
    }

    /// One body chunk: complete means cache plus open, mismatch means drop.
    fn handle_reader_chunk(&mut self, tab_id: Uuid, v: &serde_json::Value) {
        let Some(asm) = self.reader_pending.get_mut(&tab_id) else {
            return;
        };
        let seq = v
            .get("seq")
            .and_then(|x| x.as_u64())
            .unwrap_or(usize::MAX as u64) as usize;
        let total = v.get("total").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        if total != asm.total || seq >= asm.total {
            return;
        }
        asm.parts[seq] = Some(capped_field(v, "html", 25000));
        if asm.parts.iter().all(|p| p.is_some()) {
            let asm = self.reader_pending.remove(&tab_id).unwrap_or_default();
            let html = asm
                .parts
                .into_iter()
                .map(|p| p.unwrap_or_default())
                .collect::<String>();
            let minutes = ((asm.length / 5).max(1) / 200).max(1) as u32;
            let src = asm.src.clone();
            self.cache_reader(
                src.clone(),
                ReaderArticle {
                    title: asm.title,
                    byline: asm.byline,
                    site: asm.site,
                    minutes,
                    html,
                },
            );
            self.open_reader(&src);
        }
    }

    /// Our own page source, verbatim from the bundled consts (no fetch,
    /// no network, auditable build).
    fn internal_source(url: &str) -> Option<&'static str> {
        use ferrite_tabs::protocol as p;
        let name = url
            .strip_prefix("ferrite://")?
            .split(['?', '#', '/'])
            .next()?;
        Some(match name {
            "newtab" => p::NEWTAB_HTML,
            "history" => p::HISTORY_HTML,
            "downloads" => p::DOWNLOADS_HTML,
            "bookmarks" => p::BOOKMARKS_HTML,
            "passwords" => p::PASSWORDS_HTML,
            "autofill" => p::AUTOFILL_HTML,
            "settings" => p::SETTINGS_HTML,
            "about" => p::ABOUT_HTML,
            "taskmanager" => p::TASKMANAGER_HTML,
            "certs" => p::CERTS_HTML,
            "credits" => p::CREDITS_HTML,
            "tabsearch" => p::TABSEARCH_HTML,
            _ => return None,
        })
    }

    /// Task-manager snapshot: our process (kind browser) plus direct children
    /// with CPU and memory, plus the full tab inventory with states. No engine
    /// API maps tabs to PIDs, so the page shows both tables and a count line.
    // ponytail: sysinfo is the whole process backend. Per-frame CPU wants
    // raw COM and is the upgrade path on Windows.
    fn taskmanager_json(&self) -> serde_json::Value {
        let me = sysinfo::get_current_pid().ok();
        let mut procs: Vec<serde_json::Value> = Vec::new();
        let (mut mem_total, mut cpu_total) = (0u64, 0.0f32);
        for (pid, p) in self.sys.processes() {
            let is_self = Some(*pid) == me;
            if !is_self && p.parent() != me {
                continue;
            }
            let cmd = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let kind = if is_self {
                "browser"
            } else {
                classify_child(&cmd)
            };
            mem_total += p.memory();
            cpu_total += p.cpu_usage();
            procs.push(serde_json::json!({
                "pid": pid.as_u32(),
                "kind": kind,
                "name": p.name().to_string_lossy(),
                "cpu": (p.cpu_usage() * 10.0).round() / 10.0,
                "mem": p.memory(),
            }));
        }
        let active = self.app.tabs.active_id();
        let tabs: Vec<serde_json::Value> = self
            .app
            .tabs
            .iter()
            .map(|t| {
                let state = if t.is_crashed {
                    "crashed"
                } else if t.is_discarded() {
                    "inactive"
                } else if t.is_loading {
                    "loading"
                } else {
                    "active"
                };
                // Idle seconds since suspension (discard countdown input);
                // null while live. Exemption reason explains "why won't
                // this tab sleep" without opening settings.
                let idle_secs = t.suspended_since.map(|s| s.elapsed().as_secs());
                let why = self.app.tabs.sleep_exempt_reason(t);
                serde_json::json!({
                    "id": t.id.to_string(),
                    "title": t.title,
                    "url": t.url,
                    "state": state,
                    "active": Some(t.id) == active,
                    "muted": t.is_muted,
                    "pinned": t.is_pinned,
                    "discards": t.discard_count,
                    "idle_secs": idle_secs,
                    "auto_sleep": why.is_none(),
                    "sleep_why": why,
                })
            })
            .collect();
        let renderers = procs.iter().filter(|p| p["kind"] == "renderer").count();
        serde_json::json!({
            "processes": procs,
            "tabs": tabs,
            "renderers": renderers,
            "tab_count": tabs.len(),
            "mem_total": mem_total,
            "cpu_total": (cpu_total * 10.0).round() / 10.0,
            // Engine profile bytes (HTTP cache, cookies, storage):
            // engine-owned and unencrypted; cleared via Settings →
            // Clear browsing data. Measured at most once a minute.
            "engine_profile_bytes": self.engine_profile_bytes,
            // Filter lists: source bytes + rule lines per list. Bytes are
            // a floor for the compiled index (the adblock crate exposes
            // no heap API); the page labels them as source, not memory.
            "filter_lists": self.privacy.filter_list_stats().into_iter().map(|(name, bytes, rules)| {
                serde_json::json!({"name": name, "bytes": bytes, "rules": rules})
            }).collect::<Vec<_>>(),
        })
    }

    /// One-shot form error on the manager page (validation lives in Rust).
    fn push_autofill_error(&self, tab_id: Uuid, message: &str) {
        let json = serde_json::to_string(&serde_json::json!({"message": message}))
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteAutofillError({json})"))
        {
            debug!(error = %e, "autofill error push failed");
        }
    }

    /// Push the credential list (metadata only, never plaintext) plus any
    /// queued import conflicts (origin plus username only. The imported
    /// password stays Rust-side until resolved).
    fn push_passwords(&self, tab_id: Uuid) {
        let items: Vec<serde_json::Value> = match self.cred_store() {
            Some(store) => store
                .all()
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id, "url": r.url,
                        "username": r.username, "added": r.hlc_timestamp,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        self.push_list(tab_id, "__ferritePasswords", &items);
        let conflicts: Vec<serde_json::Value> = self
            .pw_conflicts
            .iter()
            .enumerate()
            .map(|(i, c)| serde_json::json!({"id": i, "url": c.origin, "username": c.username}))
            .collect();
        self.push_list(tab_id, "__ferritePasswordConflicts", &conflicts);
    }

    /// Tabs currently showing an internal page (live-push targets).
    fn page_tabs(&self, base: &str) -> Vec<Uuid> {
        self.app
            .tabs
            .iter()
            .filter(|t| singleton_match(&t.url, base))
            .map(|t| t.id)
            .collect()
    }

    /// Downloads rows for a filter plus status (`failed` lands pre-filtered).
    /// Durable vault rows first, then live in-progress events not yet
    /// finalized (same id wins once).
    fn downloads_json(&self, q: &str, status: &str) -> Vec<serde_json::Value> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut items: Vec<serde_json::Value> = Vec::new();
        if let Some(vault) = self.vault.as_ref() {
            for r in vault.list_downloads(200) {
                if !q.is_empty()
                    && !r.filename.to_lowercase().contains(q)
                    && !r.url.to_lowercase().contains(q)
                {
                    continue;
                }
                if !status.is_empty() && r.state.to_lowercase() != status {
                    continue;
                }
                seen.insert(r.id.clone());
                items.push(serde_json::json!({
                    "id": r.id,
                    "filename": r.filename,
                    "url": r.url,
                    "path": r.path,
                    "state": r.state,
                    "size": r.size_bytes,
                    "hash": r.sha256_hex,
                    "ts": r.downloaded_at,
                    "danger": r.is_dangerous,
                }));
            }
        }
        for e in self.download_log.iter().rev().take(50) {
            use ferrite_tabs::DownloadStateKind as Ds;
            if !matches!(
                e.state,
                Ds::Started | Ds::Progress | Ds::Paused | Ds::Interrupted
            ) {
                continue;
            }
            if seen.contains(&e.id) {
                continue;
            }
            if !q.is_empty()
                && !e.filename.to_lowercase().contains(q)
                && !e.url.to_lowercase().contains(q)
            {
                continue;
            }
            let state = format!("{:?}", e.state);
            if !status.is_empty() && state.to_lowercase() != *status {
                continue;
            }
            items.push(serde_json::json!({
                "id": e.id,
                "filename": e.filename,
                "url": e.url,
                "path": e.path,
                "state": state,
                "size": e.bytes_total,
                "hash": null,
                "ts": now_millis(),
                "danger": ferrite_tabs::is_dangerous_ext(&e.filename),
            }));
        }
        items
    }

    /// Record a visit for history plus autocomplete. Best-effort throughout,
    /// single choke point (params stripped, internal and excluded origins
    /// skipped, private tabs never record). Transition is `typed` or `link`.
    fn record_visit(&mut self, url: &str, transition: &str, title: Option<&str>) {
        if let Some(id) = self.app.tabs.active_id() {
            self.record_visit_for_tab(id, url, transition, title);
        }
    }

    /// Per-tab visit choke: same filters as [`Self::record_visit`], against
    /// the given tab id (which may not be active). Consumes a pending
    /// restore-suppression. Returns true when a row was written.
    fn record_visit_for_tab(
        &mut self,
        tab_id: uuid::Uuid,
        url: &str,
        transition: &str,
        title: Option<&str>,
    ) -> bool {
        if url.starts_with("ferrite://") {
            return false;
        }
        let suppressed_or_private = match self.app.tabs.get_tab_mut(tab_id) {
            Some(t) if t.is_private => true,
            Some(t) if t.suppress_next_history => {
                t.suppress_next_history = false;
                true
            }
            Some(_) => false,
            None => true,
        };
        if suppressed_or_private {
            return false;
        }
        let clean = ferrite_privacy::js::strip_tracking_params(url);
        let Some(vault) = self.vault.as_ref() else {
            return false;
        };
        if vault.history_excluded(&clean) {
            return false;
        }
        // Retry rows deferred during writer bursts, oldest-first, before
        // the fresh row. A row that still won't fit goes back to the
        // front; order preserved, nothing reordered past the live visit.
        let mut retries = self.pending_history.len();
        while retries > 0 {
            retries -= 1;
            let Some((u, t, tr)) = self.pending_history.pop_front() else {
                break;
            };
            if vault.insert_history(&u, t.as_deref(), &tr).is_err() {
                self.pending_history.push_front((u, t, tr));
                break;
            }
        }
        if let Err(e) = vault.insert_history(&clean, title, transition) {
            // Burst past the writer channel: the RAM cache above already
            // holds this visit, so stash the disk row for a later tick
            // instead of losing it silently.
            debug!(error = %e, "history record deferred to spillover");
            self.pending_history.push_back((
                clean.clone(),
                title.map(str::to_owned),
                transition.to_owned(),
            ));
            const HISTORY_SPILL_CAP: usize = 5000;
            while self.pending_history.len() > HISTORY_SPILL_CAP {
                self.pending_history.pop_front();
                self.history_spill_dropped += 1;
            }
            if self.history_spill_dropped > 0 && self.history_spill_dropped.is_multiple_of(500) {
                warn!(
                    dropped = self.history_spill_dropped,
                    "history spillover overflow: oldest rows dropped"
                );
            }
        }
        let label = title.unwrap_or(&clean).to_owned();
        self.app.autocomplete.insert_with_transition(
            clean.clone(),
            label,
            now_millis(),
            transition,
        );
        // Live-update: open history pages follow writes in real time (push,
        // not poll). Only tabs already on the page pay for a re-query.
        // Open new tabs ride along: their tiles are history-backed, so a
        // visit elsewhere would otherwise leave them stale until reload.
        for id in self.page_tabs("ferrite://history") {
            let q = self.history_queries.get(&id).cloned().unwrap_or_default();
            let items = self.history_json(&q);
            self.push_list(id, "__ferriteHistory", &items);
        }
        for id in self.page_tabs("ferrite://newtab") {
            self.push_newtab_data(id);
        }
        // Live-tab presence feed: this device's current page for the mesh.
        // Only recorded (non-private, vault-backed) visits feed presence;
        // private tabs never leave the window, and failed records feed
        // nothing. Session-scoped and cheap: one validated string handoff.
        if let Some(session) = self.sync_session.as_ref() {
            session.push_local_tab(&clean);
        }
        true
    }

    /// Engine-committed top-level navigation (`nav` IPC from `nav_commit_js`).
    /// Fold into tab stacks first, record iff fresh. Validation here, not in
    /// JS. The page channel is trusted for liveness, not content.
    fn record_engine_navigation(&mut self, tab_id: uuid::Uuid, url: &str) {
        let url = url.trim();
        if url.is_empty() || url.len() > 2048 {
            return;
        }
        // WebView2 commits internal pages in workaround form
        // (`http://ferrite.<page>`); fold back so the tab keeps its
        // canonical `ferrite://` URL and privileged IPC status.
        let canonical = ferrite_tabs::webview::canonical_engine_url(url);
        let lower = canonical.to_lowercase();
        if !(lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("ferrite://"))
        {
            return;
        }
        let fresh = match self.app.tabs.get_tab_mut(tab_id) {
            Some(tab) => tab.commit_engine_navigation(canonical.clone()),
            None => return,
        };
        if fresh {
            // `ferrite://` is filtered inside; no junk history rows.
            self.record_visit_for_tab(tab_id, &canonical, "link", None);
        } else if let Some(tab) = self.app.tabs.get_tab(tab_id) {
            // Suppressed commit = a restored/discarded tab coming alive
            // (the flag consumes exactly once per restore): re-apply the
            // saved scroll offset. New documents reset theirs in
            // `navigate()`, so this never drags stale offsets forward.
            // Numbers are stored finite (reporter + load both validate);
            // the check below is belt-and-braces against in-memory edits.
            let (sx, sy) = tab.scroll_position;
            if (sx, sy) != (0.0, 0.0) && sx.is_finite() && sy.is_finite() {
                if let Some(h) = tab.webview.as_ref() {
                    let js = format!("window.scrollTo({sx},{sy})");
                    if let Err(e) = h.evaluate_script(&js) {
                        debug!(error = %e, "scroll restore failed");
                    }
                }
            }
        }
        self.app.tabs.mark_dirty();
        // Fresh document: prefs may have changed since the installer ran
        // (or the tab predates them). Re-apply spellcheck state per commit.
        self.apply_spellcheck_prefs(tab_id);
        // A committed navigation ends any in-flight print dialog for the
        // tab, releasing the per-tab print guard.
        self.print_jobs.finish(&tab_id);
        // Leaving the viewer drops the tab's PDF sessions (renderer
        // children killed, bytes zeroized). Staying on it keeps them:
        // the commit is the viewer's own load, not a departure.
        if !canonical.starts_with("ferrite://pdf/") {
            self.pdf_sessions.close_tab_sessions(tab_id);
        }
        self.layout_tabs();
    }

    fn commit_omnibox(
        &mut self,
        text: String,
        suggested_url: Option<String>,
        provider: Option<String>,
    ) {
        // A suggested URL is trusted only when it is already a navigable
        // URL (history/bookmark/tab rows). Answer rows carry the raw
        // expression as their URL and remote rows carry a bare phrase;
        // navigating those verbatim loads garbage instead of searching.
        // Anything else falls through to classify, the same function typing
        // uses: commit can never bypass it.
        let url = match suggested_url {
            Some(u) if is_navigable_suggestion(&u) => u,
            _ => {
                // Tab-to-search: a validated provider id fills its template
                // directly. No URL-vs-search disambiguation. Unknown ids
                // fall through to classify, never to a guessed template.
                if let Some(id) = provider.as_deref().filter(|s| !s.trim().is_empty()) {
                    if self.app.omnibox.registry.knows(id) {
                        match self.app.omnibox.registry.search_url_for(id, &text) {
                            Ok(u) => u,
                            Err(e) => {
                                debug!(error = %e, provider = %id, "provider search failed");
                                return;
                            }
                        }
                    } else {
                        debug!(provider = %id, "unknown tab-search provider ignored");
                        match self.app.omnibox.url_for_input(&text) {
                            Ok(u) => u,
                            Err(e) => {
                                debug!(error = %e, "omnibox classify failed");
                                return;
                            }
                        }
                    }
                } else {
                    // Private windows resolve searches through the private
                    // default when one is set. Navigation is identical.
                    let private = self.app.tabs.active_tab().is_some_and(|t| t.is_private);
                    match self.app.omnibox.url_for_input_for(&text, private) {
                        Ok(u) => u,
                        Err(e) => {
                            debug!(error = %e, "omnibox classify failed");
                            return;
                        }
                    }
                }
            }
        };
        if let Some(tab) = self.app.tabs.active_tab_mut() {
            if let Err(e) = tab.load_url(url.clone()) {
                debug!(error = %e, "omnibox navigation failed");
            } else {
                // New document: pending script dialogs die with the old one.
                let id = tab.id;
                self.clear_tab_dialogs(id);
            }
        } else {
            self.open_tab(url.clone());
        }
        self.record_visit(&url, "typed", Some(&text));
        self.find_reset();
        self.hide_ctx_menu();
        self.app.omnibox.clear();
        self.layout_tabs();
    }

    // ── Live sync engine (LAN v1) ──

    /// Start the live sync session: bind QUIC, advertise + browse mDNS,
    /// serve inbound streams, and round with discovered trusted peers every
    /// 15s (plus one immediate round so the mesh shows up without waiting).
    /// Runs on its own runtime thread; the shell only shares Arcs and drains
    /// the dirty flag on tick. Idempotent: safe to call once at boot only;
    /// a second call is a no-op. Refused in ephemeral mode (no background
    /// tasks there, like rotation) and without vault+trust (fail-closed).
    /// A platform refusal (bind/mDNS) runs transport-off with a log line;
    /// the trust UI keeps working, rounds simply never happen.
    pub(crate) fn start_sync_engine(&mut self) {
        if self.config.incognito {
            debug!("sync engine off in ephemeral mode");
            return;
        }
        if self.sync_session.is_some() {
            return;
        }
        let (vault, trust) = match (self.vault.clone(), self.sync_trust.as_ref()) {
            (Some(v), Some(t)) => (v, t),
            _ => {
                debug!("sync engine off without vault+trust");
                return;
            }
        };
        let identity = std::sync::Arc::clone(&trust.this.identity);
        let name = trust.this.name.clone();
        let settings = std::sync::Arc::clone(&self.sync_settings);
        let conn = std::sync::Arc::clone(&vault.conn);
        let boot = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "sync runtime build failed: transport off");
                return;
            }
        };
        let session = match boot.block_on(ferrite_sync::SyncSession::start(
            conn, &identity, name, settings,
        )) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "sync session start failed: transport off");
                return;
            }
        };
        info!("sync engine live: LAN rounds every 15s");
        self.sync_session = Some(std::sync::Arc::clone(&session));
        // Off-LAN relay from sealed settings (LAN-only when empty):
        // attach before the serve tasks start so the first round can
        // already reach off-LAN peers.
        let relay_urls = Self::relay_urls_from_settings();
        if !relay_urls.is_empty() {
            session.set_relay(relay_urls.clone(), &identity);
            info!(count = relay_urls.len(), "sync relay attached (off-LAN)");
        }
        let stop = std::sync::Arc::clone(&self.sync_stop);
        let dirty = std::sync::Arc::clone(&self.sync_dirty);
        std::thread::Builder::new()
            .name("ferrite-sync-live".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                {
                    Ok(rt) => rt,
                    Err(_) => return,
                };
                rt.block_on(async move {
                    let stop_serve = std::sync::Arc::clone(&stop);
                    let serve_session = std::sync::Arc::clone(&session);
                    tokio::spawn(async move {
                        serve_session.serve_forever(&stop_serve).await;
                    });
                    // Relay inbound loop (returns immediately when no
                    // relay is configured; LAN-only operation unchanged).
                    let stop_relay = std::sync::Arc::clone(&stop);
                    let relay_session = std::sync::Arc::clone(&session);
                    tokio::spawn(async move {
                        relay_session.serve_relay(&stop_relay).await;
                    });
                    loop {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        let report = session.round().await;
                        if report.actions_received > 0 {
                            dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        for _ in 0..150 {
                            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    }
                });
            })
            .ok();
    }

    /// Refresh every surface incoming sync rows touch: history/newtab tiles,
    /// bookmarks, side panel, and open settings sync sections. Called once
    /// per dirty round, never per action. Omnibox stays local-first by
    /// design (it reflects this device's behavior; synced rows are
    /// searchable on the history page immediately).
    fn refresh_sync_surfaces(&mut self) {
        self.refresh_history_views();
        for id in self.page_tabs("ferrite://newtab") {
            self.push_newtab_data(id);
        }
        self.push_bookmarks_tabs();
        self.push_panel_section();
        for id in self.page_tabs("ferrite://settings") {
            self.push_sync_devices(id);
        }
        for id in self.page_tabs("ferrite://passwords") {
            self.push_passwords(id);
        }
    }

    // ── Chrome commands ──

    /// Push the boot-gate reasons into the decision tab. Called on the
    /// page's data request and after every failed Retry (reasons change).
    /// `recerr` carries a recovery-attempt error for the inline error line.
    fn push_bootdecision(&self, tab_id: Uuid, recerr: Option<&str>) {
        let reasons = self
            .boot_block
            .as_ref()
            .map(|b| b.reasons.clone())
            .unwrap_or_default();
        let payload = serde_json::json!({ "reasons": reasons, "recerr": recerr });
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteBootDecision({json})"))
        {
            debug!(error = %e, "bootdecision push failed");
        }
    }

    /// Push a freshly minted recovery phrase to the settings page hook.
    /// One-shot render; the phrase lives in no shell state afterwards.
    fn push_recovery(&self, tab_id: Uuid, phrase: Option<&str>) {
        let payload = serde_json::json!({ "phrase": phrase });
        let json = serde_json::to_string(&payload)
            .unwrap_or_default()
            .replace("</", "<\\/");
        if let Some(tab) = self.app.tabs.get_tab(tab_id)
            && let Some(h) = tab.webview.as_ref()
            && let Err(e) = h.evaluate_script(&format!("__ferriteRecovery({json})"))
        {
            debug!(error = %e, "recovery push failed");
        }
    }

    /// Resolve a decision-screen recovery attempt: unwrap the sidecar
    /// under the pasted phrase, store the key, relaunch into a verifying
    /// boot. Failures re-push the decision with an inline error (never a
    /// toast that could outlive the page).
    fn resolve_boot_recovery(&mut self, tab_id: Uuid, phrase: &str) {
        let decided = self
            .boot_block
            .as_ref()
            .is_some_and(|b| Some(tab_id) == b.tab);
        if !decided {
            debug!("bootrecovery ignored: gate not blocked for this tab");
            return;
        }
        match crate::recovery::recover_from_sidecar(phrase) {
            Ok(mut key) => {
                let stored = ferrite_vault::keyring::store_master_key(&key).is_ok();
                key.zeroize();
                if stored {
                    info!("boot recovery accepted: relaunching");
                    self.relaunch_requested = true;
                } else {
                    self.push_bootdecision(
                        tab_id,
                        Some("The phrase matched, but the key couldn't be stored."),
                    );
                }
            }
            Err(_) => {
                self.push_bootdecision(
                    tab_id,
                    Some("That phrase didn't match. Check the words and try again."),
                );
            }
        }
    }

    /// Resolve one decision-screen choice. Only the decision tab's posts
    /// arrive here (gated at the drain); the block is re-checked anyway,
    /// and unknown choices are ignored, never defaulted.
    fn resolve_boot_choice(&mut self, tab_id: Uuid, choice: &str) {
        let decided = self
            .boot_block
            .as_ref()
            .is_some_and(|b| Some(tab_id) == b.tab);
        if !decided {
            debug!("bootdecision ignored: gate not blocked for this tab");
            return;
        }
        match choice {
            "retry" => {
                // Relaunch into a fresh boot: clean state, real keyring
                // read, full store verification. In-process store swapping
                // would leave the proxy and factory wired to dead handles;
                // a restart is slower and more correct. The crash marker is
                // removed by the quit path so no false crash offer follows.
                info!("boot gate retry: relaunching");
                self.relaunch_requested = true;
            }
            "guest" => {
                // Explicit ephemeral mode (see enter_ephemeral): the
                // decision tab closes, the gate clears, and boot proceeds
                // with nothing persistent behind it.
                if self.enter_ephemeral() {
                    self.close_decision_tab();
                    self.boot_block = None;
                    self.boot_tabs();
                    self.notify(
                        "Incognito session: nothing will be saved. Close the window to vanish."
                            .to_owned(),
                    );
                    self.push_chrome(true);
                } else {
                    debug!("guest memory vault failed");
                    self.push_bootdecision(tab_id, None);
                }
            }
            "quit" => {
                info!("boot gate quit requested");
                self.quit_requested = true;
            }
            _ => debug!("bootdecision ignored: unknown choice"),
        }
    }

    /// Enter ephemeral mode: memory vault (history, logins, autofill work
    /// for the session), no archive snapshots, no persistence anywhere
    /// (writes check the global below). Downloads land in a temp dir owned
    /// by a guest profile (created here unless the caller already made one
    /// for the engine dir); the dir dies with a clean exit and is swept at
    /// boot after crashes. Shared by the decision screen and `--incognito`
    /// boots. Returns false when even the memory vault fails (caller
    /// re-shows the decision screen).
    fn enter_ephemeral(&mut self) -> bool {
        let Ok(vault) = ferrite_vault::Vault::open_in_memory() else {
            return false;
        };
        self.vault = Some(std::sync::Arc::new(vault));
        self.guest_mode = true;
        set_ephemeral();
        if self.guest_profile.is_none() {
            self.guest_profile = Some(crate::profiles::guest());
        }
        // The download slot follows the profile even when it predates this
        // call (incognito boots create it early for the engine dir).
        if let Some(ref gp) = self.guest_profile {
            let dl_dir = crate::profiles::guest_downloads_dir(gp);
            if std::fs::create_dir_all(&dl_dir).is_err() {
                debug!("guest downloads dir unwritable");
            }
            if let Ok(mut slot) = self.dl_dir_slot.lock() {
                *slot = Some(dl_dir);
            }
        }
        true
    }

    /// Close the decision tab without recording it (it must never appear
    /// in reopen-closed-tabs or the session).
    fn close_decision_tab(&mut self) {
        if let Some(id) = self.boot_block.as_ref().and_then(|b| b.tab)
            && self.app.tabs.get_tab(id).is_some()
        {
            let _ = self.app.tabs.close_tab(id);
            self.layout_tabs();
        }
    }

    /// Rotation worker body: resolve the old key, mint and journal the new
    /// one, rotate every store, promote, delete the journal. Returns the
    /// user-facing report line. Both key copies zeroize before return on
    /// every path; the keyring holds the truth throughout.
    fn run_rotation_worker(
        vault: std::sync::Arc<ferrite_vault::Vault>,
        archive: Option<std::sync::Arc<ferrite_archive::Archive>>,
    ) -> String {
        use ferrite_vault::keyring;
        let mut old = match keyring::load_master_key() {
            Ok(Some(k)) => k,
            _ => return "Key rotation failed: device key unreadable.".to_owned(),
        };
        let mut new = match keyring::generate_master_key() {
            Ok(k) => k,
            Err(_) => {
                old.zeroize();
                return "Key rotation failed: couldn't generate a new key.".to_owned();
            }
        };
        let (session_enc, session_legacy) = crate::session::session_paths();
        let (settings_enc, settings_legacy) = crate::settings_file::settings_paths();
        let out = crate::rotation::run_rotation(
            &vault,
            archive.as_deref(),
            &session_enc,
            &session_legacy,
            &settings_enc,
            &settings_legacy,
            &old,
            &new,
        );
        old.zeroize();
        new.zeroize();
        out
    }

    /// Spawn a key-rotation worker: fresh key, journal, live rekey of
    /// vault and archive, re-seal of session and settings, promote, journal
    /// delete. One toast reports the outcome; failures name the remedy
    /// (the journal preserves crash-resume). Refused in ephemeral mode
    /// and without a persistent vault: there is nothing durable to rotate.
    fn spawn_rotation(&self) {
        let vault = match self.vault.clone() {
            Some(v) if v.is_persistent() && !is_ephemeral() => v,
            _ => {
                let _ = self.notice_tx.send(
                    "Key rotation needs persistent storage (unavailable in Incognito).".to_owned(),
                );
                return;
            }
        };
        let archive = self.archive_store.clone();
        let notices = self.notice_tx.clone();
        std::thread::Builder::new()
            .name("ferrite-rotate".into())
            .spawn(move || {
                let _ = notices.send(Self::run_rotation_worker(vault, archive));
            })
            .ok();
    }

    fn handle_chrome_command(&mut self, cmd: ChromeCommand) {
        // Boot gate: no chrome input is honored while the decision screen
        // owns the window (the decision page drives over content IPC, so
        // this blanket ignore cannot wedge the gate itself).
        if self.boot_block.is_some() {
            debug!("chrome command ignored: boot gate blocked");
            return;
        }
        match cmd {
            ChromeCommand::RequestState => {
                // Fresh consumer (chrome reloaded): favicon cache on both
                // ends is void, so clear the sent map for a full resend.
                self.favicon_sent.clear();
                self.push_chrome(true)
            }
            ChromeCommand::ToastAction { seq } => self.toast_action(seq),
            ChromeCommand::ToastDismiss { seq } => {
                if self.toasts.dismiss(seq, Instant::now()) {
                    self.sync_undo_deadline(Instant::now());
                    self.sync_save_deadline(Instant::now());
                    self.push_chrome(true);
                }
            }
            ChromeCommand::Navigate { url } => {
                let tab_id = self.app.tabs.active_id();
                debug!(url = %url, ?tab_id, "chrome navigate");
                // Load first: an explicit command clears restore-suppression
                // (via navigate), so the record below is never swallowed.
                // New document: find highlights die with the old one.
                self.find_reset();
                self.hide_ctx_menu();
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    if let Err(e) = tab.load_url(url.clone()) {
                        debug!(error = %e, "chrome navigation failed");
                    } else {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                    }
                } else {
                    self.open_tab(url.clone());
                }
                self.record_visit(&url, "link", None);
                self.app.omnibox.clear();
                self.layout_tabs();
            }
            ChromeCommand::NewTab { url } => {
                self.open_tab(url.unwrap_or_else(|| "ferrite://newtab".to_owned()));
            }
            ChromeCommand::CloseTab { id } => {
                if let Ok(id) = id.parse::<Uuid>() {
                    self.close_tab_recorded(id);
                } else {
                    self.layout_tabs();
                }
            }
            ChromeCommand::SelectTab { id } => {
                let idx = id
                    .parse::<Uuid>()
                    .ok()
                    .and_then(|id| self.app.tabs.iter().position(|t| t.id == id));
                if let Some(idx) = idx {
                    self.switch_to_index(idx);
                    // The bar now shows the new tab: drop whatever was typed
                    // so the next push cannot resurrect stale input over it.
                    self.app.omnibox.clear();
                    self.push_chrome(true);
                }
            }
            ChromeCommand::ToggleTabSearch => self.toggle_web_tabsearch(),
            ChromeCommand::FocusContent => {
                if let Some(tab) = self.app.tabs.active_tab()
                    && let Some(h) = tab.webview.as_ref()
                    && let Err(e) = h.focus()
                {
                    debug!(error = %e, "chrome focus-content failed");
                }
            }
            ChromeCommand::DismissOverlay => {
                self.overlay_open = false;
                Self::close_web_tabsearch(&mut self.overlay, &mut self.app.tabs);
            }
            ChromeCommand::OmniboxInput { text } => {
                self.app.omnibox.set_input(text);
                // Remote suggestions (opt-in only): one thread per
                // debounced keystroke, 2s self-timeout. The generation
                // discards anything older than the latest input. Private
                // tabs never fire: a global opt-in must not exfiltrate
                // private keystrokes, so they stay local-only here.
                let q = self.app.omnibox.input.trim().to_owned();
                let private = self.app.tabs.active_tab().is_some_and(|t| t.is_private);
                // Completions come from DuckDuckGo only (see `suggest`):
                // fetching while another engine is effective would send
                // keystrokes somewhere the toolbar does not name. Scoped
                // input (`@tabs`…) never fetches either: the scope promises
                // one local source, and remote rows would break that promise
                // while leaking the scoped query.
                let ddg = self.app.omnibox.registry.effective_id(private) == "duckduckgo";
                let scoped = omni_scope(&q).0.is_some();
                if !scoped
                    && remote_fetch_allowed(self.remote_enabled, private, q.chars().count(), ddg)
                {
                    self.sug_gen += 1;
                    let generation = self.sug_gen;
                    let tx = self.sug_tx.clone();
                    std::thread::spawn(move || {
                        let phrases = crate::suggest::fetch_suggestions(&q);
                        let _ = tx.send((generation, q, phrases));
                    });
                }
            }
            ChromeCommand::OmniboxSubmit {
                text,
                url,
                provider,
            } => self.commit_omnibox(text, url, provider),
            ChromeCommand::SugDelete { url } => {
                let snap = self.vault.as_ref().and_then(|v| v.history_row_by_url(&url));
                if let Some(vault) = self.vault.as_ref()
                    && let Err(e) = vault.delete_history_url(&url)
                {
                    debug!(error = %e, "suggestion delete failed");
                }
                self.app.autocomplete.remove(&url);
                // Immediate re-push: the row vanishes without leaving
                // the omnibox (the ≤500ms tick would cover it anyway).
                if let Some(row) = snap {
                    // DB fallback keeps suggesting the URL until the next
                    // visit, so autocomplete needs no re-add on undo.
                    self.push_undo(
                        "History entry deleted".to_owned(),
                        crate::toast::UndoPayload::History(vec![row]),
                    );
                } else {
                    self.push_chrome(true);
                }
            }
            ChromeCommand::PermDecide {
                origin,
                kind,
                always,
            } => {
                // Session scope: a private tab's "always" must never become a
                // persistent Vault row. Ephemeral processes are already
                // memory-only (persist is a no-op there), but private tabs
                // inside user mode need the ephemeral grant path explicitly.
                let private_ctx = self.session.is_ephemeral()
                    || self.app.tabs.active_tab().is_some_and(|t| t.is_private);
                if always {
                    if private_ctx {
                        self.permissions.grant_always_ephemeral(
                            &origin,
                            &kind,
                            ferrite_tabs::now_millis(),
                        );
                    } else {
                        self.permissions
                            .grant_always(&origin, &kind, ferrite_tabs::now_millis());
                        self.persist_perm_snapshot();
                    }
                } else {
                    self.permissions.grant_once(&origin, &kind);
                    // Once-grants aren't stored, but the grant may have
                    // cleared an auto-block. Persist that change too (no-op
                    // for ephemeral sessions / private-only automation state
                    // is still exported without the ephemeral rows).
                    self.persist_perm_snapshot();
                }
                // The denied request already died: the popover says the
                // site must ask again. Re-push clears the decided rows.
                self.push_chrome(true);
            }
            ChromeCommand::ShieldPause => {
                // Toggle blocking pause for the ACTIVE tab's page. The host
                // is derived server-side from the live tab; the command
                // carries no host, so a compromised page cannot pause
                // protection for an arbitrary origin. Internal pages have
                // nothing to pause. Ephemeral windows apply memory-only
                // (settings writes are refused there) and say so.
                let host = self
                    .app
                    .tabs
                    .active_tab()
                    .map(|t| t.url.clone())
                    .and_then(|u| page_host_for_shield(&u));
                match host {
                    None => debug!("shield pause ignored: no pausable page"),
                    Some(host) => {
                        let was_paused = self.privacy.is_paused(&host);
                        if self.privacy.set_paused(&host, !was_paused).is_some() {
                            if is_ephemeral() {
                                self.notify("Blocking paused for this session only.".to_owned());
                            } else {
                                let stored = self.privacy.settings().paused_hosts;
                                persist_setting(
                                    "paused_hosts",
                                    serde_json::Value::Array(
                                        stored.into_iter().map(serde_json::Value::String).collect(),
                                    ),
                                );
                                self.notify(if was_paused {
                                    format!("Blocking resumed for {host}.")
                                } else {
                                    format!("Blocking paused for {host}.")
                                });
                            }
                            self.push_chrome(true);
                        }
                    }
                }
            }
            ChromeCommand::SelectEngine { id } => {
                if let Err(e) = self.app.omnibox.registry.set_current(&id) {
                    debug!(error = %e, engine = %id, "unknown engine ignored");
                } else {
                    persist_omnibox_engine(&id);
                    // Re-run any pending input against the new engine.
                    let pending = self.app.omnibox.input.clone();
                    if !pending.trim().is_empty() {
                        self.commit_omnibox(pending, None, None);
                    }
                }
            }
            ChromeCommand::MenuItem { id } => self.menu_action(&id),
            ChromeCommand::Reload => self.reload_or_stop_active(),
            ChromeCommand::GoBack { steps } => {
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    let mut moved = false;
                    for _ in 0..steps.clamp(1, 20) {
                        let Some(url) = tab.go_back() else { break };
                        if let Err(e) = tab.load_traversal(url) {
                            debug!(error = %e, "chrome back failed");
                            break;
                        }
                        moved = true;
                    }
                    if moved {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                        self.find_reset();
                    }
                }
            }
            ChromeCommand::GoForward { steps } => {
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    let mut moved = false;
                    for _ in 0..steps.clamp(1, 20) {
                        let Some(url) = tab.go_forward() else { break };
                        if let Err(e) = tab.load_traversal(url) {
                            debug!(error = %e, "chrome forward failed");
                            break;
                        }
                        moved = true;
                    }
                    if moved {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                        self.find_reset();
                    }
                }
            }
            ChromeCommand::ToggleMute { id } => {
                // Strip icons name their tab; the toolbar button, palette,
                // and old posts omit it and mean the active tab. A stale id
                // (closed tab) falls back to active rather than no-op.
                let idx = id
                    .as_deref()
                    .and_then(|s| s.parse::<Uuid>().ok())
                    .and_then(|tid| self.app.tabs.iter().position(|t| t.id == tid))
                    .or_else(|| self.app.tabs.active_index());
                if let Some(idx) = idx
                    && let Err(e) = self.app.tabs.toggle_mute(idx)
                {
                    debug!(error = %e, "chrome mute failed");
                }
            }
            ChromeCommand::TogglePin { id } => {
                let idx = id
                    .as_deref()
                    .and_then(|s| s.parse::<Uuid>().ok())
                    .and_then(|tid| self.app.tabs.iter().position(|t| t.id == tid))
                    .or_else(|| self.app.tabs.active_index());
                if let Some(idx) = idx
                    && let Err(e) = self.app.tabs.toggle_pin(idx)
                {
                    debug!(error = %e, "chrome pin failed");
                }
            }
            ChromeCommand::Reorder { from, to } => {
                if let Err(e) = self.app.tabs.reorder(from, to) {
                    debug!(error = %e, from, to, "chrome reorder ignored");
                }
            }
            ChromeCommand::ToggleStar { url, title } => {
                self.toggle_star(url, title);
                self.push_panel_section();
            }
            ChromeCommand::BookmarkAdd { url, title, parent } => {
                if self.vault.is_none() {
                    self.notify("Bookmarks need the vault available.".to_owned());
                } else if self.save_bookmark(url, title, parent) {
                    self.maybe_autoreveal_bar();
                }
                self.push_chrome(true);
                self.push_panel_section();
                self.push_bookmarks_tabs();
            }
            ChromeCommand::FolderAdd { name } => {
                match self.vault.as_ref() {
                    Some(vault) => match vault.add_folder(&name, None) {
                        Ok(_) => info!("bookmark folder created"),
                        Err(e) => {
                            debug!(error = %e, "folder create failed");
                            self.notify("Couldn't create that folder.".to_owned());
                        }
                    },
                    None => self.notify("Bookmarks need the vault available.".to_owned()),
                }
                self.push_chrome(true);
                self.push_panel_section();
                self.push_bookmarks_tabs();
            }
            ChromeCommand::BookmarkAllTabs => {
                self.bookmark_all_tabs();
                self.push_chrome(true);
                self.push_panel_section();
                self.push_bookmarks_tabs();
            }
            ChromeCommand::FolderList { id } => {
                let items: Vec<serde_json::Value> = match self.vault.as_ref() {
                    Some(v) => v
                        .list_bookmark_children(Some(&id))
                        .iter()
                        .map(|n| {
                            serde_json::json!({
                                "id": n.id, "title": n.title, "url": n.url,
                                "folder": n.is_folder,
                            })
                        })
                        .collect(),
                    None => Vec::new(),
                };
                let payload = serde_json::json!({"id": id, "items": items});
                let json = serde_json::to_string(&payload)
                    .unwrap_or_default()
                    .replace("</", "<\\/");
                if let Some(chrome) = self.chrome.as_ref()
                    && let Err(e) = chrome.evaluate_script(&format!("__ferriteFolderList({json})"))
                {
                    debug!(error = %e, "folder list push failed");
                }
            }
            ChromeCommand::BarReorder { ids } => {
                if let Some(vault) = self.vault.as_ref() {
                    // Trust-but-verify: only reorder ids that are bar roots.
                    let known: std::collections::HashSet<String> = vault
                        .list_bookmark_children(None)
                        .into_iter()
                        .map(|n| n.id)
                        .collect();
                    let ordered: Vec<String> =
                        ids.into_iter().filter(|id| known.contains(id)).collect();
                    if let Err(e) = vault.reorder_bookmarks(None, &ordered) {
                        debug!(error = %e, "bar reorder failed");
                    }
                }
                self.push_chrome(true);
                self.push_panel_section();
                self.push_bookmarks_tabs();
            }
            ChromeCommand::HistoryRecent => {
                let items = self.history_json("");
                let items: Vec<serde_json::Value> = items.into_iter().take(8).collect();
                let closed: Vec<serde_json::Value> = self
                    .closed_display()
                    .iter()
                    .enumerate()
                    .map(|(i, (url, title))| {
                        serde_json::json!({"index": i, "url": url, "title": title})
                    })
                    .collect();
                self.push_list_to(self.chrome.as_ref(), "__ferriteHistoryRecent", &items);
                self.push_list_to(self.chrome.as_ref(), "__ferriteClosedRecent", &closed);
            }
            ChromeCommand::ClosedReopen { index } => {
                if let Some(entry) = self.closed_at(index).cloned() {
                    if self.open_restored(&entry).is_some() {
                        self.take_closed_at(index);
                    }
                }
            }
            ChromeCommand::ClosedRestoreAll => {
                let mut failed = Vec::new();
                for entry in self.drain_closed_all() {
                    if self.open_restored(&entry).is_none() {
                        failed.push(entry);
                    }
                }
                self.closed_tabs = failed;
            }
            ChromeCommand::DownloadsRecent => {
                let items: Vec<serde_json::Value> =
                    self.downloads_json("", "").into_iter().take(8).collect();
                self.push_list_to(self.chrome.as_ref(), "__ferriteDownloadsRecent", &items);
            }
            ChromeCommand::DlBubbleOpen { id } => self.bubble_dl_open(&id, false),
            ChromeCommand::DlBubbleFolder { id } => self.bubble_dl_open(&id, true),
            ChromeCommand::DlBubbleRetry { id } => match self.bubble_dl_target(&id) {
                Some((_, url, _)) if !url.is_empty() => {
                    self.open_tab(url);
                }
                _ => self.notify("That file is gone.".to_owned()),
            },
            ChromeCommand::DlBubbleRemove { id } => {
                if let Some(vault) = self.vault.as_ref()
                    && let Err(e) = vault.remove_download(&id)
                {
                    debug!(error = %e, "bubble download remove failed");
                }
                let items: Vec<serde_json::Value> =
                    self.downloads_json("", "").into_iter().take(8).collect();
                self.push_list_to(self.chrome.as_ref(), "__ferriteDownloadsRecent", &items);
            }
            ChromeCommand::HistList { dir } => {
                let hook = match dir.as_str() {
                    "back" => "__ferriteBackList",
                    "fwd" => "__ferriteFwdList",
                    _ => return,
                };
                let items: Vec<serde_json::Value> = match self.app.tabs.active_tab() {
                    Some(t) => {
                        // 20 rows to match the engine cap and the GoBack
                        // steps clamp: steps 11-20 used to be unreachable
                        // because the UI only ever served 10.
                        let list = if dir == "back" {
                            t.history.back_list(20)
                        } else {
                            t.history.forward_list(20)
                        };
                        list.iter().map(|u| serde_json::json!({"url": u})).collect()
                    }
                    None => Vec::new(),
                };
                self.push_list_to(self.chrome.as_ref(), hook, &items);
            }
            ChromeCommand::PanelToggle => self.set_panel_open(!self.panel_open),
            ChromeCommand::PanelSection { id } => self.set_panel_section(&id),
            ChromeCommand::PanelResize { w } => self.set_panel_width(w),
            ChromeCommand::PanelHistory { q } => {
                let items: Vec<serde_json::Value> =
                    self.history_json(&q).into_iter().take(30).collect();
                self.push_list_to(self.chrome.as_ref(), "__ferritePanelHistory", &items);
            }
            ChromeCommand::ReadingAdd { url, title } => {
                match self.vault.as_ref() {
                    Some(v) => match v.reading_add(&url, &title) {
                        Ok(_) => info!("saved to reading list"),
                        Err(e) => {
                            debug!(error = %e, "reading-list save failed");
                            self.notify("Couldn't save to the reading list.".to_owned());
                        }
                    },
                    None => self.notify("Reading list needs the vault available.".to_owned()),
                }
                self.push_panel_section();
            }
            ChromeCommand::ReadingRead { id } => {
                // Toggle: read rows recede to the bottom, unread return up.
                let flip: Option<bool> = self.vault.as_ref().and_then(|v| {
                    v.reading_list(200)
                        .into_iter()
                        .find(|r| r.id == id)
                        .map(|r| !r.is_read)
                });
                if let (Some(v), Some(read)) = (self.vault.as_ref(), flip) {
                    if let Err(e) = v.reading_mark(&id, read) {
                        debug!(error = %e, "reading-list mark failed");
                    }
                }
                self.push_panel_section();
            }
            ChromeCommand::ReadingRemove { id } => {
                let snap = self
                    .vault
                    .as_ref()
                    .and_then(|v| v.reading_list(200).into_iter().find(|r| r.id == id));
                if let Some(v) = self.vault.as_ref()
                    && let Err(e) = v.reading_remove(&id)
                {
                    debug!(error = %e, "reading-list remove failed");
                }
                self.push_panel_section();
                if let Some(row) = snap {
                    self.push_undo(
                        "Removed from reading list".to_owned(),
                        crate::toast::UndoPayload::Reading(vec![row]),
                    );
                }
            }
            ChromeCommand::ReadingList => self.push_reading_list(),
            ChromeCommand::FindOpen => self.open_find_bar(),
            ChromeCommand::FindSearch { query, case } => {
                if !self.find_open {
                    self.find_open = true;
                    self.find_tab = self.app.tabs.active_id();
                }
                self.find_query = query.clone();
                self.find_case = case;
                if !query.trim().is_empty() {
                    self.last_find = query.clone();
                }
                if !self.find_eval("search", &query, case) {
                    self.find_total = 0;
                    self.find_index = 0;
                    self.push_chrome(true);
                }
            }
            ChromeCommand::FindNext => {
                if self.find_open {
                    let (q, case) = (self.find_query.clone(), self.find_case);
                    if !self.find_eval("next", &q, case) {
                        self.find_total = 0;
                        self.find_index = 0;
                        self.push_chrome(true);
                    }
                }
            }
            ChromeCommand::FindPrev => {
                if self.find_open {
                    let (q, case) = (self.find_query.clone(), self.find_case);
                    if !self.find_eval("prev", &q, case) {
                        self.find_total = 0;
                        self.find_index = 0;
                        self.push_chrome(true);
                    }
                }
            }
            ChromeCommand::FindClose => {
                let (q, case) = (self.find_query.clone(), self.find_case);
                self.find_eval("clear", &q, case);
                self.find_reset();
                self.push_chrome(true);
            }
            ChromeCommand::OpenDevTools => self.open_devtools(),
            ChromeCommand::EnterPip => self.pip_enter_active(false),
            ChromeCommand::ExitPip => self.pip_exit_active(),
            ChromeCommand::Share => self.share_active(),
            ChromeCommand::ReaderOpen => {
                // Extract from the active tab (explicit gesture only,
                // never on load), then open the reader on completion. A
                // second click while one is in flight is ignored (not
                // restarted): re-extracting would orphan the first
                // assembly and its chunks.
                let src = self
                    .app
                    .tabs
                    .active_tab()
                    .map(|t| t.url.clone())
                    .unwrap_or_default();
                if src.starts_with("http://") || src.starts_with("https://") {
                    if let Some(tab) = self.app.tabs.active_tab()
                        && let Some(h) = tab.webview.as_ref()
                        && !self.reader_pending.contains_key(&tab.id)
                    {
                        let js = ferrite_tabs::protocol::reader_extract_js();
                        if let Err(e) = h.evaluate_script(&js) {
                            debug!(error = %e, "reader extract failed");
                        } else {
                            self.reader_pending.insert(
                                tab.id,
                                ReaderAssembly {
                                    src,
                                    ..Default::default()
                                },
                            );
                        }
                    }
                }
            }
            ChromeCommand::CrashRestore => {
                // A find highlight on the fresh tab dies with the restore.
                let (q, case) = (self.find_query.clone(), self.find_case);
                self.find_eval("clear", &q, case);
                self.find_reset();
                self.crash_offer = false;
                if self.restore_session_tabs() > 0 {
                    self.activate_restored_tab();
                }
                self.layout_tabs();
                self.push_chrome(true);
            }
            ChromeCommand::CrashRestoreExceptActive => {
                // Same as restore, minus the saved-active tab (the likely
                // culprit: it was foreground when the process died). The
                // dropped tab lands on the closed stack, so Ctrl+Shift+T
                // reopens it if it was innocent. Indices align: restore
                // preserves file order, and the peek reads the same doc.
                let (q, case) = (self.find_query.clone(), self.find_case);
                self.find_eval("clear", &q, case);
                self.find_reset();
                self.crash_offer = false;
                if self.restore_session_tabs() > 0 {
                    let (_, _, active) = self.peek_session_docs();
                    // Resolve first (borrow ends), then mutate: the
                    // iterator cannot live across the close.
                    let suspect = self.app.tabs.iter().nth(active).map(|t| t.id);
                    if let Some(id) = suspect {
                        let (closed, _) = self.close_tab_ids(&[id]);
                        if closed > 0 {
                            self.notify(
                                "Restored without the last active tab (undo reopens it)."
                                    .to_owned(),
                            );
                        }
                    }
                    self.activate_restored_tab();
                }
                self.layout_tabs();
                self.push_chrome(true);
            }
            ChromeCommand::CtxReload { id } => {
                if let Ok(id) = id.parse::<Uuid>()
                    && let Some(tab) = self.app.tabs.get_tab_mut(id)
                {
                    if let Err(e) = tab.reload() {
                        debug!(error = %e, "ctx reload failed");
                    } else {
                        self.clear_tab_dialogs(id);
                        if self.find_tab == Some(id) {
                            self.find_reset();
                        }
                    }
                }
            }
            ChromeCommand::CtxDuplicate { id } => {
                // Full-fidelity duplicate (history, mute, pin, private flag,
                // scroll), not a fresh tab on the same URL. Refusals toast:
                // a silent menu item reads as broken.
                match id.parse::<Uuid>() {
                    Ok(source) => {
                        let window = self.window.clone();
                        let bounds = self.content_wry();
                        if let Some(window) = window.as_ref() {
                            match self.app.tabs.duplicate_tab(Some(window), source, bounds) {
                                Ok(_) => self.layout_tabs(),
                                Err(e) => {
                                    debug!(error = %e, "tab duplicate refused");
                                    self.notify("Couldn't duplicate that tab.".to_owned());
                                }
                            }
                        }
                    }
                    Err(_) => debug!("tab duplicate ignored: bad id"),
                }
            }
            ChromeCommand::CtxPin { id } => {
                let idx = id
                    .parse::<Uuid>()
                    .ok()
                    .and_then(|id| self.app.tabs.iter().position(|t| t.id == id));
                if let Some(idx) = idx
                    && let Err(e) = self.app.tabs.toggle_pin(idx)
                {
                    debug!(error = %e, "ctx pin failed");
                }
            }
            ChromeCommand::CtxMute { id } => {
                let idx = id
                    .parse::<Uuid>()
                    .ok()
                    .and_then(|id| self.app.tabs.iter().position(|t| t.id == id));
                if let Some(idx) = idx
                    && let Err(e) = self.app.tabs.toggle_mute(idx)
                {
                    debug!(error = %e, "ctx mute failed");
                }
            }
            ChromeCommand::CtxSleep { id } => match id.parse::<Uuid>() {
                Ok(tid) => {
                    // Literals only (`'static`): no borrow crosses into
                    // the notify below.
                    let verdict: Option<&'static str> = self.app.tabs.get_tab(tid).map(|t| {
                        if Some(tid) == self.app.tabs.active_id() {
                            "The active tab can't sleep."
                        } else if t.is_pinned {
                            "Pinned tabs stay awake."
                        } else if t.is_playing_audio {
                            "Stop the audio first. Sleep would kill playback."
                        } else if t.has_active_download {
                            "That tab is downloading. Wait or cancel first."
                        } else if t.is_discarded() {
                            "Already asleep."
                        } else {
                            ""
                        }
                    });
                    match verdict {
                        Some("") | None if self.sleep_tab_id(tid) => {}
                        Some("") | None => self.notify("Couldn't sleep that tab.".to_owned()),
                        Some(msg) => self.notify(msg.to_owned()),
                    }
                }
                Err(_) => debug!("ctx sleep ignored: bad id"),
            },
            ChromeCommand::SleepActive => self.sleep_active_tab(),
            ChromeCommand::TranslatePage => {
                if self.translate_enabled
                    && let Some(id) = self.app.tabs.active_id()
                {
                    self.run_translate(id, ferrite_tabs::translate::TranslateMode::Page);
                }
            }
            ChromeCommand::CtxCopyUrl { id } => {
                if let Ok(id) = id.parse::<Uuid>() {
                    let url = self
                        .app
                        .tabs
                        .get_tab(id)
                        .map(|t| t.url.clone())
                        .unwrap_or_default();
                    self.copy_with_toast(&url, "Tab URL");
                }
            }
            ChromeCommand::CtxCloseOthers { id } => {
                // Pinned tabs survive (matches the bar/dismiss conservatism:
                // never destroy pinned state from a bulk action).
                if let Ok(keep) = id.parse::<Uuid>() {
                    let ids: Vec<Uuid> = self
                        .app
                        .tabs
                        .iter()
                        .filter(|t| t.id != keep && !t.is_pinned)
                        .map(|t| t.id)
                        .collect();
                    for cid in ids {
                        if let Some(t) = self.app.tabs.get_tab(cid).map(|t| {
                            (
                                t.url.clone(),
                                t.title.clone(),
                                t.history.clone(),
                                t.scroll_position,
                                t.is_pinned,
                                t.is_private,
                            )
                        }) {
                            let (url, title, history, scroll, pinned, private) = t;
                            self.remember_closed(url, title, history, scroll, pinned, private);
                            let _ = self.app.tabs.close_tab(cid);
                            self.clear_tab_dialogs(cid);
                        }
                        if self.find_tab == Some(cid) {
                            self.find_reset();
                        }
                    }
                    self.layout_tabs();
                    self.push_chrome(true);
                }
            }
            ChromeCommand::CtxCloseRight { id } => {
                // No pinned filter: pinned tabs are always leftmost, so
                // everything right of any tab is fair game by construction.
                if let Ok(id) = id.parse::<Uuid>() {
                    let pos = self.app.tabs.iter().position(|t| t.id == id);
                    if let Some(pos) = pos {
                        let ids: Vec<Uuid> =
                            self.app.tabs.iter().skip(pos + 1).map(|t| t.id).collect();
                        for cid in ids {
                            if let Some(t) = self.app.tabs.get_tab(cid).map(|t| {
                                (
                                    t.url.clone(),
                                    t.title.clone(),
                                    t.history.clone(),
                                    t.scroll_position,
                                    t.is_pinned,
                                    t.is_private,
                                )
                            }) {
                                let (url, title, history, scroll, pinned, private) = t;
                                self.remember_closed(url, title, history, scroll, pinned, private);
                                let _ = self.app.tabs.close_tab(cid);
                                self.clear_tab_dialogs(cid);
                            }
                            if self.find_tab == Some(cid) {
                                self.find_reset();
                            }
                        }
                        self.layout_tabs();
                        self.push_chrome(true);
                    }
                }
            }
            ChromeCommand::CtxBookmarkDelete { id } => {
                if let Some(vault) = self.vault.as_ref()
                    && let Err(e) = vault.delete_bookmark(&id)
                {
                    debug!(error = %e, "ctx bookmark delete failed");
                }
                self.push_chrome(true);
                self.push_panel_section();
                self.push_bookmarks_tabs();
                self.push_undo(
                    "Bookmark deleted".to_owned(),
                    crate::toast::UndoPayload::Bookmarks(vec![id]),
                );
            }
            ChromeCommand::GroupCreate {
                name,
                tabs,
                color,
                collapsed,
            } => {
                let ids: Vec<Uuid> = tabs.iter().filter_map(|s| s.parse().ok()).collect();
                match self.app.tabs.group_tabs(&ids, &name, color, collapsed) {
                    Ok((_, n)) => {
                        self.push_chrome(true);
                        self.notify(format!("Grouped {n} tabs."));
                    }
                    Err(e) => {
                        debug!(error = %e, "group create refused");
                        self.notify("Couldn't create that group.".to_owned());
                    }
                }
            }
            ChromeCommand::GroupRename { id, name } => match id.parse::<Uuid>() {
                Ok(gid) => match self.app.tabs.rename_group(gid, &name) {
                    Ok(()) => self.push_chrome(true),
                    Err(e) => {
                        debug!(error = %e, "group rename refused");
                        self.notify("Couldn't rename that group.".to_owned());
                    }
                },
                Err(_) => debug!("group rename ignored: bad id"),
            },
            ChromeCommand::GroupColor { id, color } => match id.parse::<Uuid>() {
                Ok(gid) => {
                    if let Err(e) = self.app.tabs.set_group_color(gid, color) {
                        debug!(error = %e, "group recolor refused");
                    } else {
                        self.push_chrome(true);
                    }
                }
                Err(_) => debug!("group recolor ignored: bad id"),
            },
            ChromeCommand::GroupCollapse { id, collapsed } => match id.parse::<Uuid>() {
                Ok(gid) => {
                    if let Err(e) = self.app.tabs.set_group_collapsed(gid, collapsed) {
                        debug!(error = %e, "group collapse refused");
                    } else {
                        self.push_chrome(true);
                    }
                }
                Err(_) => debug!("group collapse ignored: bad id"),
            },
            ChromeCommand::GroupDelete { id } => match id.parse::<Uuid>() {
                Ok(gid) => {
                    if let Err(e) = self.app.tabs.delete_group(gid) {
                        debug!(error = %e, "group delete refused");
                    } else {
                        self.push_chrome(true);
                    }
                }
                Err(_) => debug!("group delete ignored: bad id"),
            },
            ChromeCommand::GroupClose { id } => match id.parse::<Uuid>() {
                Ok(gid) => {
                    let members = self.app.tabs.tabs_in_group(gid);
                    let (closed, kept) = self.close_tab_ids(&members);
                    if kept > 0 {
                        self.notify(format!("Closed {closed} tabs, kept {kept}."));
                    }
                }
                Err(_) => debug!("group close ignored: bad id"),
            },
            ChromeCommand::GroupMove { id, to } => match id.parse::<Uuid>() {
                Ok(gid) => {
                    if let Err(e) = self.app.tabs.move_group(gid, to) {
                        debug!(error = %e, "group move refused");
                    } else {
                        self.layout_tabs();
                        self.push_chrome(true);
                    }
                }
                Err(_) => debug!("group move ignored: bad id"),
            },
            ChromeCommand::GroupAddTab { id, tab } => {
                match (id.parse::<Uuid>(), tab.parse::<Uuid>()) {
                    (Ok(gid), Ok(tid)) => {
                        if let Err(e) = self.app.tabs.set_tab_group(tid, Some(gid)) {
                            debug!(error = %e, "group adopt refused");
                            self.notify("Couldn't add that tab to the group.".to_owned());
                        } else {
                            self.push_chrome(true);
                        }
                    }
                    _ => debug!("group adopt ignored: bad id"),
                }
            }
            ChromeCommand::GroupRemoveTab { tab } => match tab.parse::<Uuid>() {
                Ok(tid) => {
                    if let Err(e) = self.app.tabs.set_tab_group(tid, None) {
                        debug!(error = %e, "group remove refused");
                    } else {
                        self.push_chrome(true);
                    }
                }
                Err(_) => debug!("group remove ignored: bad id"),
            },
            ChromeCommand::TabsClose { ids } => {
                let parsed: Vec<Uuid> = ids.iter().filter_map(|s| s.parse().ok()).collect();
                let (closed, kept) = self.close_tab_ids(&parsed);
                if kept > 0 {
                    self.notify(format!("Closed {closed} tabs, kept {kept}."));
                }
            }
            ChromeCommand::TabsMove { ids, to } => {
                let parsed: Vec<Uuid> = ids.iter().filter_map(|s| s.parse().ok()).collect();
                let (_, kept) = self.app.tabs.move_tabs(&parsed, to);
                self.layout_tabs();
                self.push_chrome(true);
                if kept > 0 {
                    self.notify(format!("Kept {kept} pinned tabs in place."));
                }
            }
            ChromeCommand::WsCreate { name } => match self.app.tabs.create_workspace(&name) {
                Ok(_) => self.settle_workspace_switch(),
                Err(e) => {
                    debug!(error = %e, "workspace create refused");
                    self.notify("Couldn't create that workspace.".to_owned());
                }
            },
            ChromeCommand::WsRename { id, name } => match id.parse::<Uuid>() {
                Ok(wid) => match self.app.tabs.rename_workspace(wid, &name) {
                    Ok(()) => self.push_chrome(true),
                    Err(e) => {
                        debug!(error = %e, "workspace rename refused");
                        self.notify("Couldn't rename that workspace.".to_owned());
                    }
                },
                Err(_) => debug!("workspace rename ignored: bad id"),
            },
            ChromeCommand::WsDelete { id } => match id.parse::<Uuid>() {
                Ok(wid) => match self.app.tabs.delete_workspace(wid) {
                    Ok(dropped) => {
                        let n = dropped.len();
                        for (url, title) in dropped {
                            self.remember_closed_bare(url, title);
                        }
                        self.settle_workspace_switch();
                        self.notify(format!("Workspace deleted ({n} tabs closed)."));
                    }
                    Err(e) => {
                        debug!(error = %e, "workspace delete refused");
                        self.notify("Couldn't delete that workspace.".to_owned());
                    }
                },
                Err(_) => debug!("workspace delete ignored: bad id"),
            },
            ChromeCommand::WsSwitch { id } => match id.parse::<Uuid>() {
                Ok(wid) => {
                    if let Err(e) = self.app.tabs.switch_workspace(wid) {
                        debug!(error = %e, "workspace switch refused");
                    } else {
                        self.settle_workspace_switch();
                    }
                }
                Err(_) => debug!("workspace switch ignored: bad id"),
            },
            ChromeCommand::WsDefault => {
                if let Err(e) = self.app.tabs.switch_default() {
                    debug!(error = %e, "workspace switch refused");
                } else {
                    self.settle_workspace_switch();
                }
            }
            ChromeCommand::ProfileSwitch { id } => {
                // Spawn only for profiles that exist (Default always):
                // the id scopes a process's stores, so arbitrary ids
                // must never reach the command line unvalidated.
                let known = id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID)
                    || crate::profiles::list().iter().any(|p| p == &id);
                if known {
                    self.spawn_profile(&id);
                } else {
                    debug!(profile = %id, "profile switch ignored: unknown id");
                }
            }
            ChromeCommand::ProfileCreate { name } => {
                match crate::profiles::sanitize(&name).and_then(|id| crate::profiles::create(&id)) {
                    Some(p) => self.spawn_profile(&p.id),
                    None => self.notify(
                        "Profile names use letters, numbers, spaces, _ or -, up to 32 characters."
                            .to_owned(),
                    ),
                }
            }
            ChromeCommand::ProfileDelete { id } => {
                if id.eq_ignore_ascii_case(&self.profile_id)
                    || id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID)
                {
                    self.notify("The current and Default profiles can't be deleted.".to_owned());
                } else if crate::profiles::list().iter().any(|p| p == &id) {
                    crate::profiles::destroy(&crate::profiles::Profile {
                        id: id.clone(),
                        ephemeral: false,
                    });
                    self.push_chrome(true);
                    self.notify(format!("Profile {id} deleted."));
                } else {
                    debug!(profile = %id, "profile delete ignored: unknown id");
                }
            }
            ChromeCommand::TabsMute { ids, muted } => {
                let mut n = 0;
                for s in &ids {
                    let Ok(tid) = s.parse::<Uuid>() else {
                        continue;
                    };
                    // Resolve and compare under one immutable borrow, then
                    // release it before the toggle (borrowck discipline).
                    let job = self
                        .app
                        .tabs
                        .iter()
                        .position(|t| t.id == tid)
                        .and_then(|idx| {
                            self.app
                                .tabs
                                .iter()
                                .nth(idx)
                                .filter(|t| t.is_muted != muted)
                                .map(|_| idx)
                        });
                    if let Some(idx) = job
                        && self.app.tabs.toggle_mute(idx).is_ok()
                    {
                        n += 1;
                    }
                }
                if n > 0 {
                    self.push_chrome(true);
                }
            }
            ChromeCommand::TabsPin { ids, pinned } => {
                // Grouped tabs refuse pinning (manager invariant): counted
                // as kept and reported, never silently dropped.
                let mut done = 0;
                let mut kept = 0;
                for s in &ids {
                    let Ok(tid) = s.parse::<Uuid>() else {
                        continue;
                    };
                    let job = self
                        .app
                        .tabs
                        .iter()
                        .position(|t| t.id == tid)
                        .and_then(|idx| {
                            self.app.tabs.iter().nth(idx).and_then(|t| {
                                if t.is_pinned == pinned {
                                    None
                                } else if t.group_id.is_some() && pinned {
                                    Some(Err(()))
                                } else {
                                    Some(Ok(idx))
                                }
                            })
                        });
                    match job {
                        Some(Ok(idx)) => {
                            if self.app.tabs.toggle_pin(idx).is_ok() {
                                done += 1;
                            }
                        }
                        Some(Err(())) => kept += 1,
                        None => {}
                    }
                }
                if done > 0 {
                    self.push_chrome(true);
                }
                if kept > 0 {
                    self.notify(format!("Kept {kept} grouped tabs unpinned."));
                }
            }
            ChromeCommand::CrashDismiss => {
                // Safety net: stash the declined session on the closed-tab
                // stack (active tab last) so Ctrl+Shift+T resurrects an
                // accidental dismiss. Still never re-asks for this crash:
                // the marker was already replaced at boot.
                let (_, tabs, active) = self.peek_session_docs();
                let active_entry = tabs.get(active).cloned();
                for (i, (url, title)) in tabs.into_iter().enumerate() {
                    if i != active {
                        self.remember_closed_bare(url, title);
                    }
                }
                // Active last: with the cap enforced per push, the active
                // tab survives even when the stack was already full.
                if let Some((url, title)) = active_entry {
                    self.remember_closed_bare(url, title);
                }
                self.crash_offer = false;
                self.layout_tabs();
                self.push_chrome(true);
            }
            ChromeCommand::Home => {
                let url = self.home_url.clone();
                if let Some(tab) = self.app.tabs.active_tab_mut() {
                    if let Err(e) = tab.load_url(url) {
                        debug!(error = %e, "home navigation failed");
                    } else {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                    }
                } else {
                    self.open_tab(url);
                }
                self.app.omnibox.clear();
                self.layout_tabs();
            }
            ChromeCommand::ToggleMaximize => {
                if let Some(window) = self.window.as_ref() {
                    // winit has no toggle primitive. Read-then-set is it.
                    window.set_maximized(!window.is_maximized());
                }
            }
            ChromeCommand::ZoomIn => self.zoom_step_active(1),
            ChromeCommand::ZoomOut => self.zoom_step_active(-1),
            ChromeCommand::ZoomReset => self.zoom_reset_active(),
            ChromeCommand::FocusOmnibox => self.eval_chrome("__ferriteFocusOmnibox()"),
        }
    }

    // ── Tick: drains + dirty-checked push ──

    fn tick(&mut self) {
        // Memory pressure feed (the discard table's sensor): real OS
        // numbers via sysinfo, at most every MEM_SAMPLE_INTERVAL. Unknown
        // readings keep the previous value; the manager clamps regardless.
        // Best-effort: a failed refresh simply skips this round.
        if self.mem_sample_at.elapsed() >= MEM_SAMPLE_INTERVAL {
            self.mem_sample_at = Instant::now();
            self.sys.refresh_memory();
            let total = self.sys.total_memory();
            let available = self.sys.available_memory();
            if let Some(p) = memory_pressure(total, available) {
                self.app.tabs.set_memory_pressure(p);
            }
        }
        // Native-share OG timeout: the extractor stayed silent (dead
        // webview, swallowed IPC). Dispatch the basic payload rather than
        // hanging the gesture.
        if let Some((id, since)) = self.pending_share
            && since.elapsed() > Duration::from_secs(2)
        {
            self.pending_share = None;
            self.native_share(id, None);
        }
        // Sensitive-clipboard clobber: a fresh vault secret was replaced
        // within seconds by something else (a page script with no user
        // gesture is the hostile case (pastejacking). The toast names the
        // fact, not the attribution: Ferrite cannot see who wrote it.
        if crate::clipboard::take_clobber_notice() {
            self.notify("Copied secret was replaced before it cleared".to_owned());
        }
        // Second-instance handovers: URLs a later launch handed to this
        // window (same mode only, token-authed at the forwarder). Each opens
        // as a normal tab; unparseable entries were already refused peer-side
        // and are re-checked here before navigation. A handover also raises
        // the window: the second launch exits, so this window must answer.
        let handed: Vec<String> = self
            .forward_queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        let mut raised = false;
        for url in handed {
            if crate::forward::is_forwardable_url(&url) {
                self.open_tab(url);
                raised = true;
            } else {
                debug!("forward queue dropped non-forwardable URL");
            }
        }
        if raised && let Some(window) = self.window.as_ref() {
            // Best-effort (minimized, behind other apps): unminimize,
            // then front. Both are infallible hints on every platform.
            window.set_minimized(false);
            window.focus_window();
        }
        // Incoming sync rows landed: refresh every surface they touch, once.
        if self
            .sync_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.refresh_sync_surfaces();
        }
        // Drain proxy telemetry for diagnostics.
        let mut telemetry_count = 0u64;
        // Top-level malware blocks surface here: the CONNECT dies before
        // any tab knows why (the engine just sees a dead tunnel).
        let mut malware_hosts: Vec<String> = Vec::new();
        // Dead-tunnel dial failures, same attribution problem. Every
        // failure re-attempts: pre-creation dials match nothing (no tab
        // yet) and later ones match once the tab exists.
        let mut failed_hosts: Vec<(String, String)> = Vec::new();
        if let Some(proxy) = self.proxy.as_ref() {
            while let Ok(ev) = proxy.telemetry.try_recv() {
                telemetry_count += 1;
                self.proxy_requests += 1;
                let is_malware_doc = ev.classification
                    == ferrite_proxy::Classification::BlockedMalware
                    && ev.resource_type == ferrite_proxy::ResourceType::Document;
                let failure_kind = if ev.resource_type == ferrite_proxy::ResourceType::Document {
                    ev.failure
                } else {
                    None
                };
                if is_malware_doc
                    && malware_hosts.len() < 8
                    && !malware_hosts.contains(&ev.request_domain)
                {
                    malware_hosts.push(ev.request_domain.clone());
                }
                if let Some(kind) = failure_kind
                    && failed_hosts.len() < 8
                    && !failed_hosts.iter().any(|(h, _)| h == &ev.request_domain)
                {
                    failed_hosts.push((ev.request_domain.clone(), kind.as_str().to_owned()));
                }
            }
            let dropped = proxy
                .telemetry_dropped
                .load(std::sync::atomic::Ordering::Relaxed);
            if dropped != self.proxy_dropped && dropped > 0 && dropped % 100 == 0 {
                warn!(dropped, "proxy telemetry dropped (backpressure)");
            }
            self.proxy_dropped = dropped;
        }
        // Plaintext HTTPS-upgrade fallbacks: the fetch succeeded, so
        // nothing toasts, but a growing count belongs in diagnostics
        // and logs, never in silence.
        if let Some(proxy) = self.proxy.as_ref() {
            let downgraded = proxy.downgraded.load(std::sync::atomic::Ordering::Relaxed);
            if downgraded != self.proxy_downgraded {
                self.proxy_downgraded = downgraded;
                debug!(downgraded, "https upgrades fell back to plaintext");
            }
        }
        // Background-tab DRM denials surface on activation: the live arm
        // only toasts the active tab, and without this flush a background
        // failure would never explain itself.
        if let Some(active) = self.app.tabs.active_id()
            && let Some(text) = self.drm_pending.remove(&active)
        {
            self.notify(text);
        }
        // Private proxy telemetry: same diagnostics counters, separate pool.
        // Forks from this proxy are intentionally never archived (archive is
        // None on that spawn); telemetry here only feeds counters/toasts.
        if let Some(proxy) = self.private_proxy.as_ref() {
            while proxy.telemetry.try_recv().is_ok() {
                telemetry_count += 1;
                self.private_proxy_requests += 1;
            }
            // Drain forks so a chatty private page can't backpressure the
            // proxy channel; payloads drop here, never to disk.
            while proxy.forks.try_recv().is_ok() {
                self.private_proxy_dropped += 1;
            }
            let dropped = proxy
                .telemetry_dropped
                .load(std::sync::atomic::Ordering::Relaxed);
            if dropped != self.proxy_dropped && dropped > 0 && dropped % 100 == 0 {
                warn!(dropped, "private proxy telemetry dropped (backpressure)");
            }
        }
        if !malware_hosts.is_empty() {
            self.show_blocked_pages(&malware_hosts);
        }
        if !failed_hosts.is_empty() {
            self.show_error_pages(&failed_hosts);
        }

        // Drain download events into a capped ring for the status strip.
        let dl_events = self.downloads.drain(32);
        let dl_count = dl_events.len();
        for ev in dl_events {
            use ferrite_tabs::DownloadStateKind as Ds;
            if matches!(ev.state, Ds::Completed | Ds::Failed) {
                info!(url = %ev.url, filename = %ev.filename, state = ?ev.state, "download finished");
            }
            if matches!(ev.state, Ds::Completed) {
                // Opens the toolbar checkmark flash window and queues the
                // completion toast (same-source completions coalesce into
                // one frame; "3 downloads complete", never three popups).
                self.last_dl_done = Some(Instant::now());
                self.toasts.push(
                    crate::toast::ToastItem::download(&ev.filename, &ev.id),
                    Instant::now(),
                );
            }
            // Terminal states persist to the vault (survives restart) and
            // completed files get hashed + marked for the OS open prompt.
            if matches!(ev.state, Ds::Completed | Ds::Failed | Ds::Cancelled) {
                self.finalize_download(&ev);
            }
            self.download_log.push_back(ev);
            while self.download_log.len() > 200 {
                self.download_log.pop_front();
            }
        }
        if dl_count > 0 {
            // Fresh download events may flip protection: sync flags now,
            // the 30s backstop reconciles anything the queue drops.
            self.sync_download_flags();
        }

        let mut dirty = telemetry_count > 0 || dl_count > 0;
        for msg in self.ipc_bridge.drain() {
            // Defense in depth: the bridge caps at 64KB, but re-check here
            // (older senders, tests, future callers).
            if msg.payload.len() > ferrite_tabs::IPC_MAX_PAYLOAD_BYTES {
                warn!(
                    tab_id = %msg.tab_id,
                    bytes = msg.payload.len(),
                    "ipc oversize at drain: dropping"
                );
                continue;
            }
            dirty = true;
            // `cmd` envelopes are honored ONLY from the chrome webview. A
            // compromised content page posting one is routed to the legacy
            // path below, where it is ignored.
            if msg.tab_id == self.chrome_id {
                match ChromeCommand::parse(&msg.payload) {
                    Ok(cmd) => {
                        self.handle_chrome_command(cmd);
                        continue;
                    }
                    // Init-script telemetry ({type:...}) runs in every
                    // webview including chrome: not a command, not hostile.
                    // Fall through to the shared path when it carries a
                    // `type` field so it is counted, not dropped. The shared
                    // path keys privilege off tab lookup (chrome_id is not a
                    // tab), so only allowlisted telemetry lands there.
                    // Anything else is still rejected below.
                    Err(e) => {
                        if !chrome_payload_is_telemetry(&msg.payload) {
                            debug!(
                                error = %e,
                                bytes = msg.payload.len(),
                                cmd = ChromeCommand::envelope_cmd(&msg.payload),
                                "chrome envelope rejected"
                            );
                            continue;
                        }
                    }
                }
            }
            debug!(tab_id = %msg.tab_id, payload_len = msg.payload.len(), "ipc");
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg.payload) else {
                continue;
            };
            let msg_type = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
            // Default-deny origin check: content pages legitimately post
            // telemetry (hover/audio/find/context/scan/reader/console/
            // title), the overlay UIs (tabsearch, ctxmenu, random-UUID
            // webviews outside the tab manager, so they can't prove
            // ferrite:// by lookup), and the validated form/autofill
            // flows below. Everything else, file exec
            // (dl_open/dl_folder), vault mutations, clipboard, settings
            // writes, requires a privileged (ferrite://) sender.
            // ponytail: allowlist, not per-arm guards. Future arms are
            // covered by default. Every type admitted here is validated
            // again inside its own arm (caps, scheme gates, tab URLs).
            let privileged = self
                .app
                .tabs
                .get_tab(msg.tab_id)
                .map(|t| t.url.starts_with("ferrite://"))
                .unwrap_or(false);
            if !privileged && !content_page_allowed(msg_type) {
                debug!(msg_type, tab_id = %msg.tab_id, "ipc: unprivileged ignored");
                continue;
            }
            // Boot gate: while blocked, only the decision tab's decision
            // posts (plus its title) are honored. Everything else
            // (navigation, searches, settings) waits for the decision.
            if let Some(block) = self.boot_block.as_ref() {
                let is_decision = Some(msg.tab_id) == block.tab;
                let allowed = is_decision
                    && matches!(msg_type, "bootdecision" | "bootdecision_data" | "title");
                if !allowed {
                    debug!(msg_type, tab_id = %msg.tab_id, "ipc: boot gate blocked");
                    continue;
                }
            }
            match msg_type {
                "audio" => {
                    let playing = v.get("playing").and_then(|x| x.as_bool()).unwrap_or(false);
                    if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        tab.set_audio_playing(playing);
                    }
                }
                "form_dirty" => {
                    // Latch-only (the reporter posts once per document):
                    // feeds the never-sleep-on-unsaved-forms exemption.
                    if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        tab.has_unsaved_form = true;
                    }
                }
                "live_call" => {
                    // Level signal (reporter posts on transitions both
                    // ways): feeds the never-sleep-on-calls exemption.
                    let active = v.get("active").and_then(|x| x.as_bool()).unwrap_or(false);
                    if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        tab.has_live_call = active;
                    }
                }
                "scroll" => {
                    // Live offset for session restore. Validated finite and
                    // clamped (a hostile page must not poison layout math).
                    // No push, no dirty flag: the 30s autosave backstop and
                    // clean-exit save persist it; per-scroll writes would
                    // churn the disk for zero restore gain.
                    let x = v.get("x").and_then(|n| n.as_f64()).unwrap_or(0.0);
                    let y = v.get("y").and_then(|n| n.as_f64()).unwrap_or(0.0);
                    if x.is_finite()
                        && y.is_finite()
                        && let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id)
                    {
                        let next = (x.clamp(-1e7, 1e7) as f32, y.clamp(-1e7, 1e7) as f32);
                        if tab.scroll_position != next {
                            tab.scroll_position = next;
                            self.scroll_dirty = true;
                        }
                    }
                }
                "lang" => {
                    // Page-reported `lang` attribute (untrusted: validated
                    // before it lands). Privileged pages clear the offer,
                    // whatever they post. Changed values push at once so
                    // the offer appears with the page, not a tick later.
                    let raw = v.get("lang").and_then(|x| x.as_str()).unwrap_or("");
                    let validated = ferrite_tabs::translate::valid_page_lang(raw);
                    let privileged = self
                        .app
                        .tabs
                        .get_tab(msg.tab_id)
                        .map(|t| t.url.starts_with("ferrite://"))
                        .unwrap_or(true);
                    if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        let next = if privileged { None } else { validated };
                        if tab.page_lang != next {
                            tab.set_page_lang(next);
                            self.push_chrome(true);
                        }
                    }
                }
                "pip" => {
                    // Page-reported eligibility + activity. Candidates are
                    // re-scored Rust-side from the same payload (the page
                    // cannot light the button without a real candidate),
                    // and ad-hosted sources are demoted through the live
                    // filter lists before the flags land.
                    let report = ferrite_tabs::parse_report(&v);
                    let referrer = self.app.tabs.get_tab(msg.tab_id).map(|t| t.url.clone());
                    // Privileged pages carry no video: never light the
                    // button from ferrite://, whatever the page posts.
                    let privileged_page = referrer
                        .as_deref()
                        .is_some_and(|u| u.starts_with("ferrite://"));
                    if privileged_page {
                        if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                            tab.set_pip_report(false, false);
                        }
                        continue;
                    }
                    let mut eligible = report.eligible;
                    if eligible && !report.candidates.is_empty() {
                        let best = ferrite_tabs::pick_candidate(
                            &report.candidates,
                            &self.privacy.recent_tracker_hosts(32),
                        );
                        if let (Some(r), Some(i)) = (referrer.as_deref(), best)
                            && let Some(c) = report.candidates.get(i)
                            && !c.src.is_empty()
                            && self.privacy.is_blocked_media(&c.src, Some(r))
                        {
                            // The best candidate is itself an ad: only stay
                            // eligible when a non-ad candidate also plays.
                            eligible = report.candidates.iter().any(|c| {
                                c.playing
                                    && !c.src.is_empty()
                                    && !self.privacy.is_blocked_media(&c.src, Some(r))
                            });
                        }
                    }
                    if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        tab.set_pip_report(eligible, report.active);
                    }
                    // Late reporter: auto-PiP is on, the video started after
                    // the switch, and this tab sits in the background.
                    if self.pip_auto
                        && eligible
                        && !report.active
                        && self.app.tabs.active_id() != Some(msg.tab_id)
                    {
                        self.pip_enter_tab(msg.tab_id);
                    }
                }
                "pip_error" => {
                    // Explicit-click failures land here (no-video, engine
                    // refusal). One quiet toast. The button already told
                    // the story, this just names the outcome.
                    let reason = v.get("reason").and_then(|x| x.as_str()).unwrap_or("");
                    debug!(reason, tab_id = %msg.tab_id, "pip request failed");
                    if self.app.tabs.active_id() == Some(msg.tab_id) {
                        self.notify("Picture-in-Picture isn't available here.".to_owned());
                    }
                }
                "share_meta" => {
                    // OG enrichment for a pending native share. Tab id must
                    // match the gesture (a background page cannot trigger a
                    // sheet for another tab). The payload rebuilds from the
                    // live tab at dispatch.
                    let meta = ferrite_tabs::share::parse_meta(&v);
                    if self.pending_share.is_some_and(|(id, _)| id == msg.tab_id) {
                        self.pending_share = None;
                        self.native_share(msg.tab_id, Some(meta));
                    }
                }
                "share_copy" => {
                    // Share-page copy: re-validate like every privileged
                    // write (the page's query string is not trusted input).
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    match ferrite_tabs::share::clean_url(url) {
                        Some(clean) => {
                            if self.copy_with_toast(&clean, "Link") {
                                self.confirm_shared(msg.tab_id, "Link copied to clipboard");
                            }
                        }
                        None => self.notify("There's nothing shareable here.".to_owned()),
                    }
                }
                "share_markdown" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    let title: String = v
                        .get("title")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(ferrite_tabs::share::SHARE_MAX_TITLE_CHARS)
                        .collect();
                    match ferrite_tabs::share::clean_url(url) {
                        Some(clean) => {
                            let md = ferrite_tabs::share::markdown_link(&title, &clean);
                            if self.copy_with_toast(&md, "Markdown link") {
                                self.confirm_shared(
                                    msg.tab_id,
                                    "Markdown link copied to clipboard",
                                );
                            }
                        }
                        None => self.notify("There's nothing shareable here.".to_owned()),
                    }
                }
                "share_mailto" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    let title: String = v
                        .get("title")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(ferrite_tabs::share::SHARE_MAX_TITLE_CHARS)
                        .collect();
                    match ferrite_tabs::share::clean_url(url) {
                        Some(clean) => {
                            let mailto = ferrite_tabs::share::mailto_url(&title, &clean);
                            if let Err(e) = crate::share::launch_mailto(&mailto) {
                                debug!(error = %e, "mailto launch failed");
                                self.notify("Couldn't open your mail app.".to_owned());
                            }
                        }
                        None => self.notify("There's nothing shareable here.".to_owned()),
                    }
                }
                "console_error" => {
                    if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        tab.console_errors = tab.console_errors.saturating_add(1);
                    }
                }
                "favicon" => {
                    // Engine-reported icon URL only (never bitmaps over IPC).
                    // Validated here, fetched on a worker, applied on drain
                    // with a page-URL staleness check.
                    let icon = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if !crate::favicon::valid_icon_url(icon) {
                        debug!("favicon rejected: bad url");
                    } else if let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id) {
                        let page_url = tab.url.clone();
                        // Privileged pages keep letter avatars, never remote icons.
                        if page_url.starts_with("ferrite://") {
                            debug!("favicon rejected: privileged page");
                        } else {
                            // Host cache hit paints immediately without network.
                            if let Some(data) = self.favicon_cache.get(icon).cloned() {
                                tab.set_favicon_url(icon.to_owned());
                                tab.set_favicon_data(data);
                                self.app.tabs.mark_dirty();
                            } else {
                                tab.set_favicon_url(icon.to_owned());
                                self.app.tabs.mark_dirty();
                                if self.favicon_pending.insert(icon.to_owned()) {
                                    crate::favicon::spawn_favicon_fetch(
                                        msg.tab_id,
                                        page_url,
                                        icon.to_owned(),
                                        self.favicon_tx.clone(),
                                    );
                                }
                            }
                        }
                    }
                }
                "bootdecision_data" => {
                    // Reasons push (page requests on load and after every
                    // failed Retry, since reasons change).
                    self.push_bootdecision(msg.tab_id, None);
                }
                "bootdecision" => {
                    let choice = v.get("choice").and_then(|x| x.as_str()).unwrap_or("");
                    // Capped: the choice is one of three words, never free
                    // text echoed anywhere.
                    if choice.len() <= 16 {
                        self.resolve_boot_choice(msg.tab_id, choice);
                    } else {
                        debug!("bootdecision ignored: oversize choice");
                    }
                }
                "bootrecovery" => {
                    // Recovery phrase entry: length-capped (24 words fit in
                    // a few hundred chars); the unwrap fails closed on
                    // wrong phrases, corrupt sidecars, and wrong lengths.
                    let phrase = v.get("phrase").and_then(|x| x.as_str()).unwrap_or("");
                    if phrase.len() <= 2000 {
                        self.resolve_boot_recovery(msg.tab_id, phrase);
                    } else {
                        debug!("bootrecovery ignored: oversize phrase");
                    }
                }
                "title" | "" => {
                    // Titles are accepted ONLY on title-typed (or legacy
                    // untyped) messages. Any other typed message carrying a
                    // `title` field must not clobber the tab title
                    // (title-spoof from a compromised page).
                    if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
                        let title: String =
                            t.chars().take(ferrite_tabs::IPC_MAX_TITLE_CHARS).collect();
                        if !title.is_empty()
                            && let Some(tab) = self.app.tabs.get_tab_mut(msg.tab_id)
                        {
                            tab.set_title(title);
                            self.app.tabs.mark_dirty();
                        }
                    }
                }
                "nav" => {
                    // Engine-committed top-level navigation (main-frame-gated
                    // reporter. Subframes never post). Liveness trusted,
                    // content validated inside record_engine_navigation.
                    if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
                        self.record_engine_navigation(msg.tab_id, u);
                    }
                }
                "settings" => {
                    let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("");
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    // Unknown keys change nothing: the log below must not
                    // claim a toggle that never happened.
                    let mut known = true;
                    match key {
                        "adblock" => self.privacy.update_settings(|s| s.adblock_enabled = value),
                        "gpc" => self.privacy.update_settings(|s| s.gpc_enabled = value),
                        "realtime" => {
                            self.privacy
                                .update_settings(|s| s.realtime_lookup_enabled = value);
                            if value
                                && self
                                    .privacy
                                    .settings()
                                    .realtime_api_key
                                    .as_ref()
                                    .is_none_or(|k| k.trim().is_empty())
                            {
                                // The toggle without a key is inert by
                                // construction (no egress, fail-open). Say
                                // so now, not after a silent no-op.
                                self.notify(
                                    "Real-time lookup needs a Safe Browsing API key. See the field below."
                                        .to_owned(),
                                );
                            }
                        }
                        "https_up" => self
                            .privacy
                            .update_settings(|s| s.https_upgrade_enabled = value),
                        "spellcheck" => self
                            .privacy
                            .update_settings(|s| s.spellcheck_enabled = value),
                        "translate_enabled" => self.translate_enabled = value,
                        "notifications" => {
                            self.permissions.set_notifications_global(value);
                            persist_setting("notifications_global", serde_json::Value::Bool(value));
                        }
                        "profile_picker" => {
                            self.profile_picker = value;
                            persist_setting("profile_picker", serde_json::Value::Bool(value));
                        }
                        _ => {
                            debug!(key, "unknown privacy setting ignored");
                            known = false;
                        }
                    };
                    if known {
                        self.persist_privacy_settings();
                        info!(key, value, "privacy setting toggled");
                    }
                    if key == "spellcheck" {
                        self.apply_spellcheck_all();
                    }
                    if key == "translate_enabled" {
                        persist_setting("translate_enabled", serde_json::Value::Bool(value));
                    }
                }
                "realtime_key" => {
                    // Safe Browsing API key (secret: never pushed back to
                    // any page; the settings push carries only a set
                    // flag). Empty clears. Persisted in the sealed
                    // settings file with the other privacy fields.
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let trimmed = raw.trim();
                    let key = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_owned())
                    };
                    let had = self
                        .privacy
                        .settings()
                        .realtime_api_key
                        .as_ref()
                        .is_some_and(|k| !k.trim().is_empty());
                    self.privacy.update_settings(|s| s.realtime_api_key = key);
                    self.persist_privacy_settings();
                    self.push_settings(msg.tab_id);
                    self.notify(
                        if self.privacy.settings().realtime_api_key.is_some() {
                            "Safe Browsing API key saved."
                        } else if had {
                            "Safe Browsing API key cleared; lookup is inert."
                        } else {
                            "No API key was set; lookup stays inert."
                        }
                        .to_owned(),
                    );
                    info!("realtime api key updated");
                }
                "doh_endpoint" => {
                    // Encrypted DNS endpoint (secret-adjacent: it names the
                    // resolver that sees query metadata, so it shares the
                    // sealed file but never a page push beyond its value).
                    // Empty clears back to OS DNS. Anything else must be an
                    // https JSON endpoint: invalid values are refused with
                    // words, never probed, never persisted.
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        self.privacy.update_settings(|s| s.doh_endpoint = None);
                        self.persist_privacy_settings();
                        self.push_settings(msg.tab_id);
                        self.notify("Encrypted DNS off. OS DNS in the clear.".to_owned());
                    } else if let Some(why) = (ferrite_proxy::DnsConfig {
                        doh_endpoint: Some(trimmed.to_owned()),
                    })
                    .endpoint_problem()
                    {
                        self.notify(format!("DoH endpoint refused ({why}); keeping current."));
                    } else {
                        self.privacy
                            .update_settings(|s| s.doh_endpoint = Some(trimmed.to_owned()));
                        self.persist_privacy_settings();
                        self.push_settings(msg.tab_id);
                        self.notify("Encrypted DNS on for proxy traffic.".to_owned());
                    }
                    info!("doh endpoint updated");
                }
                "spellcheck_langs" => {
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let langs = ferrite_privacy::normalize_langs(raw);
                    self.privacy.update_settings(|s| s.spellcheck_langs = langs);
                    self.persist_privacy_settings();
                    self.apply_spellcheck_all();
                    // Explicit user action: adding a language consents to
                    // fetching its dictionary (unlike background traffic).
                    self.provision_missing_dicts();
                    self.push_settings(msg.tab_id);
                    info!("spellcheck languages updated");
                }
                "rotate_key" => {
                    // Explicit user action from the settings page button
                    // (not a toggle): re-encrypt every store under a fresh
                    // device key on a worker thread. Refusals and the
                    // outcome both arrive as toasts; failures name the
                    // remedy and preserve crash-resume via the journal.
                    self.spawn_rotation();
                    info!("key rotation requested");
                }
                "recovery_show" => {
                    // Explicit user action: mint a fresh phrase, wrap the
                    // live key, store the sidecar, show the phrase once via
                    // the dedicated hook (never through the settings value
                    // push: it must not linger in any state).
                    if is_ephemeral() {
                        self.notify("Recovery phrases aren't available in Incognito.".to_owned());
                    } else {
                        match Self::cred_key() {
                            Some(mut key) => {
                                match crate::recovery::enable_sidecar(&key) {
                                    Ok(phrase) => self.push_recovery(msg.tab_id, Some(&phrase)),
                                    Err(e) => {
                                        debug!(error = %e, "recovery enable failed");
                                        self.notify(
                                            "Couldn't create a recovery phrase.".to_owned(),
                                        );
                                    }
                                }
                                key.zeroize();
                            }
                            None => {
                                self.notify("Recovery needs the device key available.".to_owned())
                            }
                        }
                    }
                    info!("recovery phrase generated");
                }
                "recovery_disable" => {
                    if crate::recovery::disable_sidecar() {
                        self.push_recovery(msg.tab_id, None);
                        info!("recovery disabled");
                    } else {
                        self.notify("No recovery phrase is set up.".to_owned());
                    }
                }
                "translate_target" => {
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let target = ferrite_tabs::translate::normalize_target(raw);
                    self.translate_target = target.clone();
                    persist_setting("translate_target", serde_json::Value::String(target));
                    self.push_settings(msg.tab_id);
                    info!("translate target updated");
                }
                "settings_query" => {
                    self.push_settings(msg.tab_id);
                }
                "suggestions" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(true);
                    self.suggestions_enabled = value;
                    persist_setting("suggestions", serde_json::Value::Bool(value));
                    info!(value, "suggestions toggled");
                }
                "suggestions_remote" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    self.remote_enabled = value;
                    persist_setting("suggestions_remote", serde_json::Value::Bool(value));
                    if !value {
                        // Stop showing in-flight rows: bump the generation
                        // so late replies die, clear the accepted ones.
                        self.sug_gen += 1;
                        self.remote_sugs = (self.sug_gen, String::new(), Vec::new());
                        self.push_chrome(true);
                    }
                    info!(value, "remote suggestions toggled");
                }
                "restore_always" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    self.restore_always = value;
                    persist_setting("restore_always", serde_json::Value::Bool(value));
                    info!(value, "restore-always toggled");
                }
                "startup_mode" => {
                    let mode = v.get("value").and_then(|x| x.as_str()).unwrap_or("restore");
                    let clean = clean_startup_mode(mode).to_owned();
                    self.startup_mode = clean.clone();
                    persist_setting("startup_mode", serde_json::Value::String(clean));
                    self.push_settings(msg.tab_id);
                    info!(mode, "startup mode set");
                }
                "startup_url_add" => {
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let u = raw.trim().to_owned();
                    if valid_home_url(&u) && !self.startup_urls.contains(&u) {
                        self.startup_urls.push(u);
                        while self.startup_urls.len() > MAX_STARTUP_URLS {
                            self.startup_urls.remove(0);
                        }
                        persist_setting(
                            "startup_urls",
                            serde_json::Value::from(self.startup_urls.clone()),
                        );
                        self.push_settings(msg.tab_id);
                    } else {
                        self.notify("That doesn't look like a page URL.".to_owned());
                    }
                }
                "startup_url_remove" => {
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let before = self.startup_urls.len();
                    self.startup_urls.retain(|u| u != raw);
                    if self.startup_urls.len() != before {
                        persist_setting(
                            "startup_urls",
                            serde_json::Value::from(self.startup_urls.clone()),
                        );
                        self.push_settings(msg.tab_id);
                    }
                }
                "history_retention" => {
                    // Days as u64 (0 = forever); absurd values clamp to
                    // the documented range instead of erroring the row.
                    let days = v.get("value").and_then(|x| x.as_u64()).unwrap_or(90);
                    let days = if days == 0 { 0 } else { days.clamp(7, 3650) };
                    self.history_retention_days = days;
                    persist_setting("history_retention_days", serde_json::Value::from(days));
                    self.push_settings(msg.tab_id);
                    info!(days, "history retention set");
                }
                "sleep_timeout" => {
                    let minutes = v
                        .get("value")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(30)
                        .min(24 * 60);
                    self.sleep_timeout_min = minutes;
                    self.app
                        .tabs
                        .set_idle_timeout(std::time::Duration::from_secs(minutes * 60));
                    persist_setting("sleep_timeout_min", serde_json::Value::from(minutes));
                    info!(minutes, "sleep timeout set");
                    self.push_settings(msg.tab_id);
                }
                "sleep_exception_add" => {
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    match clean_sleep_host(raw) {
                        Some(host) if !self.sleep_exceptions.contains(&host) => {
                            self.sleep_exceptions.push(host);
                            self.sleep_exceptions.sort();
                            self.app
                                .tabs
                                .set_sleep_exceptions(self.sleep_exceptions.clone());
                            persist_setting(
                                "sleep_exceptions",
                                serde_json::Value::from(self.sleep_exceptions.clone()),
                            );
                            self.push_settings(msg.tab_id);
                        }
                        _ => self.notify("That doesn't look like a host.".to_owned()),
                    }
                }
                "sleep_exception_remove" => {
                    let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                    let before = self.sleep_exceptions.len();
                    self.sleep_exceptions.retain(|h| h != raw);
                    if self.sleep_exceptions.len() != before {
                        self.app
                            .tabs
                            .set_sleep_exceptions(self.sleep_exceptions.clone());
                        persist_setting(
                            "sleep_exceptions",
                            serde_json::Value::from(self.sleep_exceptions.clone()),
                        );
                        self.push_settings(msg.tab_id);
                    }
                }
                "tabs_vertical" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    self.set_tabs_vertical(value);
                    info!(value, "vertical tabs toggled");
                }
                "autofill_address" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(true);
                    self.autofill_address = value;
                    persist_setting("autofill_address", serde_json::Value::Bool(value));
                    info!(value, "autofill-address toggled");
                }
                "autofill_payment" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(true);
                    self.autofill_payment = value;
                    persist_setting("autofill_payment", serde_json::Value::Bool(value));
                    info!(value, "autofill-payment toggled");
                }
                "autofill_form" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(true);
                    self.autofill_form = value;
                    persist_setting("autofill_form", serde_json::Value::Bool(value));
                    if !value
                        && let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.clear_form_history()
                    {
                        debug!(error = %e, "form history wipe failed");
                    }
                    info!(value, "autofill-form toggled");
                }
                "ask_downloads" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    self.ask_downloads
                        .store(value, std::sync::atomic::Ordering::Relaxed);
                    persist_setting("ask_downloads", serde_json::Value::Bool(value));
                    info!(value, "ask-downloads toggled");
                }
                "pip_auto" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    self.pip_auto = value;
                    persist_setting("pip_auto", serde_json::Value::Bool(value));
                    info!(value, "auto-pip toggled");
                }
                "exclusions_query" => {
                    self.push_exclusions(msg.tab_id);
                }
                "site_query" => {
                    let raw = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
                    match clean_site_origin(raw) {
                        Some(origin) => self.push_site(msg.tab_id, &origin),
                        None => debug!("site query ignored: bad origin"),
                    }
                }
                "site_forget" => {
                    let raw = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
                    match clean_site_origin(raw) {
                        Some(origin) => self.forget_site(msg.tab_id, &origin),
                        None => debug!("site forget ignored: bad origin"),
                    }
                }
                // PDF viewer (ferrite://pdf/, privileged sender). Every arm
                // re-validates the (tab, doc) pairing: a page drives only
                // its own document, never a sibling tab's.
                "pdf_doc" => {
                    if let Some(doc) = self.pdf_arm_doc(msg.tab_id, &v) {
                        match self.pdf_doc_bundle(doc) {
                            Ok(bundle) => self.push_pdf(msg.tab_id, "__ferritePdfDoc", &bundle),
                            Err(e) => self.notify(format!("Couldn't open PDF: {e}")),
                        }
                    }
                }
                "pdf_page" => {
                    if let Some((doc, page, scale, query, thumb)) =
                        self.pdf_arm_page(msg.tab_id, &v)
                    {
                        match self.pdf_page_bundle(doc, page, scale, query, thumb) {
                            Ok(bundle) => {
                                let hook = if thumb {
                                    "__ferritePdfThumb"
                                } else {
                                    "__ferritePdfPage"
                                };
                                self.push_pdf(msg.tab_id, hook, &bundle)
                            }
                            Err(e) => self.push_pdf(
                                msg.tab_id,
                                "__ferritePdfError",
                                &serde_json::json!({"doc": doc.to_string(), "error": e}),
                            ),
                        }
                    }
                }
                "pdf_search" => {
                    if let Some(doc) = self.pdf_arm_doc(msg.tab_id, &v) {
                        let q = v.get("q").and_then(|x| x.as_str()).unwrap_or("");
                        let case = v.get("case").and_then(|x| x.as_bool()).unwrap_or(false);
                        match self.pdf_sessions.search_all(doc, q.to_owned(), case) {
                            Ok(res) => self.push_pdf(msg.tab_id, "__ferritePdfSearch", &res),
                            Err(e) => self.notify(format!("PDF search failed: {e}")),
                        }
                    }
                }
                "pdf_fill" => {
                    if let Some(doc) = self.pdf_arm_doc(msg.tab_id, &v) {
                        let index =
                            v.get("index")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(usize::MAX as u64) as usize;
                        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                        let res = match kind {
                            "text" => {
                                let val = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                                self.pdf_sessions.fill_text(doc, index, val.to_owned())
                            }
                            "check" => {
                                let on =
                                    v.get("checked").and_then(|x| x.as_bool()).unwrap_or(false);
                                self.pdf_sessions.fill_check(doc, index, on)
                            }
                            "radio" => self.pdf_sessions.fill_radio(doc, index),
                            _ => Err("unknown field kind".to_owned()),
                        };
                        match res {
                            Ok(()) => {
                                let fields = self.pdf_sessions.doc_forms(doc).unwrap_or_default();
                                self.push_pdf(msg.tab_id, "__ferritePdfForms", &fields);
                            }
                            Err(e) => self.notify(format!("Couldn't fill field: {e}")),
                        }
                    }
                }
                "pdf_save" => {
                    if let Some(doc) = self.pdf_arm_doc(msg.tab_id, &v) {
                        self.pdf_save_copy(msg.tab_id, doc);
                    }
                }
                "pdf_close" => {
                    if let Some(doc) = self.pdf_arm_doc(msg.tab_id, &v) {
                        self.pdf_sessions.close(doc);
                    }
                }
                "pdf_link" => {
                    // External document links open in a NEW tab: navigating
                    // the viewer away would drop its own session (by
                    // design), stranding the click. Re-validated here even
                    // though the page only ever receives http(s) URLs.
                    if self.pdf_arm_doc(msg.tab_id, &v).is_some() {
                        let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                        if (url.starts_with("http://") || url.starts_with("https://"))
                            && url.len() <= 2048
                            && !url.chars().any(char::is_control)
                        {
                            self.open_tab(url.to_owned());
                        } else {
                            debug!("pdf link ignored: bad url");
                        }
                    }
                }
                "pdf_unlock" => {
                    if let Some(doc) = self.pdf_arm_doc(msg.tab_id, &v) {
                        let pwd = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
                        match self.pdf_sessions.unlock(doc, pwd.to_owned()) {
                            Ok(_) => match self.pdf_doc_bundle(doc) {
                                Ok(bundle) => self.push_pdf(msg.tab_id, "__ferritePdfDoc", &bundle),
                                Err(e) => self.notify(format!("Couldn't open PDF: {e}")),
                            },
                            Err(e) => self.notify(format!("Couldn't unlock PDF: {e}")),
                        }
                    }
                }
                "pdf_open_file" => {
                    let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
                    if path.is_empty() || path.len() > 1024 {
                        debug!("pdf open file ignored: bad path");
                    } else {
                        let meta = std::fs::metadata(path);
                        let ok_len = meta.map(|m| m.len()).unwrap_or(0);
                        if ok_len == 0 || ok_len > crate::pdf::MAX_PDF_BYTES as u64 {
                            self.notify("Couldn't open PDF: file length out of bounds.".to_owned());
                        } else {
                            match std::fs::read(path) {
                                Ok(bytes) => {
                                    let title = std::path::Path::new(path)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("document.pdf")
                                        .to_owned();
                                    self.pdf_open_bytes(
                                        msg.tab_id,
                                        title,
                                        path.to_owned(),
                                        bytes,
                                        None,
                                    );
                                }
                                Err(e) => {
                                    debug!(error = %e, "pdf open file read failed");
                                    self.notify("Couldn't open PDF: file unreadable.".to_owned());
                                }
                            }
                        }
                    }
                }
                "profiles_query" => {
                    self.push_profiles(msg.tab_id);
                }
                "profile_pick" => {
                    // Switch by spawning a sibling (side-by-side windows):
                    // unknown ids never reach the command line. Picking
                    // Default in a Default window just continues below.
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if id.eq_ignore_ascii_case(&self.profile_id) {
                        self.close_picker_tab();
                        self.profile_pick_pending = false;
                        self.boot_tabs();
                    } else if id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID)
                        || crate::profiles::list().iter().any(|p| p == id)
                    {
                        self.spawn_profile(id);
                        self.quit_requested = true;
                    } else {
                        debug!(profile = %id, "profile pick ignored: unknown id");
                    }
                    self.push_chrome(true);
                }
                "profile_default" => {
                    self.close_picker_tab();
                    self.profile_pick_pending = false;
                    self.boot_tabs();
                    self.push_chrome(true);
                }
                "profile_incognito" => {
                    // In-place ephemeral entry (gate Guest precedent):
                    // picker closes, boot proceeds with nothing persistent.
                    if self.enter_ephemeral() {
                        self.close_picker_tab();
                        self.profile_pick_pending = false;
                        self.boot_tabs();
                        self.notify(
                            "Incognito session: nothing will be saved. Close the window to vanish."
                                .to_owned(),
                        );
                        self.push_chrome(true);
                    }
                }
                "profile_quit" => {
                    info!("profile picker quit requested");
                    self.quit_requested = true;
                }
                "profile_picker" => {
                    // Page-level twin of the settings key below (same
                    // name, different channel): the checkbox on the
                    // picker writes the real setting.
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(true);
                    self.profile_picker = value;
                    persist_setting("profile_picker", serde_json::Value::Bool(value));
                    info!(value, "profile picker toggled");
                }
                "exclusion_add" => {
                    // Stored as a bare host (history_excluded matches hosts,
                    // not URLs). The same slice the context menu uses.
                    let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
                    let host = host_slice(origin).to_lowercase();
                    if let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.add_excluded_origin(&host)
                    {
                        debug!(error = %e, "exclusion add failed");
                    }
                    self.push_exclusions(msg.tab_id);
                }
                "exclusion_remove" => {
                    let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.remove_excluded_origin(origin)
                    {
                        debug!(error = %e, "exclusion remove failed");
                    }
                    self.push_exclusions(msg.tab_id);
                }
                "perm_grants_query" => {
                    self.push_perm_grants(msg.tab_id);
                }
                "perm_grant_revoke" => {
                    let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
                    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                    self.permissions.revoke_always(origin, kind);
                    self.persist_perm_snapshot();
                    self.push_perm_grants(msg.tab_id);
                }
                "sync_devices" => {
                    self.push_sync_devices(msg.tab_id);
                }
                "sync_code_show" => {
                    // Explicit user action: render this device's pairing
                    // code for manual exchange (copy it to the other device).
                    // Public by design (id + public key); approval on the
                    // other end still binds the key before anything is stored.
                    match self.sync_trust.as_ref() {
                        Some(t) => {
                            let payload = ferrite_sync::PairingPayload {
                                ip: best_lan_ip(),
                                port: 0,
                                device_id: t.this.identity.device_id.to_string(),
                                public_key: t.this.identity.public_key_bytes(),
                                device_name: t.this.name.clone(),
                                version: ferrite_sync::pairing::PAIRING_VERSION,
                                created_at_ms: Some(now_millis()),
                            };
                            // The page counts the 15-minute TTL down on its
                            // own clock from this instant (v1 codes rot).
                            let expires_at_ms = payload.created_at_ms.map(|t| {
                                t.saturating_add(ferrite_sync::pairing::PAIRING_CODE_TTL_MS)
                            });
                            self.push_sync_code(msg.tab_id, Some(&payload.encode()), expires_at_ms);
                        }
                        None => self.notify("Sync needs the vault available.".to_owned()),
                    }
                }
                "sync_pair_submit" => {
                    // Manual pairing, step one: decode a pasted code and hold
                    // it for explicit Approve/Deny (never auto-trusted). Caps
                    // mirror the IPC envelope: over-long codes are refused,
                    // not truncated (truncation would corrupt the key).
                    let code = v.get("code").and_then(|x| x.as_str()).unwrap_or("");
                    if self.sync_trust.is_none() {
                        self.notify("Sync needs the vault available.".to_owned());
                    } else if code.len() > 4096 {
                        self.notify("That pairing code is too long.".to_owned());
                    } else {
                        match ferrite_sync::PairingPayload::decode(code) {
                            Some(p)
                                if !p.device_id.is_empty()
                                    && p.device_id.len() <= 128
                                    && p.public_key.len() == 32
                                    && p.device_name.len() <= 128 =>
                            {
                                if p.version > ferrite_sync::pairing::PAIRING_VERSION {
                                    // Future versions must never misparse
                                    // as current: upgrade this side first.
                                    self.notify(
                                        "That code is from a newer Ferrite. Update to pair."
                                            .to_owned(),
                                    );
                                } else if !p.is_fresh(now_millis()) {
                                    // Shown codes rot in 15 minutes: stale
                                    // screenshots and shoulder copies die.
                                    self.notify(
                                        "That pairing code expired. Show a fresh one.".to_owned(),
                                    );
                                } else {
                                    self.pending_pair = Some(p);
                                    self.push_sync_devices(msg.tab_id);
                                }
                            }
                            _ => self.notify("That pairing code didn't scan.".to_owned()),
                        }
                    }
                }
                "sync_relay_save" => {
                    // Off-LAN relay URLs: validated (ws/wss, capped),
                    // persisted sealed, attached live. Empty clears back
                    // to LAN-only. Malformed input is refused with words,
                    // never repaired into dialing the wrong host.
                    if self.sync_session.is_none() || self.sync_trust.is_none() {
                        self.notify("Sync needs the vault available.".to_owned());
                    } else {
                        let raw = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
                        let urls = clean_relay_urls(raw);
                        if !raw.trim().is_empty() && urls.is_empty() {
                            self.notify(
                                "No usable relay URL. Use ws:// or wss:// addresses.".to_owned(),
                            );
                        } else {
                            persist_setting(
                                "relay_urls",
                                serde_json::Value::Array(
                                    urls.iter()
                                        .map(|u| serde_json::Value::String(u.clone()))
                                        .collect(),
                                ),
                            );
                            if let (Some(session), Some(t)) =
                                (self.sync_session.as_ref(), self.sync_trust.as_ref())
                            {
                                session.set_relay(urls.clone(), &t.this.identity);
                            }
                            self.notify(if urls.is_empty() {
                                "Relay cleared. Sync is LAN-only.".to_owned()
                            } else {
                                "Relay saved. Off-LAN sync on next round.".to_owned()
                            });
                            self.push_sync_devices(msg.tab_id);
                        }
                    }
                }
                "sync_relay_clear" => {
                    persist_setting("relay_urls", serde_json::Value::Array(Vec::new()));
                    if let Some(session) = self.sync_session.as_ref() {
                        session.clear_relay();
                    }
                    self.notify("Relay cleared. Sync is LAN-only.".to_owned());
                    self.push_sync_devices(msg.tab_id);
                }
                "sync_pair_approve" => {
                    // Step two: explicit approval binds the key into the
                    // trust store. One ceremony, one path: manual entries
                    // rate-limit as loopback peers (documented in code).
                    let pending = self.pending_pair.take();
                    match (self.sync_trust.as_ref(), pending) {
                        (Some(t), Some(p)) => {
                            let id = p.device_id.clone();
                            let key = p.public_key.clone();
                            // Repair-shaped re-pair (same id, new key) is
                            // allowed by the ceremony but must say so:
                            // silent rotation is where takeovers hide.
                            let known = t.store.all().into_iter().find_map(|d| {
                                (d.device_id == p.device_id && !d.is_revoked)
                                    .then_some((d.device_name, d.public_key))
                            });
                            let repair = pairing_repair_text(
                                known.as_ref().map(|(n, k)| (n.as_str(), k.as_slice())),
                                &p.device_name,
                                &p.public_key,
                            );
                            let loopback: std::net::IpAddr = "127.0.0.1"
                                .parse()
                                .unwrap_or_else(|_| std::net::IpAddr::from([127, 0, 0, 1]));
                            if t.pairing
                                .handle_pairing_request(&p, loopback, id, key, true)
                            {
                                self.notify(repair.unwrap_or_else(|| "Device paired.".to_owned()));
                            } else {
                                self.notify(
                                    "Pairing refused (rate limited, try again).".to_owned(),
                                );
                            }
                            self.push_sync_devices(msg.tab_id);
                        }
                        _ => self.notify("Nothing awaiting approval.".to_owned()),
                    }
                }
                "sync_pair_deny" => {
                    self.pending_pair = None;
                    self.push_sync_devices(msg.tab_id);
                }
                "sync_category" => {
                    // Explicit user action: toggle one sync category. Trusted
                    // devices cannot be toggled (revocation always propagates
                    // mandatory by contract, rendered locked-on).
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let on = v.get("on").and_then(|x| x.as_bool()).unwrap_or(false);
                    let cat = match id {
                        "Bookmarks" => Some(ferrite_sync::SyncCategory::Bookmarks),
                        "History" => Some(ferrite_sync::SyncCategory::History),
                        "Credentials" => Some(ferrite_sync::SyncCategory::Credentials),
                        "Autofill" => Some(ferrite_sync::SyncCategory::Autofill),
                        "Totp" => Some(ferrite_sync::SyncCategory::Totp),
                        "Archive" => Some(ferrite_sync::SyncCategory::Archive),
                        _ => None,
                    };
                    match cat {
                        Some(c) => {
                            if let Ok(mut s) = self.sync_settings.write() {
                                s.set_enabled(c, on);
                                if !is_ephemeral() {
                                    s.save_to_file(
                                        &crate::profiles::data_root()
                                            .join("sync-categories.json")
                                            .to_string_lossy(),
                                    );
                                }
                            }
                            self.push_sync_devices(msg.tab_id);
                        }
                        None => debug!("sync category refused: unknown id"),
                    }
                }
                "sync_open_tab" => {
                    // Open a live tab from another mesh device locally.
                    // Same URL gate as second-instance handover: web and
                    // internal pages only, bounded, single-line.
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if crate::forward::is_forwardable_url(url) {
                        self.open_tab(url.trim().to_owned());
                    } else {
                        debug!("sync open refused: non-forwardable URL");
                    }
                }
                "sync_trust_revoke" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    match self.sync_trust.as_ref() {
                        None => self.notify("Sync needs the vault available.".to_owned()),
                        Some(t) if id == t.this.identity.device_id.to_string() => {
                            self.notify("You can't revoke this device from itself.".to_owned())
                        }
                        Some(_) if id.is_empty() || id.len() > 128 => {
                            debug!("trust revoke refused: bad id");
                        }
                        Some(t) => {
                            if t.store.revoke(id, ferrite_tabs::now_millis()).is_ok() {
                                self.notify("Device revoked.".to_owned());
                            } else {
                                debug!("trust revoke failed");
                            }
                            self.push_sync_devices(msg.tab_id);
                        }
                    }
                }
                "passwords_query" => {
                    self.push_passwords(msg.tab_id);
                }
                "passwords_add" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    let username = v.get("username").and_then(|x| x.as_str()).unwrap_or("");
                    let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
                    match (self.cred_store(), Self::cred_key()) {
                        (Some(store), Some(mut key)) => {
                            match ferrite_credentials::CredentialRecord::new(
                                url, username, password, &key,
                            ) {
                                Ok(rec) => {
                                    if let Err(e) = store.save(&rec) {
                                        debug!(error = %e, "credential save failed");
                                    }
                                }
                                Err(e) => debug!(error = %e, "credential encrypt failed"),
                            }
                            key.zeroize();
                        }
                        _ => debug!("passwords unavailable: vault or keyring"),
                    }
                    self.push_passwords(msg.tab_id);
                }
                "passwords_delete" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    // No Undo staging here by design: restoring the
                    // tombstone would orphan the cascade-deleted TOTP
                    // secrets (see toast.rs scope notes).
                    if let Some(store) = self.cred_store()
                        && let Err(e) = store.delete(id)
                    {
                        debug!(error = %e, "credential delete failed");
                    }
                    self.push_passwords(msg.tab_id);
                }
                "passwords_conflict_resolve" => {
                    // Import-collision choice from ferrite://passwords.
                    // `id` is the queue index plus the row's
                    // origin+username. The row must still match both
                    // (indices shift on every resolve, and the list is
                    // re-pushed each time).
                    // keep=true drops the import. keep=false saves the
                    // imported password first, then tombstones the
                    // incumbents (save-first, so a write failure can
                    // never lose the stored entry). The row must still
                    // match (origin+username): indices shift on every
                    // resolve, so a stale double-click re-pushes
                    // instead of hitting the wrong row.
                    let idx = v.get("id").and_then(|x| x.as_u64()).unwrap_or(u64::MAX) as usize;
                    let keep = v.get("keep").and_then(|x| x.as_bool()).unwrap_or(true);
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    let username = v.get("username").and_then(|x| x.as_str()).unwrap_or("");
                    let matched = self
                        .pw_conflicts
                        .get(idx)
                        .is_some_and(|c| c.origin == url && c.username == username);
                    if matched {
                        let c = self.pw_conflicts.remove(idx);
                        if !keep
                            && let (Some(store), Some(mut key)) =
                                (self.cred_store(), Self::cred_key())
                        {
                            if let Ok(rec) = ferrite_credentials::CredentialRecord::new(
                                &c.origin,
                                &c.username,
                                &c.password,
                                &key,
                            ) {
                                if store.save(&rec).is_ok() {
                                    for old in store.get_for_origin(&c.origin) {
                                        if old.username == c.username
                                            && old.id != rec.id
                                            && let Err(e) = store.delete(&old.id)
                                        {
                                            debug!(error = %e, "conflict incumbent delete failed");
                                        }
                                    }
                                } else {
                                    debug!("conflict import save failed; incumbents untouched");
                                }
                            }
                            key.zeroize();
                        }
                    }
                    self.push_passwords(msg.tab_id);
                }
                "passwords_export" => {
                    // Plaintext CSV to disk (the page already confirmed in
                    // two steps). Built + stashed here, written after the
                    // save pick. The stash clears on write or cancel so
                    // plaintext never lingers beyond the flow.
                    let mut rows: Vec<(String, String, String)> = Vec::new();
                    if let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key()) {
                        for rec in store.all() {
                            if let Some(pw) = rec.decrypt_password(&key) {
                                rows.push((rec.url.clone(), rec.username.clone(), pw));
                            }
                        }
                        key.zeroize();
                    }
                    if rows.is_empty() {
                        self.notify("Nothing to export; no saved passwords".to_owned());
                    } else {
                        let csv = ferrite_credentials::export_csv(&rows);
                        for (_, _, pw) in rows.iter_mut() {
                            pw.zeroize();
                        }
                        self.pending_export = Some(csv);
                        self.launch_pick(
                            crate::filedlg::PickSpec::save("ferrite-passwords.csv"),
                            crate::filedlg::PickTag::PasswordExport,
                            msg.tab_id,
                        );
                    }
                }
                "passwords_reveal" => {
                    // One-shot plaintext to the page (transient, same class
                    // as field injection). Prefer copy. It stays in Rust.
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let mut password = String::new();
                    if let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key()) {
                        if let Some(rec) = store.all().into_iter().find(|r| r.id == id) {
                            password = rec.decrypt_password(&key).unwrap_or_default();
                        }
                        key.zeroize();
                    }
                    let payload = serde_json::json!({"id": id, "password": password});
                    let json = serde_json::to_string(&payload)
                        .unwrap_or_default()
                        .replace("</", "<\\/");
                    if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        && let Some(h) = tab.webview.as_ref()
                        && let Err(e) =
                            h.evaluate_script(&format!("__ferritePasswordReveal({json})"))
                    {
                        debug!(error = %e, "password reveal push failed");
                    }
                }
                "passwords_copy" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if let (Some(store), Some(mut key)) = (self.cred_store(), Self::cred_key()) {
                        if let Some(rec) = store.all().into_iter().find(|r| r.id == id) {
                            if let Some(mut pw) = rec.decrypt_password(&key) {
                                self.copy_sensitive_with_toast(&pw, "Password");
                                pw.zeroize();
                            }
                        }
                        key.zeroize();
                    }
                }
                "passwords_copy_user" => {
                    // Usernames are identifiers, not secrets: plain copy,
                    // no privacy flags, no auto-clear timer to fight.
                    let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
                    self.copy_with_toast(text, "Username");
                }
                "welcome_detect" => {
                    self.push_welcome_detect(msg.tab_id);
                }
                "welcome_import" => {
                    // Detected kinds import directly (one click, no picker);
                    // "bookmarks" (no suffix) is the manual bookmark-file
                    // picker (HTML or JSON); "*-bookmarks" kinds are
                    // detected Chromium JSON. History importers sniff the
                    // SQLite schema, so Chrome and Firefox files share the
                    // "history" path (reads go through a temp copy, never
                    // the live file). Pickers run on workers (never block
                    // the loop). The work lands in the tick drain with the
                    // picked path.
                    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                    let tab_id = msg.tab_id;
                    if kind == "bookmarks" {
                        self.launch_pick(
                            crate::filedlg::PickSpec::open(vec![(
                                "Bookmark files",
                                &["html", "htm", "json"],
                            )]),
                            crate::filedlg::PickTag::WelcomeBookmarks,
                            tab_id,
                        );
                    } else if let Some(src) = detect_browsers().into_iter().find(|s| s.id == kind) {
                        // Registry/store targets bypass the vault import
                        // core (engines live in settings, addresses in the
                        // autofill store).
                        if kind.ends_with("-engines") {
                            self.import_welcome_engines(tab_id, &src);
                        } else if kind.ends_with("-addresses") {
                            self.import_welcome_addresses(tab_id, &src);
                        } else {
                            self.import_welcome_detected(tab_id, &src, kind);
                        }
                    } else {
                        info!(kind, n = 0, "welcome import");
                        self.push_welcome_result(tab_id, kind, 0, 0, 0, None);
                    }
                }
                "welcome_passwords" => {
                    // CSV export from the old browser (Chrome/Firefox both
                    // export this shape). Keychains are never read directly.
                    self.launch_pick(
                        crate::filedlg::PickSpec::open(vec![("Passwords CSV", &["csv"])]),
                        crate::filedlg::PickTag::WelcomePasswords,
                        msg.tab_id,
                    );
                }
                "welcome_done" => {
                    persist_setting("onboarding_completed", serde_json::Value::Bool(true));
                    if let Some(tab) = self.app.tabs.active_tab_mut() {
                        let _ = tab.load_url("ferrite://newtab".to_owned());
                    }
                    self.layout_tabs();
                }
                "welcome_default_browser" => {
                    // Windows 10+: no programmatic default possible. Deep
                    // link to Default Apps and say so (other platforms: the
                    // page shows its own honest note instead of posting).
                    #[cfg(windows)]
                    if let Err(e) = std::process::Command::new("explorer")
                        .arg("ms-settings:defaultapps")
                        .spawn()
                    {
                        debug!(error = %e, "default-apps deep link failed");
                    }
                }
                "about_copy" => {
                    // Mirrors the baked page values (same calls, kept in
                    // the same order so drift is obvious in review). Boot
                    // timings ride along: durations only, no user data.
                    // The toast is the confirmation. The page no longer
                    // sets its own optimistic label.
                    let engine = wry::webview_version().unwrap_or_else(|_| "unknown".to_owned());
                    let mut text = format!(
                        "Ferrite {}\nWeb engine {}\nPlatform {}",
                        env!("CARGO_PKG_VERSION"),
                        engine,
                        std::env::consts::OS
                    );
                    if let Some(boot) = read_boot_last_in(&crate::profiles::data_root()) {
                        text.push_str(&format!("\nLast boot {boot}"));
                    }
                    self.copy_with_toast(&text, "Diagnostics");
                }
                "update_check" => {
                    self.spawn_update_check(true);
                }
                "update_restart" => {
                    // Restart into a staged update: only from Ready (a
                    // stray post must not bounce the browser), session
                    // saved first so the new boot restores silently. The
                    // quit path removes the crash marker and spawns the
                    // current (newly swapped) binary.
                    if matches!(self.update_phase, crate::update::UpdatePhase::Ready { .. })
                        && !self.guest_mode
                    {
                        if self.boot_block.is_none() {
                            let _ = self.save_session_file();
                        }
                        self.relaunch_requested = true;
                    } else {
                        debug!("update restart ignored: nothing staged");
                    }
                }
                "update_query" => {
                    self.push_update_about();
                }
                "autofill_query" => {
                    self.push_autofill(msg.tab_id);
                }
                "autofill_add_address" => {
                    let fields: ferrite_credentials::AddressFields = serde_json::from_value(
                        v.get("fields").cloned().unwrap_or(serde_json::Value::Null),
                    )
                    .unwrap_or_default();
                    match (self.autofill_store(), Self::cred_key()) {
                        (Some(store), Some(mut key)) => {
                            match store.save_address(&fields, &key) {
                                Ok(_) => self.push_autofill_tabs(),
                                Err(e) => {
                                    debug!(error = %e, "address save failed");
                                    self.push_autofill_error(
                                        msg.tab_id,
                                        &format!("Couldn't save: {e}"),
                                    );
                                }
                            }
                            key.zeroize();
                        }
                        _ => {
                            debug!("autofill unavailable: vault or keyring");
                            self.push_autofill_error(msg.tab_id, "Vault unavailable");
                        }
                    }
                }
                "autofill_add_card" => {
                    // Unknown object keys (a smuggled `cvv`) are dropped by
                    // the struct shape. There is nowhere to store one.
                    let mut fields: ferrite_credentials::CardFields = serde_json::from_value(
                        v.get("fields").cloned().unwrap_or(serde_json::Value::Null),
                    )
                    .unwrap_or_default();
                    match (self.autofill_store(), Self::cred_key()) {
                        (Some(store), Some(mut key)) => {
                            match store.save_card(&mut fields, &key) {
                                Ok(_) => self.push_autofill_tabs(),
                                Err(e) => {
                                    debug!(error = %e, "card save failed");
                                    self.push_autofill_error(
                                        msg.tab_id,
                                        &format!("Couldn't save: {e}"),
                                    );
                                }
                            }
                            key.zeroize();
                        }
                        _ => {
                            debug!("autofill unavailable: vault or keyring");
                            self.push_autofill_error(msg.tab_id, "Vault unavailable");
                        }
                    }
                }
                "autofill_delete" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(store) = self.autofill_store()
                        && let Err(e) = store.delete(id)
                    {
                        debug!(error = %e, "autofill delete failed");
                    }
                    self.push_autofill_tabs();
                }
                "autofill_copy_card" => {
                    // PAN by id: the page only sends the record id, the
                    // full number is decrypted here and written via the
                    // sensitive path (private formats + auto-clear). The
                    // manager page itself only ever sees network + last4.
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    match (self.autofill_store(), Self::cred_key()) {
                        (Some(store), Some(mut key)) => {
                            let mut done = false;
                            if let Some(rec) = store
                                .list(ferrite_credentials::AutofillKind::Card, &key)
                                .into_iter()
                                .find(|r| r.id == id)
                                && let Ok(mut fields) =
                                    serde_json::from_str::<ferrite_credentials::CardFields>(
                                        &rec.fields_json,
                                    )
                            {
                                self.copy_sensitive_with_toast(&fields.pan, "Card number");
                                fields.pan.zeroize();
                                done = true;
                            }
                            key.zeroize();
                            if !done {
                                self.notify("Couldn't copy card number".to_owned());
                            }
                        }
                        _ => {
                            debug!("autofill unavailable: vault or keyring");
                            self.notify("Vault unavailable".to_owned());
                        }
                    }
                }
                "taskman_query" => {
                    self.sys
                        .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                    self.push_taskmanager(msg.tab_id);
                }
                "taskman_end_task" => {
                    // In-architecture End task: discard frees the renderer
                    // (pinned/audio tabs are exempt by discard itself).
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if let Ok(id) = id.parse::<Uuid>() {
                        let idx = self.app.tabs.iter().position(|t| t.id == id);
                        if let Some(idx) = idx
                            && let Err(e) = self.app.tabs.discard_tab(idx)
                        {
                            debug!(error = %e, "end task failed");
                        }
                        // A discarded renderer takes pending dialogs with it.
                        self.clear_tab_dialogs(id);
                        if self.find_tab == Some(id) {
                            self.find_reset();
                        }
                    }
                    self.layout_tabs();
                }
                "taskman_close" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if let Ok(id) = id.parse::<Uuid>() {
                        self.close_tab_recorded(id);
                    }
                }
                "taskman_kill" => {
                    // PID kill (no confirm dialog. The click is the
                    // confirmation, consistent with tab close). Never our
                    // own process.
                    let pid = v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0);
                    let me = std::process::id() as u64;
                    if pid != 0 && pid != me {
                        let killed = self
                            .sys
                            .process(sysinfo::Pid::from(pid as usize))
                            .map(|p| p.kill())
                            .unwrap_or(false);
                        debug!(pid, killed, "taskmanager kill");
                    }
                }
                "certs_query" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    match crate::cert::https_host(url) {
                        Some((host, port)) => {
                            if self.cert_pending.insert(msg.tab_id) {
                                let now = ferrite_tabs::now_millis();
                                let exceptions = self
                                    .vault
                                    .as_ref()
                                    .and_then(|vault| vault.list_cert_exceptions().ok())
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|r| crate::cert::ExceptionEntry {
                                        host: r.host,
                                        sha256: r.sha256,
                                        expires_at: r.expires_at,
                                    })
                                    .collect();
                                crate::cert::spawn_cert_fetch(
                                    host,
                                    port,
                                    msg.tab_id,
                                    self.cert_tx.clone(),
                                    exceptions,
                                    now,
                                );
                            }
                        }
                        None => {
                            // Not HTTPS: immediate verdict, no fetch, no leak.
                            self.push_cert(
                                msg.tab_id,
                                &crate::cert::CertReport {
                                    host: url.to_owned(),
                                    verified: false,
                                    verify_error: "not encrypted".to_owned(),
                                    leaf: None,
                                    chain: Vec::new(),
                                    exception_trusted: false,
                                    exception_expires_at: None,
                                },
                            );
                        }
                    }
                }
                "certs_trust" => {
                    // Trust-this-cert: host + fingerprint must match the
                    // report Ferrite just fetched for this tab (never a
                    // pasted value), and the Vault must persist it.
                    let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("");
                    let fp = v.get("sha256").and_then(|x| x.as_str()).unwrap_or("");
                    let known = self
                        .cert_last
                        .get(&msg.tab_id)
                        .is_some_and(|(h, f)| h == host && f == fp && !fp.is_empty());
                    match (known, self.vault.as_ref()) {
                        (true, Some(vault)) => {
                            let now = ferrite_tabs::now_millis();
                            match vault.save_cert_exception(
                                host,
                                fp,
                                now.saturating_add(crate::cert::EXCEPTION_TTL_MS),
                                now,
                            ) {
                                Ok(()) => self.refresh_cert_report(msg.tab_id, host),
                                Err(e) => debug!(error = %e, "cert exception save failed"),
                            }
                        }
                        (false, _) => debug!("cert trust refused: no matching fetched report"),
                        (true, None) => debug!("cert trust refused: vault unavailable"),
                    }
                }
                "certs_revoke" => {
                    let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("");
                    let fp = v.get("sha256").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(vault) = self.vault.as_ref() {
                        match vault.remove_cert_exception(host, fp) {
                            Ok(true) => self.refresh_cert_report(msg.tab_id, host),
                            Ok(false) => {}
                            Err(e) => debug!(error = %e, "cert exception revoke failed"),
                        }
                    }
                }
                "cosmetic_query" => {
                    // Content-script answer path: page asks at document_start,
                    // host answers with this URL's hide-selectors evaluated
                    // into the same tab. Page-supplied URL, so scheme-gated
                    // before any lookup. Empty results push nothing.
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    let http = {
                        let l = url.trim().to_lowercase();
                        l.starts_with("http://") || l.starts_with("https://")
                    };
                    if http {
                        let sels = self.privacy.cosmetic_selectors(url);
                        if !sels.is_empty() {
                            let refs: Vec<&str> = sels.iter().map(|s| s.as_str()).collect();
                            let css = ferrite_privacy::js::cosmetic_css(&refs);
                            if !css.is_empty() {
                                let js = ferrite_privacy::js::cosmetic_js(&css);
                                if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                                    && let Some(h) = tab.webview.as_ref()
                                    && let Err(e) = h.evaluate_script(&js)
                                {
                                    debug!(error = %e, "cosmetic push failed");
                                }
                            }
                        }
                    }
                }
                "form_risk" => {
                    // Submit-time report: password form posting cross-host.
                    // Validated + judged engine-side (registrable comparison,
                    // TTL). The reporter only observes the DOM.
                    let page = v.get("page").and_then(|x| x.as_str()).unwrap_or("");
                    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
                    if !page.is_empty() && !action.is_empty() {
                        self.privacy
                            .note_form_risk(page, action, ferrite_tabs::now_millis());
                    }
                }
                "blocked_query" => {
                    let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("");
                    self.push_blocked_detail(msg.tab_id, host);
                }
                "threat_trust" => {
                    // Fast local override: host must equal this tab's own
                    // blocked target (never a pasted value), and the Vault
                    // must persist it. Friction is the interstitial itself.
                    let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("");
                    let target = self.blocked_target_host(msg.tab_id);
                    if target.as_deref() != Some(host) || host.is_empty() {
                        debug!("threat trust refused: host mismatch");
                    } else if let Some(vault) = self.vault.as_ref() {
                        match vault.save_threat_exception(host, ferrite_tabs::now_millis()) {
                            Ok(()) => {
                                if let Ok(hosts) = vault.list_threat_exceptions() {
                                    self.privacy.set_threat_exceptions(hosts);
                                }
                                self.push_blocked_detail(msg.tab_id, host);
                            }
                            Err(e) => debug!(error = %e, "threat override save failed"),
                        }
                    } else {
                        debug!("threat trust refused: vault unavailable");
                    }
                }
                "threat_report" => {
                    // List-quality feedback: recorded locally with the
                    // trigger, never transmitted. Collection without
                    // exfiltration. Export is a future consumer's job.
                    let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("");
                    let why = v.get("why").and_then(|x| x.as_str()).unwrap_or("");
                    let target = self.blocked_target_host(msg.tab_id);
                    if target.as_deref() != Some(host) || host.is_empty() {
                        debug!("threat report refused: host mismatch");
                    } else if let Some(vault) = self.vault.as_ref() {
                        match vault.save_threat_report(host, why, ferrite_tabs::now_millis()) {
                            Ok(()) => self.push_blocked_detail(msg.tab_id, host),
                            Err(e) => debug!(error = %e, "threat report save failed"),
                        }
                    }
                }
                "viewsource_query" => {
                    // Target comes from the tab's own URL (?u=), never page
                    // params. Own pages serve verbatim consts. Http(s)
                    // re-fetches with the tab's cookies on a thread.
                    let tab_url = self
                        .app
                        .tabs
                        .get_tab(msg.tab_id)
                        .map(|t| t.url.clone())
                        .unwrap_or_default();
                    let query = tab_url
                        .strip_prefix("ferrite://viewsource")
                        .unwrap_or("")
                        .trim_start_matches('?');
                    let target =
                        ferrite_tabs::protocol::decode_query_param(query, "u").unwrap_or_default();
                    if let Some(src) = Self::internal_source(&target) {
                        self.push_viewsource(
                            msg.tab_id,
                            &crate::viewsource::VsReport {
                                url: target,
                                content_type: "text/html".to_owned(),
                                truncated: false,
                                escaped: crate::viewsource::escape_html(src),
                                error: String::new(),
                            },
                        );
                    } else if target.starts_with("http://") || target.starts_with("https://") {
                        if self.vs_pending.insert(msg.tab_id) {
                            let cookies = self
                                .app
                                .tabs
                                .get_tab(msg.tab_id)
                                .and_then(|t| t.webview.as_ref())
                                .and_then(|h| h.cookies_for_url(&target).ok())
                                .unwrap_or_default();
                            crate::viewsource::spawn_viewsource(
                                target,
                                cookies,
                                msg.tab_id,
                                self.vs_tx.clone(),
                            );
                        }
                    } else {
                        self.push_viewsource(
                            msg.tab_id,
                            &crate::viewsource::VsReport {
                                url: target,
                                content_type: String::new(),
                                truncated: false,
                                escaped: String::new(),
                                error: "nothing viewable here".to_owned(),
                            },
                        );
                    }
                }
                "viewsource_wrap" => {
                    let value = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                    self.viewsource_wrap = value;
                    persist_setting("viewsource_wrap", serde_json::Value::Bool(value));
                }
                "passwords_generate" => {
                    let len = v
                        .get("len")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(20)
                        .clamp(12, 64) as usize;
                    // The focus dropdown follows this control (it has no
                    // length UI of its own).
                    self.gen_length = len;
                    let pw = ferrite_credentials::generate_password(len, true, true, true, true);
                    let payload = serde_json::json!({"password": pw.value});
                    let json = serde_json::to_string(&payload)
                        .unwrap_or_default()
                        .replace("</", "<\\/");
                    if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        && let Some(h) = tab.webview.as_ref()
                        && let Err(e) =
                            h.evaluate_script(&format!("__ferritePasswordGenerated({json})"))
                    {
                        debug!(error = %e, "password generate push failed");
                    }
                }
                "select_engine" => {
                    // Toolbar selector always sets the REGULAR default, even
                    // in a private tab (explicit beats contextual, same as
                    // Chrome: settings are global). The private default
                    // changes only via Settings. The toolbar DISPLAY still
                    // shows the effective engine, so what you see is what
                    // Enter searches.
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if let Err(e) = self.app.omnibox.registry.set_current(id) {
                        debug!(error = %e, engine = %id, "unknown engine ignored");
                    } else {
                        persist_omnibox_engine(id);
                    }
                }
                "select_private_engine" => {
                    // Empty id clears back to follow-the-regular.
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if id.is_empty() {
                        self.app.omnibox.registry.clear_private();
                        clear_setting("omnibox_engine_private");
                    } else if let Err(e) = self.app.omnibox.registry.set_private(id) {
                        debug!(error = %e, engine = %id, "unknown private engine ignored");
                    } else {
                        persist_setting(
                            "omnibox_engine_private",
                            serde_json::Value::String(id.to_owned()),
                        );
                    }
                    self.push_settings(msg.tab_id);
                }
                "select_pack" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    match ThemePack::from_name(id) {
                        Some(pack) => self.set_theme_pack(pack),
                        None => debug!(pack = %id, "unknown pack ignored"),
                    }
                }
                "select_home" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if valid_home_url(url) {
                        self.home_url = url.trim().to_owned();
                        persist_setting(
                            "home_url",
                            serde_json::Value::String(self.home_url.clone()),
                        );
                        info!(url = %self.home_url, "home destination set");
                    } else {
                        debug!("home destination rejected");
                    }
                }
                "custom_add" => {
                    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    let template = v.get("template").and_then(|x| x.as_str()).unwrap_or("");
                    match self.app.omnibox.registry.add_custom(name, template) {
                        Ok(id) => {
                            info!(engine = %id, "custom engine added");
                            self.persist_customs();
                            self.push_settings(msg.tab_id);
                        }
                        Err(e) => debug!(error = %e, "custom engine rejected"),
                    }
                }
                "custom_delete" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if self.app.omnibox.registry.remove_custom(id) {
                        self.persist_customs();
                        persist_omnibox_engine(self.app.omnibox.registry.current_id());
                        // The deleted custom may have been the private
                        // default (registry repairs to follow-regular).
                        match self.app.omnibox.registry.private_id() {
                            Some(pid) => persist_setting(
                                "omnibox_engine_private",
                                serde_json::Value::String(pid.to_owned()),
                            ),
                            None => clear_setting("omnibox_engine_private"),
                        }
                        self.push_settings(msg.tab_id);
                    }
                }
                "export_settings" => {
                    self.launch_pick(
                        crate::filedlg::PickSpec::save("ferrite-settings.json"),
                        crate::filedlg::PickTag::ExportSettings,
                        msg.tab_id,
                    );
                }
                "import_settings" => {
                    self.launch_pick(
                        crate::filedlg::PickSpec::open(vec![("JSON", &["json"])]),
                        crate::filedlg::PickTag::ImportSettings,
                        msg.tab_id,
                    );
                }
                "reset_settings" => {
                    crate::settings_file::delete_settings_files();
                    self.reset_to_defaults();
                    self.push_chrome(true);
                    self.push_settings(msg.tab_id);
                    info!("settings reset to defaults");
                }
                "import" => {
                    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                    if kind == "export" {
                        self.launch_pick(
                            crate::filedlg::PickSpec::save("ferrite-bookmarks.html"),
                            crate::filedlg::PickTag::BookmarkExport,
                            msg.tab_id,
                        );
                    } else if kind == "export_history" {
                        self.launch_pick(
                            crate::filedlg::PickSpec::save("ferrite-history.csv"),
                            crate::filedlg::PickTag::HistoryExport,
                            msg.tab_id,
                        );
                    } else {
                        self.launch_pick(
                            crate::filedlg::PickSpec::open(vec![(
                                "Bookmark files",
                                if kind == "bookmarks" {
                                    // Chromium profile files are
                                    // extensionless JSON; Firefox backups
                                    // are .json. Content sniffing sorts
                                    // them out after the pick.
                                    &["html", "htm", "json"][..]
                                } else {
                                    &["sqlite", "db"][..]
                                },
                            )]),
                            crate::filedlg::PickTag::BookmarkKind {
                                kind: kind.to_owned(),
                            },
                            msg.tab_id,
                        );
                    }
                }
                "export_all" => {
                    // One portable archive (bookmarks HTML + passwords CSV
                    // + settings JSON). Plaintext by nature. the page
                    // confirms the cost before posting.
                    match self.build_portable_tar() {
                        Ok(bytes) => {
                            self.pending_blob = Some(bytes);
                            self.launch_pick(
                                crate::filedlg::PickSpec::save("ferrite-export.tar"),
                                crate::filedlg::PickTag::ExportAll,
                                msg.tab_id,
                            );
                        }
                        Err(e) => self.notify(format!("Couldn't build export: {e}")),
                    }
                }
                "backup_export" => {
                    // Encrypted native backup for a new device. Never
                    // plaintext on disk. The passphrase never leaves this
                    // handler except into the KDF (zeroized after).
                    let mut pp = v
                        .get("passphrase")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let out = self.build_sealed_backup(&pp);
                    pp.zeroize();
                    match out {
                        Ok(bytes) => {
                            self.pending_blob = Some(bytes);
                            self.launch_pick(
                                crate::filedlg::PickSpec::save("ferrite-backup.enc"),
                                crate::filedlg::PickTag::BackupExport,
                                msg.tab_id,
                            );
                        }
                        Err(e) => self.notify(format!("Couldn't build backup: {e}")),
                    }
                }
                "backup_import" => {
                    self.launch_pick(
                        crate::filedlg::PickSpec::open(vec![("Backup", &["enc"])]),
                        crate::filedlg::PickTag::BackupImport,
                        msg.tab_id,
                    );
                }
                "backup_restore" => {
                    let mut pp = v
                        .get("passphrase")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let path = self.pending_backup_path.take();
                    match path {
                        Some(path) => self.restore_backup_file(&path, &pp, msg.tab_id),
                        None => self.notify("Pick a backup file first.".to_owned()),
                    }
                    pp.zeroize();
                }
                "palette" => {
                    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
                    match action {
                        // Full snapshot on open: the page filters and ranks
                        // client-side (sync fuzzy, no per-keystroke IPC).
                        "open" => self.push_palette(),
                        "run" => {
                            let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                            self.run_palette_row(kind, id);
                            self.overlay_open = false;
                            Self::close_web_tabsearch(&mut self.overlay, &mut self.app.tabs);
                        }
                        _ => {
                            self.overlay_open = false;
                            Self::close_web_tabsearch(&mut self.overlay, &mut self.app.tabs);
                        }
                    }
                }
                // First-party page messages. `navigate`/`search` drive the
                // active tab (this also powers the newtab page search box);
                // list pages query Rust for rows and render via textContent.
                "navigate" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if !url.is_empty() {
                        // Load first (clears restore-suppression), then
                        // record. Same order as chrome Navigate.
                        self.find_reset();
                        self.hide_ctx_menu();
                        // PDF documents never load in the engine: remote URLs fetch to
                        // RAM and open in the isolated viewer, local files read straight
                        // in. The engine's built-in viewer is outside Ferrite guarantees
                        // (no sandboxing, no password hygiene, no permission handling).
                        match ferrite_tabs::pdf::classify_pdf_url(url) {
                            ferrite_tabs::pdf::PdfNav::Viewer => {}
                            ferrite_tabs::pdf::PdfNav::Remote => {
                                if let Some(tab) = self.app.tabs.active_tab() {
                                    let tab_id = tab.id;
                                    self.notify("Fetching PDF...".to_owned());
                                    crate::pdf::spawn_pdf_fetch(
                                        url.to_owned(),
                                        crate::pdf::display_title(url),
                                        tab_id,
                                        self.pdf_fetch_tx.clone(),
                                    );
                                }
                                return;
                            }
                            ferrite_tabs::pdf::PdfNav::Local => {
                                if let Some(tab) = self.app.tabs.active_tab() {
                                    let tab_id = tab.id;
                                    self.pdf_open_local(tab_id, url);
                                }
                                return;
                            }
                            ferrite_tabs::pdf::PdfNav::No => {}
                        }
                        if let Some(tab) = self.app.tabs.active_tab_mut() {
                            if let Err(e) = tab.load_url(url.to_owned()) {
                                debug!(error = %e, "page navigation failed");
                            } else {
                                let id = tab.id;
                                self.clear_tab_dialogs(id);
                            }
                        } else {
                            self.open_tab(url.to_owned());
                        }
                        self.record_visit(url, "link", None);
                        self.layout_tabs();
                    }
                }
                "search" => {
                    let q = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    if !q.trim().is_empty() {
                        self.commit_omnibox(q.to_owned(), None, None);
                    }
                }
                "newtab_data" => {
                    self.push_newtab_data(msg.tab_id);
                }
                "ntp_pin" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.ntp_pin(url, title)
                    {
                        debug!(error = %e, "tile pin failed");
                    }
                    self.push_newtab_data(msg.tab_id);
                }
                "ntp_unpin" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.ntp_unpin(url)
                    {
                        debug!(error = %e, "tile unpin failed");
                    }
                    self.push_newtab_data(msg.tab_id);
                }
                "ntp_remove" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.ntp_remove(url)
                    {
                        debug!(error = %e, "tile remove failed");
                    }
                    self.push_newtab_data(msg.tab_id);
                }
                "history_query" => {
                    let q = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    self.history_queries.insert(msg.tab_id, q.to_owned());
                    let items = self.history_json(q);
                    self.push_list(msg.tab_id, "__ferriteHistory", &items);
                }
                "history_commit" => {
                    // Enter in the filter box: the query becomes a real
                    // navigation (back button returns to the unfiltered list).
                    let q: String = v
                        .get("query")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                    let url = if q.trim().is_empty() {
                        "ferrite://history".to_owned()
                    } else {
                        format!("ferrite://history?q={}", encode_query(&q))
                    };
                    if let Some(tab) = self.app.tabs.active_tab_mut() {
                        if let Err(e) = tab.load_url(url) {
                            debug!(error = %e, "history commit failed");
                        } else {
                            let id = tab.id;
                            self.clear_tab_dialogs(id);
                        }
                    }
                    self.layout_tabs();
                }
                "history_delete" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let snap = self
                        .vault
                        .as_ref()
                        .and_then(|vault| vault.history_row_by_id(id));
                    if let Some(vault) = self.vault.as_ref() {
                        if let Err(e) = vault.delete_history_id(id) {
                            debug!(error = %e, "history delete failed");
                        }
                    }
                    let q = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    self.history_queries.insert(msg.tab_id, q.to_owned());
                    let items = self.history_json(q);
                    self.push_list(msg.tab_id, "__ferriteHistory", &items);
                    if let Some(row) = snap {
                        self.push_undo(
                            "History entry deleted".to_owned(),
                            crate::toast::UndoPayload::History(vec![row]),
                        );
                    }
                }
                "history_delete_site" => {
                    let host = v.get("host").and_then(|x| x.as_str()).unwrap_or("");
                    let snap = self
                        .vault
                        .as_ref()
                        .map(|vault| {
                            vault.history_rows_for_site(host, crate::toast::UNDO_SNAPSHOT_CAP)
                        })
                        .unwrap_or_default();
                    let n = if let Some(vault) = self.vault.as_ref() {
                        match vault.delete_history_site(host) {
                            Ok(n) => {
                                info!(host, n, "site history deleted");
                                n
                            }
                            Err(e) => {
                                debug!(error = %e, "site delete failed");
                                0
                            }
                        }
                    } else {
                        0
                    };
                    let q = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    self.history_queries.insert(msg.tab_id, q.to_owned());
                    let items = self.history_json(q);
                    self.push_list(msg.tab_id, "__ferriteHistory", &items);
                    // Undo only when the snapshot provably covers the
                    // delete (it merges the same cache + database rows the
                    // delete sweeps. Overflow means it may be partial, so
                    // the toast confirms without Undo rather than lying).
                    // Restoring extra still-present rows is a verbatim
                    // no-op (INSERT OR REPLACE with identical values).
                    if snap.len() <= crate::toast::UNDO_SNAPSHOT_CAP {
                        if !snap.is_empty() {
                            let text = format!("{} visits from {host} deleted", snap.len());
                            self.push_undo(text, crate::toast::UndoPayload::History(snap));
                        }
                    } else {
                        info!(host, n, "site delete too large for undo staging");
                        self.push_delete_done(format!("Visits from {host} deleted"));
                    }
                }
                "cookies_query" => {
                    // Per-site viewer: what's stored for this origin. Page JS
                    // never touches the jar directly. It only renders what
                    // Rust pushes (same IPC discipline as every native call).
                    let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
                    let (host, rows) = self.cookie_rows(origin);
                    let payload = serde_json::json!({"origin": host, "cookies": rows});
                    let js = serde_json::to_string(&payload).unwrap_or_default();
                    if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        && let Some(handle) = tab.webview.as_ref()
                        && let Err(e) = handle.evaluate_script(&format!("__ferriteCookies({js})"))
                    {
                        debug!(error = %e, "cookies push failed");
                    }
                }
                "cookies_clear_site" => {
                    let origin = v.get("origin").and_then(|x| x.as_str()).unwrap_or("");
                    let (host, n) = self.cookies_clear_site(origin);
                    info!(%host, n, "site cookies cleared");
                    let payload = serde_json::json!({"origin": host, "cleared": n});
                    let js = serde_json::to_string(&payload).unwrap_or_default();
                    if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        && let Some(handle) = tab.webview.as_ref()
                        && let Err(e) =
                            handle.evaluate_script(&format!("__ferriteCookiesCleared({js})"))
                    {
                        debug!(error = %e, "cookies-cleared push failed");
                    }
                    // Refresh the viewer in place when it's still open.
                    let (_, rows) = self.cookie_rows(origin);
                    let payload = serde_json::json!({"origin": host, "cookies": rows});
                    let js = serde_json::to_string(&payload).unwrap_or_default();
                    if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        && let Some(handle) = tab.webview.as_ref()
                        && let Err(e) = handle.evaluate_script(&format!("__ferriteCookies({js})"))
                    {
                        debug!(error = %e, "cookies refresh push failed");
                    }
                }
                "clear_data" => {
                    let range = v.get("range").and_then(|x| x.as_str()).unwrap_or("day");
                    let hours: i64 = match range {
                        "hour" => 1,
                        "week" => 24 * 7,
                        "all" => 24 * 365 * 100,
                        _ => 24,
                    };
                    let cutoff = now_millis() - hours * 3_600_000;
                    let want_history = v.get("history").and_then(|x| x.as_bool()).unwrap_or(true);
                    let want_downloads =
                        v.get("downloads").and_then(|x| x.as_bool()).unwrap_or(true);
                    let want_cache = v.get("cache").and_then(|x| x.as_bool()).unwrap_or(false);
                    let want_cookies = v.get("cookies").and_then(|x| x.as_bool()).unwrap_or(false);
                    let (mut h, mut d) = (0u64, 0u64);
                    // Snapshot first for undo (capped. Overflow means the
                    // toast confirms without Undo, never a partial restore).
                    let (snap_h, snap_d) = match self.vault.as_ref() {
                        Some(vault) => (
                            vault.history_rows_since(cutoff, crate::toast::UNDO_SNAPSHOT_CAP),
                            vault.download_rows_since(cutoff, crate::toast::UNDO_SNAPSHOT_CAP),
                        ),
                        None => (Vec::new(), Vec::new()),
                    };
                    if let Some(vault) = self.vault.as_ref() {
                        if want_history {
                            h = vault.delete_history_since(cutoff).unwrap_or(0);
                            self.app.autocomplete.clear();
                        }
                        if want_downloads {
                            d = vault.delete_downloads_since(cutoff).unwrap_or(0);
                        }
                    }
                    // Engine state: the engine's OWN clearing call on every
                    // live webview (Ferrite doesn't own the on-disk format).
                    // Runs for history clears too, not just cache clears:
                    // wry exposes one blanket `clear_all_browsing_data`
                    // (no per-kind flags like WebView2's BrowsingDataKinds),
                    // and the engine retains history-adjacent state Ferrite
                    // can never see (back/forward stacks, HSTS, autofill
                    // cache). A "clear my history" that skips this would lie.
                    // Discarded tabs have no webview and no memory cache. The
                    // shared disk-cache clear covers their share. Irreversible
                    //. No undo is offered when this runs (said in the text).
                    let mut c = 0u64;
                    if want_cache || want_history {
                        for t in self.app.tabs.iter() {
                            if let Some(handle) = t.webview.as_ref() {
                                match handle.clear_browsing_data() {
                                    Ok(()) => c += 1,
                                    Err(e) => {
                                        debug!(error = %e, "engine cache clear failed")
                                    }
                                }
                            }
                        }
                    }
                    info!(range, h, d, c, "browsing data cleared");
                    // Profile-wide cookie wipe (any live webview sees the
                    // same jar). All-time regardless of range: jar entries
                    // carry expiry, not creation time. Irreversible like the
                    // cache clear. Suppresses Undo below.
                    let mut k = 0u64;
                    if want_cookies {
                        k = self.cookies_clear_all();
                    }
                    info!(range, k, "cookies cleared");
                    let js = serde_json::to_string(&format!(
                        "Cleared {h} history entries, {d} download records, engine data on {c} tabs and {k} cookies ({range}). Engine and cookie clears can't be undone."
                    ))
                    .unwrap_or_default();
                    if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        && let Some(handle) = tab.webview.as_ref()
                        && let Err(e) = handle.evaluate_script(&format!("__ferriteCleared({js})"))
                    {
                        debug!(error = %e, "clear confirm push failed");
                    }
                    // The page already shows the inline result above. The
                    // toast adds Undo only when both snapshots are proven
                    // complete (warm cache rows included, overflow ruled
                    // out) AND no irreversible clear ran (engine cache or
                    // cookies. Undoing history rows after wiping those
                    // would lie about what "undo" restores). Oversized
                    // clears and cache/cookie clears keep the inline text.
                    if h + d > 0
                        && c == 0
                        && k == 0
                        && snap_h.len() <= crate::toast::UNDO_SNAPSHOT_CAP
                        && snap_d.len() <= crate::toast::UNDO_SNAPSHOT_CAP
                    {
                        self.push_undo(
                            format!("Cleared {h} history entries and {d} download records"),
                            crate::toast::UndoPayload::ClearData {
                                history: snap_h,
                                downloads: snap_d,
                            },
                        );
                    }
                }
                "downloads_query" => {
                    let q = v
                        .get("query")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let status = v
                        .get("status")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    self.dl_queries
                        .insert(msg.tab_id, (q.clone(), status.clone()));
                    let items = self.downloads_json(&q, &status);
                    self.push_list(msg.tab_id, "__ferriteDownloads", &items);
                }
                "downloads_commit" => {
                    // Enter in the filter box: real navigation, like history.
                    let q: String = v
                        .get("query")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                    let status: String = v
                        .get("status")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(20)
                        .collect();
                    let mut url = "ferrite://downloads".to_owned();
                    let mut sep = '?';
                    for (k, val) in [("q", q.trim()), ("status", status.trim())] {
                        if !val.is_empty() {
                            url.push(sep);
                            url.push_str(k);
                            url.push('=');
                            url.push_str(&encode_query(val));
                            sep = '&';
                        }
                    }
                    if let Some(tab) = self.app.tabs.active_tab_mut() {
                        if let Err(e) = tab.load_url(url) {
                            debug!(error = %e, "downloads commit failed");
                        }
                    }
                    self.layout_tabs();
                }
                "dl_retry" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if !url.is_empty() {
                        self.open_tab(url.to_owned());
                    }
                }
                "dl_remove" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let snap = self
                        .vault
                        .as_ref()
                        .and_then(|vault| vault.download_by_id(id));
                    if let Some(vault) = self.vault.as_ref() {
                        if let Err(e) = vault.remove_download(id) {
                            debug!(error = %e, "download remove failed");
                        }
                    }
                    // Refresh through the shared builder (keeps live rows;
                    // the old inline rebuild dropped in-progress ones).
                    let (q, status) = self
                        .dl_queries
                        .get(&msg.tab_id)
                        .cloned()
                        .unwrap_or_default();
                    let items = self.downloads_json(&q, &status);
                    self.push_list(msg.tab_id, "__ferriteDownloads", &items);
                    if let Some(row) = snap {
                        self.push_undo(
                            "Removed from downloads".to_owned(),
                            crate::toast::UndoPayload::Downloads(vec![row]),
                        );
                    }
                }
                "dl_open" => {
                    let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
                    open_path(path, false);
                }
                "dl_folder" => {
                    let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
                    open_path(path, true);
                }
                "dl_copy" => {
                    let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
                    self.copy_with_toast(text, "Link");
                }
                "bookmarks_tree" => {
                    self.bm_views.entry(msg.tab_id).or_default();
                    self.push_bm_tree(msg.tab_id);
                }
                "bookmarks_children" => {
                    let folder = v
                        .get("folder")
                        .and_then(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_owned());
                    self.bm_views.insert(
                        msg.tab_id,
                        BmView {
                            folder: folder.clone(),
                            query: String::new(),
                        },
                    );
                    self.push_bm_children(msg.tab_id, folder.as_deref());
                }
                "bookmarks_search" => {
                    let q = v.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    self.bm_views.entry(msg.tab_id).or_default().query = q.to_owned();
                    self.push_bm_search(msg.tab_id, q);
                }
                "bookmarks_commit" => {
                    // Enter in the filter box: real navigation, like history.
                    let q: String = v
                        .get("query")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect();
                    let url = if q.trim().is_empty() {
                        "ferrite://bookmarks".to_owned()
                    } else {
                        format!("ferrite://bookmarks?q={}", encode_query(&q))
                    };
                    if let Some(tab) = self.app.tabs.active_tab_mut() {
                        if let Err(e) = tab.load_url(url) {
                            debug!(error = %e, "bookmarks commit failed");
                        } else {
                            let id = tab.id;
                            self.clear_tab_dialogs(id);
                        }
                    }
                    self.layout_tabs();
                }
                "bookmarks_move" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let parent = v
                        .get("parent")
                        .and_then(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_owned());
                    if let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.move_bookmark(id, parent.as_deref())
                    {
                        debug!(error = %e, "bookmark move failed");
                    }
                    self.push_chrome(true);
                    self.push_panel_section();
                    self.push_bookmarks_tabs();
                }
                "bookmarks_rename" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
                    if let Some(vault) = self.vault.as_ref()
                        && let Err(e) = vault.rename_bookmark(id, title)
                    {
                        debug!(error = %e, "bookmark rename failed");
                    }
                    self.push_chrome(true);
                    self.push_panel_section();
                    self.push_bookmarks_tabs();
                }
                "bookmarks_delete_many" => {
                    let ids: Vec<String> = v
                        .get("ids")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                                .take(500)
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(vault) = self.vault.as_ref() {
                        for id in &ids {
                            if let Err(e) = vault.delete_bookmark(id) {
                                debug!(error = %e, "bookmark bulk delete failed");
                            }
                        }
                    }
                    self.push_chrome(true);
                    self.push_panel_section();
                    self.push_bookmarks_tabs();
                    // Tombstones, so every id restores exactly (parent,
                    // order, title intact). Always undoable.
                    if !ids.is_empty() {
                        let text = if ids.len() == 1 {
                            "Bookmark deleted".to_owned()
                        } else {
                            format!("{} bookmarks deleted", ids.len())
                        };
                        self.push_undo(text, crate::toast::UndoPayload::Bookmarks(ids));
                    }
                }
                "bookmarks_add_folder" => {
                    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    let parent = v
                        .get("parent")
                        .and_then(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_owned());
                    if let Some(vault) = self.vault.as_ref() {
                        match vault.add_folder(name, parent.as_deref()) {
                            Ok(_) => info!("manager folder created"),
                            Err(e) => debug!(error = %e, "manager folder failed"),
                        }
                    }
                    self.push_chrome(true);
                    self.push_panel_section();
                    self.push_bookmarks_tabs();
                }
                "bookmarks_dups" => {
                    self.push_bm_dups(msg.tab_id);
                }
                "restore_data" => {
                    // Session list for the restore page: rows, active index
                    // (culprit hint), crash count, and the crash-log tail.
                    // Object payload (not a row list): evaluate directly.
                    let (count, tabs, active) = self.peek_session_docs();
                    let (hint, genuine) = last_crash_hint();
                    let items: Vec<serde_json::Value> = tabs
                        .into_iter()
                        .map(|(url, title)| serde_json::json!({"url": url, "title": title}))
                        .collect();
                    let can_rollback = std::env::current_exe()
                        .map(|exe| crate::update::has_previous_version(&exe))
                        .unwrap_or(false);
                    let payload = serde_json::json!({
                        "tabs": items, "active": active, "count": count,
                        "hint": hint, "genuine": genuine,
                        "can_rollback": can_rollback,
                    });
                    let json = serde_json::to_string(&payload)
                        .unwrap_or_default()
                        .replace("</", "<\\/");
                    if let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        && let Some(h) = tab.webview.as_ref()
                        && let Err(e) = h.evaluate_script(&format!("__ferriteRestore({json})"))
                    {
                        debug!(error = %e, "restore push failed");
                    }
                }
                "restore_selected" => {
                    let urls: Vec<String> = v
                        .get("urls")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                                .take(200)
                                .collect()
                        })
                        .unwrap_or_default();
                    self.restore_pending = false;
                    // Hand off: the decision tab closes once tabs land.
                    if self.restore_chosen(&urls) > 0 {
                        self.close_tab_recorded(msg.tab_id);
                    }
                }
                "restore_all" => {
                    self.restore_pending = false;
                    if self.restore_session_tabs() > 0 {
                        self.activate_restored_tab();
                        self.close_tab_recorded(msg.tab_id);
                    }
                    self.layout_tabs();
                }
                "restore_fresh" => {
                    // Start fresh (never re-asked: the marker was already
                    // replaced at boot, like the infobar dismiss).
                    self.restore_pending = false;
                    if let Some(tab) = self.app.tabs.active_tab_mut() {
                        let _ = tab.load_url("ferrite://newtab".to_owned());
                    }
                    self.layout_tabs();
                }
                "restore_rollback" => {
                    // Roll back to the retained previous release, then
                    // relaunch into it through the graceful close path
                    // (about_to_wait consumes the relaunch marker). Session
                    // persist is skipped like any crash offer: the crashed
                    // tabs stay on disk for the relaunched build to offer.
                    // A failed rollback toasts and stays put (the driver
                    // preserves state on failure).
                    let exe = std::env::current_exe().unwrap_or_default();
                    match crate::update::rollback_to_previous(&exe) {
                        Ok(version) => {
                            tracing::info!(version = %version, "rolled back, relaunching");
                            crate::update::write_relaunch_marker(
                                &crate::profiles::data_root(),
                                &version,
                            );
                        }
                        Err(e) => {
                            self.notify(format!("Couldn't roll back: {e}"));
                        }
                    }
                }
                "restore_diag" => {
                    // Copy-only diagnostics (never auto-submitted. Same
                    // telemetry-off stance as everywhere else).
                    let engine = wry::webview_version().unwrap_or_else(|_| "unknown".to_owned());
                    let (hint, _) = last_crash_hint();
                    self.copy_with_toast(
                        &format!(
                            "Ferrite {}\nWeb engine {}\nPlatform {}\nTabs offered: {}\nLast error: {}",
                            env!("CARGO_PKG_VERSION"),
                            engine,
                            std::env::consts::OS,
                            self.crash_tabs,
                            if hint.is_empty() {
                                "(none recorded)"
                            } else {
                                &hint
                            },
                        ),
                        "Diagnostics",
                    );
                }
                "dismiss" => {
                    if let Some(tab) = self.app.tabs.active_tab()
                        && let Some(h) = tab.webview.as_ref()
                        && let Err(e) = h.focus()
                    {
                        debug!(error = %e, "dismiss refocus failed");
                    }
                }
                "link_hover" => {
                    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if !url.is_empty() {
                        self.hover = Some((msg.tab_id, url.to_owned()));
                    }
                }
                "link_leave" => {
                    if self
                        .hover
                        .as_ref()
                        .map(|(id, _)| *id == msg.tab_id)
                        .unwrap_or(false)
                    {
                        self.hover = None;
                    }
                }
                "context_menu" => {
                    let p = CtxPayload {
                        tab: msg.tab_id,
                        x: v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0),
                        y: v.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0),
                        href: capped_field(&v, "href", 2048),
                        src: capped_field(&v, "src", 2048),
                        selection: capped_field(&v, "selection", 200),
                        editable: v.get("editable").and_then(|x| x.as_bool()).unwrap_or(false),
                    };
                    // Only the active tab may open a menu: background posts
                    // are stale, and other pages share this channel.
                    if Some(msg.tab_id) == self.app.tabs.active_id() {
                        self.show_ctx_menu(p);
                    }
                }
                "ctx_activate" => {
                    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    self.activate_ctx(id);
                }
                "ctx_dismiss" => self.hide_ctx_menu(),
                "dialog_respond" => {
                    // Script-dialog overlay answer. Applies only to the
                    // shown frame. Stale or forged seqs die in
                    // `dialog_respond`, never in a script.
                    let seq = v.get("seq").and_then(|x| x.as_u64()).unwrap_or(u64::MAX);
                    let accepted = v.get("accepted").and_then(|x| x.as_bool()).unwrap_or(false);
                    let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
                    let text: String = text
                        .chars()
                        .take(ferrite_tabs::DIALOG_MESSAGE_CAP)
                        .collect();
                    let suppress = v.get("suppress").and_then(|x| x.as_bool()).unwrap_or(false);
                    self.dialog_respond(seq, accepted, text, suppress);
                }
                "reader_result" => {
                    self.handle_reader_result(msg.tab_id, &v);
                }
                "reader_chunk" => {
                    self.handle_reader_chunk(msg.tab_id, &v);
                }
                "reader_query" => {
                    // Reader page asking for its article (by its own ?src=).
                    let src = self
                        .app
                        .tabs
                        .get_tab(msg.tab_id)
                        .map(|t| t.url.clone())
                        .unwrap_or_default();
                    let src = src
                        .strip_prefix("ferrite://reader")
                        .unwrap_or("")
                        .trim_start_matches('?');
                    let src =
                        ferrite_tabs::protocol::decode_query_param(src, "src").unwrap_or_default();
                    if let Some((_, art)) = self.reader_cache.iter().find(|(s, _)| s == &src) {
                        let art = art.clone();
                        self.push_reader(msg.tab_id, &art);
                    } else {
                        self.push_reader(msg.tab_id, &ReaderArticle::default());
                    }
                }
                "reader_pref" => {
                    let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("");
                    match key {
                        "size" => {
                            let n = v
                                .get("value")
                                .and_then(|x| x.as_i64())
                                .map(|n| n.clamp(87, 150) as i32)
                                .unwrap_or(100);
                            self.reader_size = n;
                            persist_setting("reader_size", serde_json::Value::Number(n.into()));
                        }
                        "serif" => {
                            let b = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                            self.reader_serif = b;
                            persist_setting("reader_serif", serde_json::Value::Bool(b));
                        }
                        "wide" => {
                            let b = v.get("value").and_then(|x| x.as_bool()).unwrap_or(false);
                            self.reader_wide = b;
                            persist_setting("reader_wide", serde_json::Value::Bool(b));
                        }
                        "theme" => {
                            let t = v.get("value").and_then(|x| x.as_str()).unwrap_or("light");
                            let t = if matches!(t, "light" | "sepia" | "dark") {
                                t
                            } else {
                                "light"
                            };
                            self.reader_theme = t.to_owned();
                            persist_setting(
                                "reader_theme",
                                serde_json::Value::String(t.to_owned()),
                            );
                        }
                        _ => {}
                    }
                }
                "translate_state" => {
                    // Progress posts from the page-local engine. Only the
                    // active tab may toast: background posts are stale, and
                    // other pages share this channel.
                    if Some(msg.tab_id) == self.app.tabs.active_id()
                        && let Some(st) = ferrite_tabs::translate::TranslateState::parse(&v)
                    {
                        // Remember incapable hosts for this session: the
                        // toolbar stops offering what the engine just
                        // proved it can't do, instead of failing late on
                        // every click.
                        if matches!(
                            st.status,
                            ferrite_tabs::translate::TranslateStatus::Unavailable
                        ) && let Some(tab) = self.app.tabs.get_tab(msg.tab_id)
                        {
                            self.translate_unavailable
                                .insert(host_slice(&tab.url).to_owned());
                        }
                        if let Some(text) = st.toast_text() {
                            self.notify(text);
                        }
                    }
                }
                "media_drm_blocked" => {
                    // EME denial observed page-side. Only the active tab
                    // may toast (background players share this channel),
                    // and each tab+host+system toasts once per session so
                    // retry loops cannot spam the toast queue. Background
                    // denials wait in drm_pending and surface when their
                    // tab goes active, so they still explain themselves.
                    if let Some(rep) = ferrite_tabs::media::MediaDrmReport::parse(&v) {
                        let host = self
                            .app
                            .tabs
                            .get_tab(msg.tab_id)
                            .map(|t| host_slice(&t.url).to_owned())
                            .unwrap_or_default();
                        let key = rep.dedupe_key(&msg.tab_id.to_string(), &host);
                        if self.drm_reported.contains(&key) {
                            continue;
                        }
                        let text =
                            ferrite_tabs::media::DrmCapabilities::blocked_message(&rep.system);
                        if Some(msg.tab_id) == self.app.tabs.active_id() {
                            self.drm_reported.push_back(key);
                            while self.drm_reported.len() > ferrite_tabs::media::DRM_DEDUPE_CAP {
                                self.drm_reported.pop_front();
                            }
                            self.notify(text);
                        } else {
                            // Recorded too: the flush below toasts once,
                            // and a repeat denial must not re-arm it.
                            self.drm_reported.push_back(key);
                            while self.drm_reported.len() > ferrite_tabs::media::DRM_DEDUPE_CAP {
                                self.drm_reported.pop_front();
                            }
                            self.drm_pending.insert(msg.tab_id, text);
                        }
                    }
                }
                "find_result" => {
                    // Only the scoped tab may move the counter. Results
                    // from a backgrounded tab arrive stale and are dropped.
                    if Some(msg.tab_id) == self.find_tab {
                        self.find_total = v
                            .get("total")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0)
                            .min(1000) as u32;
                        self.find_index = (v.get("index").and_then(|x| x.as_u64()).unwrap_or(0)
                            as u32)
                            .min(self.find_total);
                        dirty = true;
                    }
                }
                // First-party page messages with no Rust consumer yet.
                // Explicitly ignored (logged at debug): they must NOT fall
                // through to title handling.
                "recovery" | "console" | "elements" | "storage" => {
                    debug!(msg_type, tab_id = %msg.tab_id, "ipc: known type without consumer - ignored");
                }
                "autofill_scan" => {
                    // Latest fields per tab (replaced wholesale. The scanner
                    // reports the full visible set each pass). Top frame
                    // only: rects are viewport-space, and iframe-relative
                    // coords would poison geometry matching (fill_hit).
                    // Origin-keyed one-shots (save candidates, risk reports)
                    // carry their own origin and work from any frame.
                    if !v.get("top").and_then(|x| x.as_bool()).unwrap_or(true) {
                        debug!(tab_id = %msg.tab_id, "ipc: subframe scan dropped");
                        continue;
                    }
                    let fields: Vec<ScanField> = v
                        .get("fields")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .take(50)
                                .filter_map(|f| {
                                    let rect = f.get("rect")?;
                                    Some(ScanField {
                                        kind: capped_field(f, "kind", 16),
                                        name: capped_field(f, "name", 128),
                                        id: capped_field(f, "id", 128),
                                        ac: capped_field(f, "ac", 64),
                                        x: rect.get("x")?.as_f64()?,
                                        y: rect.get("y")?.as_f64()?,
                                        w: rect.get("w")?.as_f64()?,
                                        h: rect.get("h")?.as_f64()?,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if fields.is_empty() {
                        self.scan_cache.remove(&msg.tab_id);
                    } else {
                        self.scan_cache.insert(msg.tab_id, fields);
                    }
                }
                "autofill_focus" => {
                    // Focus dropdown: stale posts after a tab switch must
                    // not steer (the rows resolve against the live tab).
                    // Top frame only: iframe rects are frame-relative and
                    // would misposition the dropdown. Iframe fills have no
                    // delivery path (no wry frame API, SOP-blocked).
                    let tab = msg.tab_id;
                    if !v.get("top").and_then(|x| x.as_bool()).unwrap_or(true) {
                        debug!(tab_id = %tab, "ipc: subframe focus dropped");
                    } else if Some(tab) != self.app.tabs.active_id() {
                        debug!(tab_id = %tab, "ipc: focus from background tab - ignored");
                    } else {
                        let f = ScanField {
                            kind: capped_field(&v, "kind", 16),
                            name: capped_field(&v, "name", 128),
                            id: capped_field(&v, "id", 128),
                            ac: capped_field(&v, "ac", 64),
                            x: v.get("x").and_then(|x| x.as_f64()).unwrap_or(-1.0),
                            y: v.get("y").and_then(|x| x.as_f64()).unwrap_or(-1.0),
                            w: v.get("w").and_then(|x| x.as_f64()).unwrap_or(0.0),
                            h: v.get("h").and_then(|x| x.as_f64()).unwrap_or(0.0),
                        };
                        if f.w > 0.0 && f.h > 0.0 && f.w.is_finite() && f.h.is_finite() {
                            self.af_dropdown(tab, &f);
                        }
                    }
                }
                "autofill_dismiss" => {
                    // Scroll-dismiss from the content script. Unconditional:
                    // a no-op when nothing is open, and a scrolling page
                    // should take down a context menu too.
                    self.hide_ctx_menu();
                }
                "af_nav" => {
                    // Forwarded arrow key: move the visual highlight with
                    // wraparound. The field keeps focus and caret.
                    if self.af_open {
                        let dir = v.get("dir").and_then(|x| x.as_i64()).unwrap_or(1);
                        let n = Self::af_visible(&self.af_items, &self.af_filter).len();
                        if n > 0 {
                            let cur = self.af_hl.unwrap_or(0) as i64;
                            self.af_hl =
                                Some(cur.saturating_add(dir).rem_euclid(n as i64) as usize);
                            self.af_repush();
                        }
                    }
                }
                "af_pick" => {
                    // Forwarded Enter: run the highlighted row (first row
                    // when nothing highlighted yet).
                    if self.af_open {
                        let visible = Self::af_visible(&self.af_items, &self.af_filter);
                        let idx = self.af_hl.unwrap_or(0).min(visible.len().saturating_sub(1));
                        if let Some(id) = visible
                            .get(idx)
                            .and_then(|r| r.get("id"))
                            .and_then(|s| s.as_str())
                        {
                            let id = id.to_owned();
                            self.activate_ctx(&id);
                        }
                    }
                }
                "af_filter" => {
                    // Forwarded field value: narrow rows, reset highlight.
                    if self.af_open {
                        self.af_filter = capped_field(&v, "value", 100);
                        self.af_hl = Some(0);
                        self.af_repush();
                    }
                }
                "blob_chunk" => {
                    // One base64 slice of a page-side blob fetch (36864 raw
                    // bytes per post, under the IPC cap). Unknown ids are
                    // ignored. Out-of-order or over-cap slices drop the
                    // whole transfer rather than reassembling garbage.
                    let id = capped_field(&v, "id", 32);
                    let seq = v.get("seq").and_then(|x| x.as_u64()).unwrap_or(u64::MAX);
                    let data = v.get("data").and_then(|x| x.as_str()).unwrap_or("");
                    let key = (msg.tab_id, id);
                    // ponytail: 100MB cap per transfer. Stale entries sweep
                    // in tick. Raise alongside disk-streaming, not before.
                    const BLOB_CAP: u64 = 100 * 1024 * 1024;
                    if !self.blob_asm.contains_key(&key) {
                        debug!("ipc: blob chunk for unknown transfer - ignored");
                    } else if let Some((buf, total, _)) = self.blob_asm.get_mut(&key) {
                        use base64::Engine as _;
                        let bytes = base64::engine::general_purpose::STANDARD.decode(data);
                        match bytes {
                            Ok(bytes)
                                if seq * 36864 == *total
                                    && *total + bytes.len() as u64 <= BLOB_CAP =>
                            {
                                *total += bytes.len() as u64;
                                buf.extend(bytes);
                            }
                            Ok(_) => {
                                self.blob_asm.remove(&key);
                                self.notify(
                                    "Couldn't save file: over the 100MB save-as cap".to_owned(),
                                );
                            }
                            Err(_) => {
                                self.blob_asm.remove(&key);
                                self.notify("Couldn't save file: interrupted download".to_owned());
                            }
                        }
                    }
                }
                "blob_end" => {
                    let id = capped_field(&v, "id", 32);
                    let mime = capped_field(&v, "mime", 128);
                    let key = (msg.tab_id, id);
                    if let Some((buf, _, _)) = self.blob_asm.remove(&key) {
                        if buf.is_empty() {
                            self.notify("Couldn't save file: unreadable blob".to_owned());
                        } else {
                            let ext = crate::filedlg::mime_ext(&mime);
                            crate::filedlg::spawn_save_bytes(
                                buf,
                                format!("download.{ext}"),
                                self.save_last_dir.clone(),
                                self.forced_download_dir(),
                                self.notice_tx.clone(),
                            );
                        }
                    }
                }
                "password_save_candidate" => {
                    // Login-submit report: the tab's URL is authoritative
                    // (never the posted origin. A framed page must not
                    // mint credentials for its top origin or vice versa;
                    // the prompt names the origin so a mismatch is visible).
                    let tab_url = self
                        .app
                        .tabs
                        .get_tab(msg.tab_id)
                        .map(|t| t.url.clone())
                        .unwrap_or_default();
                    let origin = ferrite_tabs::origin_of(&tab_url).unwrap_or_default();
                    if origin.is_empty() {
                        debug!("ipc: password candidate off-origin page - ignored");
                    } else {
                        let username: String = v
                            .get("username")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .chars()
                            .take(256)
                            .collect();
                        let password: Vec<u8> = v
                            .get("password")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .chars()
                            .take(4096)
                            .collect::<String>()
                            .into_bytes();
                        self.push_save_candidate(origin, username, password);
                    }
                }
                "form_history_save" => {
                    // Per-origin typed values from a submitted form. The
                    // tab's URL is authoritative (never the posted origin);
                    // excluded origins, sensitive keys, and digit runs
                    // (card/phone-shaped) are refused at the gate.
                    if !self.autofill_form {
                        debug!("ipc: form save with toggle off - ignored");
                    } else if let Some(vault) = self.vault.as_ref() {
                        let tab_url = self
                            .app
                            .tabs
                            .get_tab(msg.tab_id)
                            .map(|t| t.url.clone())
                            .unwrap_or_default();
                        let origin = ferrite_tabs::origin_of(&tab_url).unwrap_or_default();
                        if !origin.is_empty() && !vault.history_excluded(&tab_url) {
                            let now = now_millis();
                            if let Some(entries) = v.get("entries").and_then(|x| x.as_array()) {
                                for e in entries.iter().take(20) {
                                    let key = capped_field(e, "key", 128);
                                    let value: String = e
                                        .get("value")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("")
                                        .trim()
                                        .chars()
                                        .take(200)
                                        .collect();
                                    if key.is_empty()
                                        || value.is_empty()
                                        || Self::sensitive_field_key(&key)
                                        || (value.len() >= 7
                                            && value.chars().all(|c| c.is_ascii_digit()))
                                    {
                                        continue;
                                    }
                                    if let Err(err) =
                                        vault.record_form_value(&origin, &key, &value, now)
                                    {
                                        debug!(error = %err, "form record failed");
                                    }
                                }
                            }
                        }
                    }
                }
                unknown => {
                    debug!(msg_type = unknown, tab_id = %msg.tab_id, "ipc: unknown type - ignored");
                }
            }
        }
        // Idle discards free renderers: outstanding dialog deferrals die
        // with them, so complete-as-cancelled and clear the queues.
        // Zero-minute timeout disables the sweep (manual Sleep stays).
        if self.sleep_timeout_min > 0 {
            for id in self.app.tabs.discard_idle_tabs(Instant::now()) {
                self.clear_tab_dialogs(id);
            }
        }
        if self.app.sync_theme_from_tabs() {
            dirty = true;
        }
        // Crash-restore autosave: the session file must reflect live tabs,
        // not just the last clean exit. Otherwise a crash offer restores
        // stale data. Dirty fast-path (2s debounce after structural change)
        // plus a 30s backstop for nav/title commits TabState records
        // without reaching the manager. Atomic tmp+rename inside
        // save_to_file. Skipped while a crash offer OR the restore page is
        // pending: the file still holds the crashed session, and
        // overwriting it would destroy the offer.
        // ponytail: 2s/30s steps, not per-event writes. Per-keystroke
        // snapshots would churn the disk for zero restore gain.
        if !self.crash_offer && !self.restore_pending {
            let elapsed = self.last_autosave.elapsed().as_secs();
            let structural = elapsed >= 2 && self.app.tabs.take_dirty();
            // Scroll moves without touching the dirty flag (per-scroll
            // writes would churn); the backstop still covers it, and a
            // fully idle browser now skips the rewrite instead of paying
            // a whole-file encrypt every 30s. Worst case for scroll-only
            // staleness is one backstop interval; structural data keeps
            // the 2s fast path, and a 5-minute outer bound covers a
            // wedged flag. Flags clear only on write (below), never on
            // skip, or a quiet scroller would lose its position.
            let scrolled = self.scroll_dirty;
            if structural || (scrolled && elapsed >= 30) || elapsed >= 300 {
                self.scroll_dirty = false;
                // No session writes while blocked or ephemeral: the strip
                // holds only the decision tab, or nothing may persist.
                if self.boot_block.is_none() && !self.guest_mode {
                    if let Err(e) = self.save_session_file() {
                        debug!(error = %e, "session autosave failed");
                    }
                }
                // Download-flag reconcile (a dropped terminal event must
                // never pin a tab awake forever): cheap, idempotent.
                self.sync_download_flags();
                // Daily storage hygiene (first run lands ~30s after boot,
                // then every 24h): history retention + free-page reclaim.
                if self.last_retention_sweep.elapsed().as_secs() >= 86_400 {
                    self.retention_sweep();
                }
                self.flush_blocked_day();
                self.last_autosave = Instant::now();
                // Threat context refresh (5min): frecency moves slowly and
                // overrides change rarely. Memory copies for the hot path,
                // Vault stays the truth. Same tick, separate throttle.
                if self.last_threat_push.elapsed().as_secs() >= 300 {
                    if let Some(v) = self.vault.as_ref() {
                        match v.list_threat_exceptions() {
                            Ok(hosts) => self.privacy.set_threat_exceptions(hosts),
                            Err(e) => debug!(error = %e, "threat override refresh failed"),
                        }
                        self.privacy.set_frequent_hosts(v.top_hosts(256));
                    }
                    self.last_threat_push = Instant::now();
                }
            }
        }
        // Downloads live progress: re-push open downloads tabs at most
        // every 500ms (the tick itself is the throttle. No per-chunk
        // re-render, which stutters with several running at once).
        if self.last_dl_push.elapsed().as_millis() >= 500 {
            let dl_tabs = self.page_tabs("ferrite://downloads");
            if !dl_tabs.is_empty() {
                for id in dl_tabs {
                    let (q, status) = self.dl_queries.get(&id).cloned().unwrap_or_default();
                    let items = self.downloads_json(&q, &status);
                    self.push_list(id, "__ferriteDownloads", &items);
                }
                self.last_dl_push = Instant::now();
            }
        }
        // Task manager live poll: same 500ms throttle. Refresh first (CPU
        // needs consecutive samples), then push open pages.
        if self.last_tm_push.elapsed().as_millis() >= 500 {
            let tm_tabs = self.page_tabs("ferrite://taskmanager");
            if !tm_tabs.is_empty() {
                self.sys
                    .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                // Engine profile size: 60s cache (directory walks are
                // slow-path; a GB-sized cache must not cost per push).
                if self.engine_profile_at.elapsed().as_secs() >= 60 {
                    self.engine_profile_bytes =
                        dir_size_capped(&self.session.engine_dir(), 100_000);
                    self.engine_profile_at = Instant::now();
                }
                for id in tm_tabs {
                    self.push_taskmanager(id);
                }
                self.last_tm_push = Instant::now();
            }
        }
        // Self-update: hourly worker spawn, event drain, restart-reminder
        // escalation, About pushes. Worker does network off the loop.
        self.tick_update();
        // Cert viewer results: fetch threads report back here for push.
        while let Ok((tab_id, rep)) = self.cert_rx.try_recv() {
            self.cert_pending.remove(&tab_id);
            // Remember what was actually fetched: trust actions must match
            // this report, never a caller-supplied fingerprint.
            match rep.leaf.as_ref() {
                Some(leaf) => {
                    self.cert_last
                        .insert(tab_id, (rep.host.clone(), leaf.sha256.clone()));
                }
                None => {
                    self.cert_last.remove(&tab_id);
                }
            }
            self.push_cert(tab_id, &rep);
        }
        // View-source results: same bridge, separate channel.
        while let Ok((tab_id, rep)) = self.vs_rx.try_recv() {
            self.vs_pending.remove(&tab_id);
            self.push_viewsource(tab_id, &rep);
        }
        // Dictionary provisioning: first tick only, then per-tag reports.
        // Missing dictionaries for the active languages fetch once via
        // the direct client; failures are fail-soft (spellcheck continues
        // without that dictionary, retry next boot).
        if self.dict_boot_pending {
            self.dict_boot_pending = false;
            self.provision_missing_dicts();
        }
        while let Ok(done) = self.dict_rx.try_recv() {
            if done.ok {
                tracing::info!(tag = %done.tag, "spellcheck dictionary provisioned");
            } else {
                tracing::warn!(tag = %done.tag, "spellcheck dictionary unavailable");
            }
        }
        // Boot gate: the decision tab is the only tab while blocked. If it
        // was closed (every close path funnels here on the next tick),
        // reopen it rather than leaving a dead window.
        let decision_gone = self
            .boot_block
            .as_ref()
            .and_then(|b| b.tab)
            .is_some_and(|id| self.app.tabs.get_tab(id).is_none());
        if decision_gone {
            self.open_tab("ferrite://bootdecision".to_owned());
            let active = self.app.tabs.active_id();
            if let Some(block) = self.boot_block.as_mut() {
                block.tab = active;
            }
        }
        // Favicon results: apply only when the tab still shows the page
        // the fetch was issued for and still names this icon URL. Stale
        // (navigated, closed, re-reported) results only warm the cache.
        while let Ok(done) = self.favicon_rx.try_recv() {
            self.favicon_pending.remove(&done.icon_url);
            if let Some(data) = done.data_url {
                self.favicon_order.push_back(done.icon_url.clone());
                self.favicon_cache
                    .insert(done.icon_url.clone(), data.clone());
                while self.favicon_cache.len() > crate::favicon::FAVICON_CACHE_CAP {
                    if let Some(old) = self.favicon_order.pop_front() {
                        self.favicon_cache.remove(&old);
                    } else {
                        break;
                    }
                }
                if let Some(tab) = self.app.tabs.get_tab_mut(done.tab_id)
                    && tab.url == done.page_url
                    && tab.favicon_url.as_deref() == Some(done.icon_url.as_str())
                {
                    tab.set_favicon_data(data);
                    self.app.tabs.mark_dirty();
                    dirty = true;
                }
            }
        }
        // File-picker results: the dialog ran on a worker. The work runs
        // here (vault/file writes stay on the loop thread).
        while let Ok(done) = self.filedlg_rx.try_recv() {
            if let Some(path) = &done.path
                && let Some(dir) = path.parent()
            {
                self.save_last_dir = Some(dir.to_owned());
                persist_setting(
                    "save_last_dir",
                    serde_json::Value::String(dir.to_string_lossy().into_owned()),
                );
            }
            self.handle_pick_done(done);
        }
        // Worker failure texts for the toast (success stays silent).
        while let Ok(text) = self.notice_rx.try_recv() {
            self.notify(text);
        }
        // Intercepted script dialogs (Windows glue holds each deferral
        // under its frame seq until the overlay answers).
        #[cfg(target_os = "windows")]
        while let Ok(ev) = self.dialog_rx.try_recv() {
            self.on_dialog_opened(ev);
        }
        // Ask-every-time downloads: denied in-handler, dialog + refetch
        // here (GET + tab cookies; POST-body downloads can't survive
        // this, documented on the DeniedDownload type).
        while let Ok(d) = self.dlask_rx.try_recv() {
            let cookies = self.tab_cookies(d.tab_id, &d.url);
            // Guest: no save dialog by design (any permanent path it could
            // offer would violate ephemerality). Fetch straight into the
            // temp dir; the first-download warning covers the surprise.
            let force = self
                .guest_profile
                .as_ref()
                .map(crate::profiles::guest_downloads_dir);
            crate::filedlg::spawn_fetch_save(
                d.url,
                d.filename,
                cookies,
                self.downloads.clone(),
                self.save_last_dir.clone(),
                force,
                self.notice_tx.clone(),
                Some(d.tab_id),
            );
        }
        // PDF viewer: remote fetch completions from worker threads.
        // The tab may be gone (closed mid-fetch): then the bytes drop
        // with the result instead of opening somewhere surprising.
        while let Ok(done) = self.pdf_fetch_rx.try_recv() {
            let tab_alive = self.app.tabs.get_tab(done.tab_id).is_some();
            match (done.result, tab_alive) {
                (Ok(bytes), true) => {
                    self.pdf_open_bytes(done.tab_id, done.title, done.url, bytes, None);
                }
                (Err(e), true) => {
                    self.notify(format!("Couldn't open PDF: {e}"));
                }
                _ => {
                    debug!("pdf fetch landed on a closed tab; dropping bytes");
                }
            }
        }
        // PDF viewer: engine-link interceptions vetoed in the navigation
        // handler. Re-classified here (defense in depth: the handler and
        // the loop must agree, or nothing opens).
        while let Ok(ev) = self.pdf_intercept_rx.try_recv() {
            let (tab_id, url) = (ev.tab_id, ev.url);
            if self.app.tabs.get_tab(tab_id).is_none() {
                continue;
            }
            match ferrite_tabs::pdf::classify_pdf_url(&url) {
                ferrite_tabs::pdf::PdfNav::Remote => {
                    self.notify("Fetching PDF...".to_owned());
                    crate::pdf::spawn_pdf_fetch(
                        url.clone(),
                        crate::pdf::display_title(&url),
                        tab_id,
                        self.pdf_fetch_tx.clone(),
                    );
                }
                ferrite_tabs::pdf::PdfNav::Local => {
                    self.pdf_open_local(tab_id, &url);
                }
                _ => {
                    debug!("pdf intercept ignored: handler/loop disagree");
                }
            }
        }
        // Guest downloads: first active download earns one explicit
        // warning (temp folder, deleted on exit). Once per session; the
        // newtab banner states the rule permanently.
        if self.guest_mode && !self.guest_dl_warned {
            use ferrite_tabs::DownloadStateKind as S;
            let active = self.download_log.iter().any(|e| {
                matches!(
                    e.state,
                    S::Started | S::Progress | S::Paused | S::Interrupted
                )
            });
            if active {
                self.guest_dl_warned = true;
                self.notify(
                    "Incognito download: saved to a temporary folder and deleted when you quit."
                        .to_owned(),
                );
            }
        }
        let mut sug_dirty = false;
        // Remote suggestions: accept only the latest generation whose
        // input still matches (slow/older replies die here, silently).
        while let Ok((generation, input, phrases)) = self.sug_rx.try_recv() {
            if generation == self.sug_gen && input == self.app.omnibox.input.trim() {
                let rows: Vec<SugRow> = phrases
                    .into_iter()
                    .map(|p| SugRow {
                        url: p.clone(),
                        title: p,
                        tab: None,
                        kind: "search".to_owned(),
                        provider: None,
                    })
                    .collect();
                self.remote_sugs = (generation, input, rows);
                sug_dirty = true;
            }
        }
        // Page-zoom restore runs before the push so the chip follows
        // within one heartbeat of navigation, switch, or restore.
        self.zoom_sync_tick();
        // Toast queue: retire the head past its duration and advance.
        // A dead undo staging drops with its toast (the delete stands);
        // a dead save staging drops zeroized with its toast.
        if self.toasts.tick(Instant::now()) {
            dirty = true;
        }
        self.sync_undo_deadline(Instant::now());
        self.sync_save_deadline(Instant::now());
        // Stale blob rescues (page died mid-transfer): drop after 2min.
        self.blob_asm
            .retain(|_, (_, _, at)| at.elapsed() < std::time::Duration::from_secs(120));
        // Open palette follows the live tab set: a close or load under the
        // popup re-pushes (signature compare, so steady state costs one
        // JSON build per tick while open, nothing when closed).
        if self.overlay_open {
            let tabs = serde_json::to_string(&self.palette_tab_rows())
                .unwrap_or_else(|_| "[]".to_owned())
                .replace("</", "<\\/");
            let json = self.palette_snapshot();
            if json != self.palette_sig {
                self.palette_sig = json;
                if let Some(ov) = self.overlay.as_ref()
                    && let Err(e) = ov.evaluate_script(&format!("__ferriteTabs({tabs})"))
                {
                    debug!(error = %e, "palette tabs refresh failed");
                }
            }
        }
        if dirty || sug_dirty {
            self.push_chrome(false);
        }
    }

    // ── Boot ──

    fn bootstrap(&mut self, elwt: &ActiveEventLoop) {
        let config = self.config.clone();
        let window = match elwt.create_window(config.window_attributes()) {
            Ok(w) => Arc::new(w),
            Err(s) => {
                self.pending_error = Some(ShellError::WindowCreation { source: s });
                elwt.exit();
                return;
            }
        };
        window.set_cursor_visible(true);
        self.window = Some(window.clone());
        apply_frame_theme(&window, self.theme_pack);
        self.app.is_initialized = true;
        // Engine gate (Windows): no Evergreen Runtime means every webview
        // creation below fails. Say so once, with the fix, instead of
        // cascading into per-tab errors. Checked here (not at install
        // time, which Ferrite doesn't control) and before the first
        // WebView2 instance, per Microsoft's guidance.
        #[cfg(target_os = "windows")]
        if ferrite_tabs::webview2_runtime_version().is_none() {
            engine_missing_dialog();
            self.pending_error = Some(ShellError::WebView2Runtime(
                "WebView2 Runtime not found".to_owned(),
            ));
            elwt.exit();
            return;
        }
        if let Some((w, h)) = self.window_logical() {
            self.app.update_window_dimensions(w, h);
        }
        // Chrome first (full window), so content webviews created below can
        // be positioned in the slot immediately.
        match self.app.tabs.factory().create_with_id(
            &window,
            "ferrite://chrome",
            self.full_wry(),
            self.chrome_id,
        ) {
            Ok(h) => {
                self.chrome = Some(h);
                self.push_chrome(true);
            }
            Err(e) => {
                error!(error = %e, "chrome webview creation failed");
                self.pending_error = Some(ShellError::Chrome(e.to_string()));
                elwt.exit();
                return;
            }
        }
        if self.app.tabs.is_empty() {
            if self.boot_block.is_some() {
                // Encrypted stores failed: the decision screen is the only
                // tab until Retry/Guest/Quit resolves the gate.
                self.open_tab("ferrite://bootdecision".to_owned());
                let active = self.app.tabs.active_id();
                if let Some(block) = self.boot_block.as_mut() {
                    block.tab = active;
                }
            } else if self.config.incognito {
                // Incognito boots skip restore/welcome/crash flows outright:
                // one fresh tab, bannered, with an explicit toast so the
                // ephemeral window is unmistakable.
                let first = self
                    .config
                    .initial_url
                    .clone()
                    .filter(|u| !u.trim().is_empty())
                    .unwrap_or_else(|| "ferrite://newtab".to_owned());
                self.open_tab(first);
                self.notify(
                    "Incognito window: nothing on this device will keep what you do here."
                        .to_owned(),
                );
            } else {
                self.boot_tabs();
                self.maybe_show_updated();
            }
        }
        self.layout_tabs();
        self.push_chrome(true);
        // First paint: stamp + append the local-only boot line (durations
        // only, never URLs/titles: safe for the diagnostics copy below).
        let paint_ms = self.boot_t0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        append_boot_log_in(
            &crate::profiles::data_root(),
            env!("CARGO_PKG_VERSION"),
            self.boot_stores_ms,
            Some(paint_ms),
        );
        if let Some((w, h)) = self.window_logical() {
            info!(logical_width = w, logical_height = h, "shell bootstrapped");
        }
    }

    /// Normal first-tabs flow: welcome wizard, crash offer/restore page, or
    /// session restore with CLI/fresh fallback. Runs at bootstrap when the
    /// boot gate is clear, and again when Retry/Guest resolves the gate
    /// (the decision tab is closed first, so the strip is empty again).
    /// Guest sessions skip restore and open one fresh tab: yesterday's
    /// encrypted tabs must not leak into an ephemeral window.
    fn boot_tabs(&mut self) {
        if self.guest_mode {
            self.open_tab("ferrite://newtab".to_owned());
            return;
        }
        // First run (no flag, no explicit URL): the welcome wizard.
        // Skippable on every screen. Sovereignty means never forcing
        // a wizard before browsing.
        let cli_url = self
            .config
            .initial_url
            .clone()
            .filter(|u| !u.trim().is_empty());
        if !self.onboarding_completed && cli_url.is_none() {
            self.open_tab("ferrite://welcome".to_owned());
        } else if self.should_pick_profile() {
            // Several profiles, no explicit pick: the picker owns first
            // paint (choices resolve back into boot_tabs, crash state
            // intact; nothing is consumed until Default proceeds).
            self.profile_pick_pending = true;
            self.open_tab("ferrite://profiles".to_owned());
        } else {
            // Crash marker (written at launch, deleted on clean exit): a
            // leftover means the last run died abnormally. Offer its
            // autosaved session once instead of silently restoring it.
            // ponytail: no registry/queue. One window-scoped kind exists.
            // A second kind earns the abstraction.
            let (crashed, crashes) = take_crash_marker();
            let (saved_tabs, _, _) = self.peek_session_docs();
            // Crash-loop escalation: from the second consecutive crash on,
            // open the full restore page INSTEAD of loading anything. No
            // tab reloads behind the decision (deferred restore satisfies
            // the timing requirement without a second window).
            let restored = if crashed && !self.restore_always && saved_tabs > 0 {
                if crashes >= CRASH_PAGE_THRESHOLD {
                    self.restore_pending = true;
                    self.crash_tabs = saved_tabs;
                    self.open_tab("ferrite://restore".to_owned());
                    1
                } else {
                    self.crash_tabs = saved_tabs;
                    self.crash_offer = true;
                    1
                }
            } else {
                self.startup_tabs()
            };
            let mut opened_cli = false;
            if restored == 0 {
                // CLI --url wins over a fresh newtab (explicit user intent).
                let first = self
                    .config
                    .initial_url
                    .clone()
                    .filter(|u| !u.trim().is_empty());
                let first = first.unwrap_or_else(|| "ferrite://newtab".to_owned());
                if self.proxy_down {
                    // No silent direct browsing: the proxy was wanted but
                    // is unusable. Park the first tab on the error page
                    // with the intended URL so Retry has somewhere to go
                    // after a restart fixes the proxy.
                    self.open_tab(format!(
                        "ferrite://error?url={}&reason=proxy-down&at={}",
                        encode_query(&first),
                        now_millis()
                    ));
                } else {
                    opened_cli = cli_url.is_some();
                    self.open_tab(first);
                }
            } else {
                self.activate_restored_tab();
            }
            // Explicit CLI URLs open IN ADDITION (newest ends active, like
            // restore-all): invocation intent is never dropped silently,
            // whether the session restored or not.
            if !opened_cli && let Some(url) = cli_url {
                self.open_tab(url);
            }
        }
    }

    /// Mode-driven first tabs (no crash pending, no welcome): restore
    /// silently reopens; newtab opens nothing (the caller falls back to
    /// blank); urls opens each validated startup URL (active ends on the
    /// last). Empty urls falls back to blank. Returns the opened count
    /// for the activate-or-fallback decision in `boot_tabs`.
    fn startup_tabs(&mut self) -> usize {
        match self.startup_mode.as_str() {
            "newtab" => 0,
            "urls" => {
                let urls = self.startup_urls.clone();
                if urls.is_empty() {
                    return 0;
                }
                let mut n = 0;
                for u in urls {
                    self.open_tab(u);
                    n += 1;
                }
                n
            }
            _ => self.restore_session_tabs(),
        }
    }

    /// Post-update banner, once: if a staged update applied at the last
    /// boot, open About with the previous version called out. Skipped in
    /// guest/incognito (ephemeral windows don't narrate updates) and
    /// while the boot gate blocks (the decision screen owns first paint).
    /// The marker was already consumed in the constructor, so this fires
    /// at most once per update no matter how boot resolves.
    fn maybe_show_updated(&mut self) {
        if self.guest_mode || self.config.incognito || self.boot_block.is_some() {
            return;
        }
        if let Some(prev) = self.updated_from.take() {
            use ferrite_tabs::protocol::encode_query_component;
            self.open_tab(format!(
                "ferrite://about?updated={}",
                encode_query_component(&prev)
            ));
        }
    }

    // ── Input ──

    fn on_local_key(&mut self, event: &KeyEvent) -> bool {
        let Some(window) = self.window.clone() else {
            return false;
        };
        if event.state != ElementState::Pressed || event.repeat {
            return false;
        }
        // Boot gate: no local shortcuts while blocked (new tabs, private
        // tabs, taskmanager, find, zoom would all open or mutate state
        // behind the decision screen). F11 falls through (window chrome,
        // no browsing state); everything else is consumed here.
        if self.boot_block.is_some() && !matches!(event.logical_key, Key::Named(NamedKey::F11)) {
            return true;
        }
        if self.modifiers.control_key() && self.modifiers.shift_key() {
            if let Key::Character(ref s) = event.logical_key
                && s.eq_ignore_ascii_case("a")
            {
                self.toggle_web_tabsearch();
                return true;
            }
            if let Key::Character(ref s) = event.logical_key
                && s.eq_ignore_ascii_case("i")
            {
                self.open_devtools();
                return true;
            }
            // Private tab: incognito engine store, never persisted. Same
            // creation-time constraint as everything engine-side.
            if let Key::Character(ref s) = event.logical_key
                && s.eq_ignore_ascii_case("n")
            {
                self.open_private_tab("ferrite://newtab".to_owned());
                return true;
            }
        }
        // Page zoom: Ctrl/Cmd +,-,0 (unshifted = included. The + key
        // reports "=" without Shift). Numpad keys have no stable
        // Character mapping. Add when someone misses them.
        if (self.modifiers.control_key() || self.modifiers.super_key())
            && !self.modifiers.shift_key()
            && !self.modifiers.alt_key()
        {
            if let Key::Character(ref s) = event.logical_key {
                if s == "+" || s == "=" {
                    self.zoom_step_active(1);
                    return true;
                }
                if s == "-" {
                    self.zoom_step_active(-1);
                    return true;
                }
                if s == "0" {
                    self.zoom_reset_active();
                    return true;
                }
            }
        }
        if event.logical_key == Key::Named(NamedKey::Escape)
            && self.modifiers.shift_key()
            && !self.modifiers.control_key()
            && !self.modifiers.alt_key()
        {
            self.overlay_open = false;
            Self::close_web_tabsearch(&mut self.overlay, &mut self.app.tabs);
            self.open_tab("ferrite://taskmanager".to_owned());
            return true;
        }
        if self.overlay_open && event.logical_key == Key::Named(NamedKey::Escape) {
            self.overlay_open = false;
            Self::close_web_tabsearch(&mut self.overlay, &mut self.app.tabs);
            return true;
        }
        match event.logical_key {
            Key::Named(NamedKey::F11) => {
                self.toggle_fullscreen(&window);
                true
            }
            Key::Named(NamedKey::F12) => {
                self.open_devtools();
                true
            }
            Key::Named(NamedKey::F3) => {
                // Find owns F3 everywhere (next match; Shift+F3 previous).
                // The status toggle is menu-only with no default key.
                if self.modifiers.shift_key() {
                    if self.find_open {
                        let (q, case) = (self.find_query.clone(), self.find_case);
                        self.find_eval("prev", &q, case);
                    }
                } else if self.find_open {
                    let (q, case) = (self.find_query.clone(), self.find_case);
                    self.find_eval("next", &q, case);
                } else {
                    self.open_find_bar();
                }
                true
            }
            Key::Named(NamedKey::F4) => {
                // F4 cycles DESIGN packs (each pack owns its flare. Archive
                // gold still overrides via accent context sync in tick).
                let next = self.theme_pack.next();
                self.set_theme_pack(next);
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("b")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                self.set_panel_open(!self.panel_open);
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("b")
                    && self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                let next = match self.bar_mode.as_str() {
                    "always" => "ntp",
                    "ntp" => "never",
                    _ => "always",
                };
                self.bar_seen = true;
                persist_setting("bar_seen", serde_json::Value::Bool(true));
                self.set_bar_mode(next);
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("f")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                self.open_find_bar();
                true
            }
            Key::Character(ref s) if (s == "f" || s == "F") => {
                if let Some(tab) = self.app.tabs.active_tab() {
                    if !tab.is_loading && !tab.is_crashed {
                        let theme = &self.app.theme;
                        let hint_theme = ferrite_omnibox::HintTheme {
                            accent_hex: theme.resolved_interactive_accent().to_owned(),
                            text_hex: theme.palette.substrate.to_owned(),
                            ..ferrite_omnibox::HintTheme::iris()
                        };
                        let js = ferrite_omnibox::hint_generation_js("asdfghjkl", &hint_theme);
                        if let Some(handle) = tab.webview.as_ref()
                            && let Err(e) = handle.evaluate_script(&js)
                        {
                            debug!(error = %e, "hint injection failed");
                        }
                    }
                }
                true
            }
            // Ctrl-gated shortcuts: bare l/t/w must never fire while typing
            // in a page (the old code opened/closed tabs mid-typing).
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("l")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                self.eval_chrome("__ferriteFocusOmnibox()");
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("p")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                self.print_active();
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("t")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                self.open_tab("ferrite://newtab".to_owned());
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("k")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                // Command palette (same overlay as tab search, unified
                // list). Ctrl-gated like every other shell shortcut, so
                // typing k in a page never fires it.
                self.toggle_web_tabsearch();
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("t")
                    && self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                self.reopen_closed_tab();
                true
            }
            Key::Named(NamedKey::Tab)
                if self.modifiers.control_key() && !self.modifiers.alt_key() =>
            {
                // Ctrl+Tab cycles (Shift reverses). Same focus caveat as all
                // shell shortcuts: the OS window must hold keyboard focus.
                let n = self.app.tabs.len();
                if n > 1
                    && let Some(cur) = self.app.tabs.active_index()
                {
                    let next = if self.modifiers.shift_key() {
                        (cur + n - 1) % n
                    } else {
                        (cur + 1) % n
                    };
                    self.switch_to_index(next);
                }
                true
            }
            Key::Character(ref s)
                if s.len() == 1
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key()
                    && matches!(
                        s.as_str(),
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
                    ) =>
            {
                // Ctrl+1..8 jumps to tab N, Ctrl+9 to the last tab.
                let n = self.app.tabs.len();
                if n > 0 {
                    let idx = if s.as_str() == "9" {
                        n - 1
                    } else {
                        s.parse::<usize>().unwrap_or(1).saturating_sub(1).min(n - 1)
                    };
                    self.switch_to_index(idx);
                }
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("w")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                if let Some(id) = self.app.tabs.active_id() {
                    // Pinned tabs refuse normal close (PinnedTab error);
                    // surface it in the log instead of failing silently.
                    self.close_tab_recorded(id);
                } else {
                    self.layout_tabs();
                }
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("d")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                if let Some(tab) = self.app.tabs.active_tab() {
                    let (url, title) = (tab.url.clone(), tab.title.clone());
                    self.toggle_star(url, title);
                }
                true
            }
            Key::Character(ref s)
                if s.eq_ignore_ascii_case("r")
                    && self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && !self.modifiers.alt_key() =>
            {
                // Same smart toggle as the toolbar button and Esc: a loading
                // tab stops instead of reloading over itself.
                self.reload_or_stop_active();
                true
            }
            Key::Named(NamedKey::ArrowLeft) if self.modifiers.alt_key() => {
                if let Some(tab) = self.app.tabs.active_tab_mut()
                    && let Some(url) = tab.go_back()
                {
                    if let Err(e) = tab.load_traversal(url) {
                        debug!(error = %e, "alt-back traversal failed");
                    } else {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                    }
                }
                true
            }
            Key::Named(NamedKey::ArrowRight) if self.modifiers.alt_key() => {
                if let Some(tab) = self.app.tabs.active_tab_mut()
                    && let Some(url) = tab.go_forward()
                {
                    if let Err(e) = tab.load_traversal(url) {
                        debug!(error = %e, "alt-forward traversal failed");
                    } else {
                        let id = tab.id;
                        self.clear_tab_dialogs(id);
                    }
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Stop a loading tab.
                if self
                    .app
                    .tabs
                    .active_tab()
                    .map(|t| t.is_loading)
                    .unwrap_or(false)
                {
                    if let Some(tab) = self.app.tabs.active_tab_mut()
                        && let Err(e) = tab.stop()
                    {
                        debug!(error = %e, "esc stop failed");
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn toggle_fullscreen(&mut self, window: &Window) {
        self.fullscreen = self.fullscreen.toggle();
        let mode = match self.fullscreen {
            FullscreenMode::Borderless => window
                .current_monitor()
                .map(|m| Fullscreen::Borderless(Some(m))),
            FullscreenMode::Windowed => None,
        };
        window.set_fullscreen(mode);
        trace!("fullscreen -> {}", self.fullscreen.debug_label());
    }

    // ── Imports (settings-page buttons, native picker, no dialogs inline) ──

    /// Launch a native picker on a worker (never block the loop).
    fn launch_pick(
        &self,
        spec: crate::filedlg::PickSpec,
        tag: crate::filedlg::PickTag,
        tab_id: Uuid,
    ) {
        use crate::filedlg::PickTag as Tag;
        // Guest: no file picker that writes. Reads (imports) stay
        // available; they land in memory stores and vanish with the
        // window. Writes would break the ephemeral promise silently, so
        // they are refused here, before any dialog opens.
        let writes = matches!(
            tag,
            Tag::ExportSettings
                | Tag::BookmarkExport
                | Tag::HistoryExport
                | Tag::PasswordExport
                | Tag::ExportAll
                | Tag::BackupExport
        );
        if writes && is_ephemeral() {
            let _ = self
                .notice_tx
                .send("Exports aren't available in Incognito sessions.".to_owned());
            return;
        }
        crate::filedlg::spawn_pick(
            spec,
            tag,
            tab_id,
            self.save_last_dir.clone(),
            self.filedlg_tx.clone(),
        );
    }

    /// Picker completions: the dialog is done (or was cancelled), the
    /// vault/file work runs here on the loop thread.
    fn handle_pick_done(&mut self, done: crate::filedlg::PickDone) {
        use crate::filedlg::PickTag as Tag;
        let tab_id = done.tab_id;
        match done.tag {
            Tag::WelcomeBookmarks => {
                let (report, error) = match (&done.path, self.vault.as_ref()) {
                    (Some(path), Some(vault)) => match import_from_path(vault, "bookmarks", path) {
                        Ok(r) => (r, None),
                        // A picked file that parses as nothing errors
                        // inside the importer; surface it instead of a
                        // misleading zero.
                        Err(e) => (
                            ferrite_vault::import::ImportReport::default(),
                            Some(e.to_string()),
                        ),
                    },
                    _ => (ferrite_vault::import::ImportReport::default(), None),
                };
                info!(?report, "welcome import");
                self.push_welcome_result(
                    tab_id,
                    "bookmarks",
                    report.imported,
                    report.skipped(),
                    0,
                    error,
                );
            }
            Tag::WelcomePasswords => {
                // Explicit conflict rule, never silent either way:
                // identical rows skip, differing passwords keep the stored
                // entry and queue a conflict for ferrite://passwords.
                // (The Arc is cloned up front so the queue mutation below
                // never fights the vault borrow.)
                let vault = self.vault.clone();
                let mut imported = 0u64;
                let mut skipped = 0u64;
                if let (Some(path), Some(vault), Some(mut key)) =
                    (done.path, vault, Self::cred_key())
                {
                    use ferrite_credentials::ImportVerdict;
                    let store = ferrite_credentials::CredentialStore::new(vault.clone());
                    let (recs, csv) =
                        ferrite_credentials::import_csv(&path.to_string_lossy(), &key);
                    skipped += csv.skipped as u64;
                    for rec in &recs {
                        match store.import_row(rec, &key) {
                            ImportVerdict::Imported => imported += 1,
                            ImportVerdict::SkippedIdentical | ImportVerdict::Failed => skipped += 1,
                            ImportVerdict::ConflictKeptExisting => {
                                self.pw_conflicts.push(PendingPasswordConflict {
                                    origin: rec.url.clone(),
                                    username: rec.username.clone(),
                                    password: rec.decrypt_password(&key).unwrap_or_default(),
                                });
                            }
                        }
                    }
                    key.zeroize();
                }
                let conflicts = self.pw_conflicts.len() as u64;
                info!(imported, skipped, conflicts, "welcome password import");
                self.push_welcome_result(tab_id, "passwords", imported, skipped, conflicts, None);
                for id in self.page_tabs("ferrite://passwords") {
                    self.push_passwords(id);
                }
            }
            Tag::ExportSettings => {
                if let Some(dest) = done.path {
                    // Explicit user export: decrypted document to the picked
                    // path (never the sealed file itself, which is useless
                    // outside this device key).
                    let mut export_key = Self::cred_key();
                    let doc = crate::settings_file::read_settings_value(export_key.as_deref());
                    if let Some(k) = export_key.as_mut() {
                        k.zeroize();
                    }
                    match serde_json::to_string_pretty(&doc) {
                        Ok(text) => match std::fs::write(&dest, text) {
                            Ok(_) => {
                                info!(path = %dest.display(), "settings exported");
                                self.notify(
                                    "Settings exported as plain JSON. It includes secrets. Store it safely."
                                        .to_owned(),
                                );
                            }
                            Err(e) => warn!(error = %e, "settings export failed"),
                        },
                        Err(e) => warn!(error = %e, "settings file invalid, export refused"),
                    }
                }
            }
            Tag::ImportSettings => {
                if let Some(path) = done.path {
                    match std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok())
                    {
                        Some(value) => {
                            if self.write_and_apply_settings(tab_id, &value) {
                                info!(path = %path.display(), "settings imported");
                            }
                        }
                        _ => warn!("settings import rejected: not a JSON object"),
                    }
                }
            }
            Tag::BookmarkExport => {
                if let (Some(path), Some(vault)) = (done.path, self.vault.as_ref()) {
                    let html = ferrite_vault::import::export_bookmarks_netscape(vault);
                    match std::fs::write(&path, &html) {
                        Ok(_) => self.notify("Bookmarks exported as HTML.".to_owned()),
                        Err(e) => {
                            warn!(error = %e, "bookmark export failed");
                            self.notify(format!("Couldn't write bookmarks export: {e}"));
                        }
                    }
                }
            }
            Tag::HistoryExport => {
                if let (Some(path), Some(vault)) = (done.path, self.vault.as_ref()) {
                    let csv = ferrite_vault::import::export_history_csv(vault, 100_000);
                    match std::fs::write(&path, &csv) {
                        Ok(_) => self.notify("History exported as CSV.".to_owned()),
                        Err(e) => {
                            warn!(error = %e, "history export failed");
                            self.notify(format!("Couldn't write history export: {e}"));
                        }
                    }
                }
            }
            Tag::BookmarkKind { kind } => {
                if let (Some(path), Some(vault)) = (done.path, self.vault.as_ref()) {
                    match import_from_path(vault, &kind, &path) {
                        Ok(report) => {
                            info!(kind = %kind, ?report, "import complete");
                            let noun = if kind == "history" {
                                ("visit", "visits")
                            } else {
                                ("bookmark", "bookmarks")
                            };
                            self.notify(format!(
                                "Import complete: {}.",
                                report.describe(noun.0, noun.1)
                            ));
                        }
                        Err(e) => {
                            warn!(error = %e, kind = %kind, "import failed");
                            self.notify(format!("Couldn't import {kind}: {e}"));
                        }
                    }
                }
            }
            Tag::PasswordExport => {
                // Stash clears on write AND on cancel (take first).
                let csv = self.pending_export.take();
                if let (Some(path), Some(csv)) = (done.path, csv) {
                    match std::fs::write(&path, &csv) {
                        Ok(_) => info!(path = %path.display(), "passwords exported"),
                        Err(e) => {
                            self.notify(format!("Couldn't write passwords CSV: {e}"));
                        }
                    }
                }
            }
            Tag::ExportAll | Tag::BackupExport => {
                // Binary stash (tarball or sealed backup): zeroized
                // whether written or cancelled (take first).
                match (done.path, self.pending_blob.take()) {
                    (Some(path), Some(mut bytes)) => {
                        match std::fs::write(&path, &bytes) {
                            Ok(_) => info!(path = %path.display(), "export written"),
                            Err(e) => {
                                self.notify(format!("Couldn't write export: {e}"));
                            }
                        }
                        bytes.zeroize();
                    }
                    (_, Some(mut bytes)) => bytes.zeroize(),
                    _ => {}
                }
            }
            Tag::BackupImport => {
                // Stash the picked file. The page prompts for the
                // passphrase next, then posts `backup_restore`.
                self.pending_backup_path = done.path.clone();
                if let Some(tab) = self.app.tabs.get_tab(tab_id)
                    && let Some(h) = tab.webview.as_ref()
                    && let Err(e) = h.evaluate_script("__ferriteBackupPrompt()")
                {
                    debug!(error = %e, "backup prompt push failed");
                    self.pending_backup_path = None;
                }
            }
        }
    }
}
impl ApplicationHandler<ShellWake> for ShellApp {
    fn new_events(&mut self, elwt: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::Init) {
            elwt.set_control_flow(ControlFlow::Wait);
        }
        if let Some(err) = self.pending_error.take() {
            error!(error = %err, "fatal");
            elwt.exit();
        }
    }
    fn resumed(&mut self, elwt: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let _span = tracing::debug_span!("shell.boot").entered();
        debug!("resumed");
        self.bootstrap(elwt);
    }
    fn user_event(&mut self, _elwt: &ActiveEventLoop, event: ShellWake) {
        match event {
            // Both wakes funnel into the same drain: the tick empties the
            // IPC bridge, both proxy channels, and the forward queue.
            ShellWake::Ipc | ShellWake::Forward => self.tick(),
        }
    }
    fn window_event(&mut self, elwt: &ActiveEventLoop, _window_id: WindowId, event: WindowEvent) {
        if self.window.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                info!("close requested: stopping proxy and shutting down");
                // Guest profile owns a temp dir (downloads included): wipe
                // it on clean exit. Crash leftovers are swept at boot.
                // Session teardown covers the same tree via the single
                // identity (no second path to forget); the take() keeps both.
                if let Some(profile) = self.guest_profile.take() {
                    crate::profiles::destroy(&profile);
                }
                self.session.teardown();
                // Private-tab spill dir (persistent processes): wipe on clean
                // exit so private downloads never outlive the window. Crash
                // leftovers are swept at boot by PID liveness.
                let _ = std::fs::remove_dir_all(crate::profiles::private_downloads_dir());
                // Drop session-scoped permission grants before exit (process
                // ends anyway; explicit clear keeps the intent reviewable).
                self.permissions.clear_ephemeral();
                // Stop the sync thread promptly (it would otherwise ride out
                // its sleep to process death).
                self.sync_stop
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                // A pending crash offer or restore page freezes the session
                // file: closing without answering is NOT consent to discard
                //. The crashed tabs stay on disk and the marker stays, so
                // the next boot offers again instead of silently losing them.
                if !self.crash_offer && !self.restore_pending {
                    // Best-effort encrypted session persist for crash
                    // recovery (AES-GCM, atomic tmp+rename inside.
                    // Failure only logs). Skipped while blocked (decision
                    // tab only) or ephemeral (nothing may persist).
                    if self.boot_block.is_none() && !self.guest_mode {
                        if let Err(e) = self.save_session_file() {
                            warn!(error = %e, "session persist failed");
                        }
                    }
                    // Clean exit: remove the crash marker (a leftover at next
                    // launch is exactly how a crash is detected) and reset
                    // the loop counter. Skipped in guest sessions sharing
                    // the process list with a normal window: erasing the
                    // marker would destroy another window's crash evidence.
                    if !self.guest_mode {
                        if let Err(e) = std::fs::remove_file(session_lock_path()) {
                            debug!(error = %e, "crash marker removal failed");
                        }
                        reset_crash_count();
                    }
                }
                if let Some(proxy) = self.proxy.take() {
                    proxy.stop();
                }
                if let Some(proxy) = self.private_proxy.take() {
                    proxy.stop();
                }
                elwt.exit();
            }
            WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    trace!("minimised");
                    return;
                }
                if let Some((w, h)) = self.window_logical() {
                    self.app.update_window_dimensions(w, h);
                }
                self.layout_tabs();
                if let Some(chrome) = self.chrome.as_ref()
                    && let Some(rect) = self.full_wry()
                    && let Err(e) = chrome.set_bounds(rect)
                {
                    debug!(error = %e, "chrome resize failed");
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some((w, h)) = self.window_logical() {
                    self.app.update_window_dimensions(w, h);
                }
                self.layout_tabs();
            }
            WindowEvent::RedrawRequested => {
                // No retained scene: just re-pin bounds and push if dirty.
                self.layout_tabs();
                self.push_chrome(false);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.on_local_key(&event);
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            _ => (),
        }
    }
    fn about_to_wait(&mut self, elwt: &ActiveEventLoop) {
        if self.window.is_none() {
            return;
        }
        if let Some(err) = self.pending_error.take() {
            error!(error = %err, "frame error");
            elwt.exit();
            return;
        }
        // Rollback relaunch: the restore page asked to restart into the
        // previous release. Same cleanup as a window close minus session
        // persist (crashed tabs stay on disk for the new build to offer);
        // `run()` consumes the marker and spawns the restored binary.
        if crate::update::relaunch_marker_path(&crate::profiles::data_root()).exists() {
            if let Some(proxy) = self.proxy.take() {
                proxy.stop();
            }
            if let Some(proxy) = self.private_proxy.take() {
                proxy.stop();
            }
            elwt.exit();
            return;
        }
        // Decision-screen Quit/Retry: clean-exit bookkeeping (no crash
        // offer follows a deliberate exit), then leave the loop. Retry
        // additionally spawns a fresh process first: the new boot reads
        // the keyring and verifies stores for real, which an in-process
        // retry could only approximate around live handles.
        if self.quit_requested || self.relaunch_requested {
            if self.relaunch_requested
                && let Ok(exe) = std::env::current_exe()
            {
                info!("relaunching after boot-gate retry");
                let _ = std::process::Command::new(&exe).spawn();
            }
            if let Some(profile) = self.guest_profile.take() {
                crate::profiles::destroy(&profile);
            }
            self.session.teardown();
            let _ = std::fs::remove_dir_all(crate::profiles::private_downloads_dir());
            self.permissions.clear_ephemeral();
            self.sync_stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(proxy) = self.proxy.take() {
                proxy.stop();
            }
            if let Some(proxy) = self.private_proxy.take() {
                proxy.stop();
            }
            // Incognito windows never touch the shared crash marker (see
            // CloseRequested): another window's crash evidence survives.
            if !self.guest_mode {
                let _ = std::fs::remove_file(session_lock_path());
                reset_crash_count();
            }
            elwt.exit();
            return;
        }
        // 2Hz heartbeat: IPC/telemetry drains and chrome pushes stay alive
        // without any repaint loop. Steady state issues no webview traffic
        // beyond dirty-checked pushes.
        elwt.set_control_flow(ControlFlow::WaitUntil(Instant::now() + HEARTBEAT));
        // Drain on every wake (input events included) for low latency;
        // pushes stay dirty-checked so idle wakes are free.
        self.tick();
    }
    fn suspended(&mut self, _elwt: &ActiveEventLoop) {
        debug!("suspended");
    }
}

/// Chrome-webview payload classifier for the drain: init-script telemetry
/// (`{type:...}`, posted by scripts that run in every webview including
/// chrome) is not a command and must fall through to the shared path, not
/// be rejected. Anything without a string `type` field stays rejected.
/// Pure so the routing decision is unit-testable without a Shell.
fn chrome_payload_is_telemetry(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("type")?.as_str().map(|_| ()))
        .is_some()
}

/// Missing-engine guidance (Windows): no WebView2 Runtime means no webview
/// can render. Native dialog, shown once before exit, plain text with the
/// Evergreen installer address spelled out.
#[cfg(target_os = "windows")]
fn engine_missing_dialog() {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Ferrite needs the WebView2 Runtime")
        .set_description(
            "Ferrite could not find the WebView2 Runtime, so it cannot open \
             any pages on this machine.\n\nInstall the Evergreen Runtime from \
             Microsoft, then start Ferrite again:\n\
             https://go.microsoft.com/fwlink/p/?LinkId=2124703",
        )
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Open a downloaded file, or reveal it in its folder. The page passes a
/// bare filename (sanitized at save time). Anything with separators or
/// parent refs is rejected, and the file must exist under the download dir.
fn open_path(name: &str, reveal: bool) {
    if name.is_empty() || name.contains(['/', '\\']) {
        return;
    }
    let path = ferrite_tabs::default_download_dir().join(name);
    if !path.is_file() {
        debug!(name, "open requested for missing file");
        return;
    }
    #[cfg(windows)]
    let r = if reveal {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
    } else {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.display().to_string()])
            .spawn()
    };
    #[cfg(target_os = "macos")]
    let r = if reveal {
        std::process::Command::new("open")
            .args(["-R", &path.display().to_string()])
            .spawn()
    } else {
        std::process::Command::new("open").arg(&path).spawn()
    };
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let r = if reveal {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path.as_path()))
            .spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&path).spawn()
    };
    if let Err(e) = r {
        debug!(error = %e, name, "open failed");
    }
}

/// `#rrggbb` to Windows COLORREF (`0x00bbggrr`). `None` on malformed input.
/// Windows-only in lib builds (the caption-tint path is its sole caller);
/// also compiled under `test` everywhere so the pure-logic tests keep running.
#[cfg(any(windows, test))]
fn hex_to_colorref(hex: &str) -> Option<u32> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u32::from_str_radix(&h[0..2], 16).ok()?;
    let g = u32::from_str_radix(&h[2..4], 16).ok()?;
    let b = u32::from_str_radix(&h[4..6], 16).ok()?;
    Some(r | (g << 8) | (b << 16))
}

/// Native-frame theming (Windows only): tint the OS caption to the active
/// pack through two direct `dwmapi` calls (no new dependency). Buttons, snap,
/// drag, and screen-reader support stay native. Best-effort, failures log.
#[cfg(windows)]
mod winframe {
    type Hwnd = *mut std::ffi::c_void;
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: Hwnd,
            dwAttribute: u32,
            pvAttribute: *const std::ffi::c_void,
            cbAttribute: u32,
        ) -> i32;
    }
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
    /// 2 = Mica (Win11 signature material). Best-effort: older builds
    /// reject the attribute and keep the solid caption color below.
    const DWMSBT_MICA: u32 = 2;

    fn set(hwnd: Hwnd, attr: u32, value: u32) -> bool {
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attr,
                &value as *const u32 as *const std::ffi::c_void,
                4,
            ) == 0
        }
    }

    pub fn apply(hwnd: Hwnd, colorref: u32, dark: bool) {
        // Dark mode first (system menus follow it), then the Win11 Mica
        // material, then the solid caption color as the pre-Mica fallback.
        // Each step is independent: a rejection logs and keeps going.
        if !set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(dark)) {
            tracing::debug!("DWM dark-mode attr rejected");
        }
        if !set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MICA) {
            tracing::debug!("DWM backdrop attr rejected (pre-Win11 22000?)");
        }
        if !set(hwnd, DWMWA_CAPTION_COLOR, colorref) {
            tracing::debug!("DWM caption-color attr rejected (pre-Win11?)");
        }
    }
}

/// Tint the native OS frame to `pack`. No-op off Windows (macOS keeps
/// traffic lights by HIG. Linux compositors vary too much to override).
fn apply_frame_theme(window: &Window, pack: ThemePack) {
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let colorref = hex_to_colorref(pack.frame_bg());
        match (window.window_handle(), colorref) {
            (Ok(handle), Some(color)) => {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    winframe::apply(h.hwnd.get() as *mut _, color, pack.frame_dark())
                }
            }
            _ => tracing::debug!("frame theme skipped: no handle or bad color"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, pack);
    }
}

/// Full privileged-page theme block: shell base tokens, then the active
/// pack (overlapping `--f-*` names resolve to the pack by cascade order),
/// then a pack-driven `color-scheme` so `prefers-color-scheme` queries in
/// pages track the design pack, not the OS. Single constructor for the
/// boot factory seed and the live pack-switch path; they must never drift.
fn full_theme_css(theme: &ThemeConfig, pack: ThemePack) -> String {
    format!(
        "{}{}:root{{color-scheme:{}}}",
        theme.theme_css(),
        pack.css(),
        if pack.frame_dark() { "dark" } else { "light" }
    )
}

/// Home destinations are deliberately narrow: web or internal pages only.
fn valid_home_url(url: &str) -> bool {
    let u = url.trim();
    !u.is_empty()
        && u.len() <= 2048
        && (u.starts_with("https://") || u.starts_with("http://") || u.starts_with("ferrite://"))
}

/// Max startup URLs (mode `urls`). Enough for a morning set; a hostile
/// settings file must not open 500 tabs.
pub const MAX_STARTUP_URLS: usize = 10;

/// Startup mode from settings: `restore` | `newtab` | `urls`. Unknown
/// values (hand edits, future modes) fall back to restore, never blank.
fn clean_startup_mode(raw: &str) -> &'static str {
    match raw.trim() {
        "newtab" => "newtab",
        "urls" => "urls",
        _ => "restore",
    }
}

/// Startup URL list from a settings array: valid http(s)/ferrite URLs,
/// trimmed, deduped, capped. Garbage entries drop, never the list.
fn clean_startup_urls(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(a) = v.as_array() {
        for item in a.iter().filter_map(|x| x.as_str()) {
            let u = item.trim().to_owned();
            if valid_home_url(&u) && !out.contains(&u) {
                out.push(u);
                if out.len() >= MAX_STARTUP_URLS {
                    break;
                }
            }
        }
    }
    out
}

/// Managed first-run seed file, beside the data root (Chromium
/// `master_preferences` precedent). JSON object, unknown keys ignored.
pub const MASTER_PREFS_NAME: &str = "master-preferences.json";

/// Read the seed file: must parse as a JSON object, else None (a parseable
/// object always yields whatever valid keys it holds; per-key validation
/// below isolates failures, so one malformed key cannot poison the rest).
fn read_master_prefs() -> Option<serde_json::Value> {
    let text =
        std::fs::read_to_string(crate::profiles::data_root().join(MASTER_PREFS_NAME)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.is_object().then_some(v)
}

/// Seed fresh settings from master preferences. Keys honored:
/// `homepage` (valid http(s)/ferrite URL), `startup_mode`
/// (restore/newtab/urls, cleaned), `startup_urls` (validated list),
/// `search_engine` (known engine id, else default). Unknown keys
/// ignored; invalid values fall back. Pure for tests.
fn apply_master_prefs(
    settings: serde_json::Value,
    master: &serde_json::Value,
) -> serde_json::Value {
    let mut out = settings;
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    if let Some(home) = master
        .get("homepage")
        .and_then(|x| x.as_str())
        .filter(|u| valid_home_url(u))
    {
        obj.insert(
            "home_url".to_owned(),
            serde_json::Value::String(home.to_owned()),
        );
    }
    if let Some(mode) = master.get("startup_mode").and_then(|x| x.as_str()) {
        obj.insert(
            "startup_mode".to_owned(),
            serde_json::Value::String(clean_startup_mode(mode).to_owned()),
        );
    }
    let urls = master
        .get("startup_urls")
        .map(clean_startup_urls)
        .unwrap_or_default();
    if !urls.is_empty() {
        obj.insert("startup_urls".to_owned(), serde_json::Value::from(urls));
    }
    if let Some(engine) = master.get("search_engine").and_then(|x| x.as_str()) {
        // Registry validation lives downstream (unknown ids fall back
        // in set_current); store verbatim, trimmed and capped.
        let id = engine.trim();
        if !id.is_empty() && id.len() <= 64 {
            obj.insert(
                "omnibox_engine".to_owned(),
                serde_json::Value::String(id.to_owned()),
            );
        }
    }
    out
}

/// OS memory pressure 0.0–1.0 from total/available bytes: the fraction
/// of RAM in use. Unknown totals (or zero) yield None and the caller
/// keeps the previous value; a bogus sample must never slam the
/// discard table to either extreme. Pure for tests.
fn memory_pressure(total: u64, available: u64) -> Option<f32> {
    if total == 0 || available > total {
        return None;
    }
    Some(1.0 - available as f32 / total as f32)
}

/// Minimum gap between memory samples: sysinfo walks process tables,
/// so sampling rides the slow path, never the 500ms heartbeat.
const MEM_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// Directory size in bytes, best effort: unreadable entries skip, never
/// fail. Symlinks are not followed (a profile dir must not escape into
/// the filesystem), entry count capped (a pathological cache must not
/// hang the measurer). Pure over any dir for tests.
fn dir_size_capped(dir: &std::path::Path, cap: usize) -> u64 {
    fn walk(dir: &std::path::Path, left: &mut usize, total: &mut u64) {
        if *left == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if *left == 0 {
                return;
            }
            *left -= 1;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            } else if kind.is_dir() {
                walk(&entry.path(), left, total);
            } else if kind.is_file() {
                if let Ok(m) = entry.metadata() {
                    *total = total.saturating_add(m.len());
                }
            }
        }
    }
    let mut total = 0u64;
    let mut left = cap;
    walk(dir, &mut left, &mut total);
    total
}

/// Canonical http(s) origin from user input (site page param, shield
/// host): lowercase `scheme://host`, default ports stripped, no
/// userinfo/path/query. Bare hosts assume https. None when unusable.
/// Pure for tests.
fn clean_site_origin(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() || t.len() > 512 || t.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let with_scheme = if t.contains("://") {
        t.to_owned()
    } else {
        format!("https://{t}")
    };
    ferrite_tabs::permissions::origin_of(&with_scheme)
}

/// Host half of a canonical origin (`scheme://host` → `host`).
fn site_origin_host(origin: &str) -> &str {
    origin.split("://").nth(1).unwrap_or(origin)
}

/// Current time as millis since the Unix epoch (autocomplete recency).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Synchronous wrapper for the direct fetch (updater closures are `Fn()`,
/// not async). One current-thread runtime per call, 30s budget, blocking
/// is free. `None` means retain last-known-good.
fn block_on_direct_fetch(url: &str) -> Option<(String, Option<String>)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(tokio::time::timeout(
        std::time::Duration::from_secs(30),
        ferrite_privacy::updater::fetch_direct(url),
    ))
    .ok()
    .flatten()
}

/// This device's long-term sync identity: stable id, display name, and the
/// keypair rebuilt from the vault-sealed seed at boot. The seed itself is
/// zeroized after reconstruction and never stored outside the vault.
#[derive(Debug, Clone)]
struct SyncSelf {
    identity: std::sync::Arc<ferrite_sync::DeviceIdentity>,
    name: String,
}

/// Device trust graph handed to the shell: the trust store (encrypted vault
/// rows) plus the pairing ceremony bound to it. Live transport reads this
/// same store when it lands, so approvals and revocations made today are
/// enforced at the first handshake without re-pairing.
struct SyncTrust {
    store: std::sync::Arc<ferrite_sync::TrustedStore>,
    pairing: ferrite_sync::PairingService,
    this: SyncSelf,
}

/// Build the trust graph over `vault`. Fails closed on any step (bad seed,
/// bad id, schema error): half a trust graph is worse than none, because
/// approvals would bind keys the store cannot later verify.
fn init_sync_trust(vault: &std::sync::Arc<ferrite_vault::Vault>) -> Result<SyncTrust, String> {
    let store = std::sync::Arc::new(ferrite_sync::TrustedStore::new(std::sync::Arc::clone(
        &vault.conn,
    )));
    store
        .init_schema()
        .map_err(|e| format!("trust store: {e}"))?;
    let row = vault
        .load_or_create_device_identity()
        .map_err(|e| format!("device identity: {e}"))?;
    let seed: [u8; 32] = row
        .seed
        .as_slice()
        .try_into()
        .map_err(|_| "device seed corrupt: move the vault aside to re-pair".to_owned())?;
    let device_id = row
        .device_id
        .parse::<uuid::Uuid>()
        .map_err(|_| "device id corrupt: move the vault aside to re-pair".to_owned())?;
    let identity = ferrite_sync::DeviceIdentity::from_seed(device_id, &seed)
        .map_err(|e| format!("device identity: {e}"))?;
    // Seed served its purpose: the keypair lives in memory from here.
    use zeroize::Zeroize;
    let mut seed = seed;
    seed.zeroize();
    let pairing = ferrite_sync::PairingService::new(ferrite_sync::TrustedStore::new(
        std::sync::Arc::clone(&vault.conn),
    ));
    Ok(SyncTrust {
        store,
        pairing,
        this: SyncSelf {
            identity: std::sync::Arc::new(identity),
            name: row.device_name,
        },
    })
}

/// Sort device rows newest-first by `first_paired_at` (missing stamps sort
/// last). The trust list doubles as the pairing review surface: recency
/// first, so a just-approved device is visible without scrolling. Pure.
fn sort_device_rows(rows: &mut [serde_json::Value]) {
    rows.sort_by_key(|r| {
        std::cmp::Reverse(
            r.get("first_paired_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        )
    });
}

/// Best-effort LAN address for pairing codes (the endpoint a future direct
/// transport would dial). UDP `connect` sends no packets; failure yields an
/// empty string, which decoders treat as "no listener yet" (port 0 agrees).
fn best_lan_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_default()
}

/// Remote-key fingerprint for pairing review: SHA-256, first 4 bytes hex
/// (8 chars), the same shape as `DeviceIdentity::fingerprint`, so a phone
/// call can compare both ends aloud. Pure.
fn fingerprint_pubkey(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Repair-vs-fresh wording for an approved pairing code. Approving a code
/// for an already-trusted id with a different key rotates the key
/// (repair-shaped, allowed) but must say so; silent rotation is where
/// takeovers hide. Same key, unknown id, or revoked rows: no notice
/// (fresh pair, or the revoke path owns the story). Pure for tests.
fn pairing_repair_text(
    known: Option<(&str, &[u8])>,
    incoming_name: &str,
    incoming_key: &[u8],
) -> Option<String> {
    let (name, key) = known?;
    if key == incoming_key {
        return None;
    }
    Some(format!(
        "{incoming_name} paired with a new key (was paired as {name}). If you did not re-pair this device, revoke it."
    ))
}

/// Bulk catch-up indicator without protocol changes: two consecutive
/// non-empty rounds close together means convergence is still working
/// through backlog. Anything else reads as done. Times are wall ms;
/// skew-tolerant by construction (only ordering within a minute matters).
/// Pure for tests.
fn catching_up(prev: Option<(usize, i64)>, actions: usize, at_ms: i64) -> bool {
    match prev {
        Some((prev_actions, prev_at)) => {
            // Ordered and close: backward clocks (skew, restored snapshots)
            // read as done, never as syncing.
            prev_actions > 0 && actions > 0 && at_ms >= prev_at && at_ms - prev_at <= 60_000
        }
        None => false,
    }
}

/// Boot timing log (local only, never transmitted): one line per boot
/// (`v=<version> stores=<ms|?> paint=<ms|?>`). Powers the About
/// diagnostics line so "startup got slow" is answerable without
/// telemetry. Capped at 200 lines; durations carry no user data by
/// construction (no URLs, titles, or counts).
const BOOT_LOG_NAME: &str = "ferrite-boot.log";
const BOOT_LOG_LINES: usize = 200;

fn boot_ms(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "?".to_owned())
}

pub(crate) fn append_boot_log_in(
    dir: &std::path::Path,
    version: &str,
    stores_ms: Option<u64>,
    paint_ms: Option<u64>,
) {
    let line = format!(
        "v={} stores={} paint={}\n",
        version.trim(),
        boot_ms(stores_ms),
        boot_ms(paint_ms)
    );
    let path = dir.join(BOOT_LOG_NAME);
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.push(line.trim_end().to_owned());
    while lines.len() > BOOT_LOG_LINES {
        lines.remove(0);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, lines.join("\n") + "\n");
}

/// Latest boot line for diagnostics (`None` when never logged).
/// Durations only; safe to copy out.
pub(crate) fn read_boot_last_in(dir: &std::path::Path) -> Option<String> {
    // Newest-first scan (double-ended): the log appends, so the answer
    // is usually the last line. Empty lines skipped either way.
    std::fs::read_to_string(dir.join(BOOT_LOG_NAME))
        .ok()?
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::to_owned)
}

/// Crash marker file: created at launch, deleted on clean exit. A leftover
/// at the next launch means the previous run died abnormally. Scoped per
/// profile (two live profiles must not clear each other's evidence).
fn session_lock_path() -> std::path::PathBuf {
    let id = crate::profiles::process_profile();
    if id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID) {
        crate::profiles::data_root().join("ferrite-session.lock")
    } else {
        crate::profiles::profile_dir(id).join("session.lock")
    }
}
/// Consecutive-crash counter (escalates infobar to full page).
fn crash_count_path() -> std::path::PathBuf {
    let id = crate::profiles::process_profile();
    if id.eq_ignore_ascii_case(crate::profiles::DEFAULT_ID) {
        crate::profiles::data_root().join("ferrite-crash-count")
    } else {
        crate::profiles::profile_dir(id).join("crash-count")
    }
}
/// Second consecutive crash opens the full restore page, not the infobar.
const CRASH_PAGE_THRESHOLD: u32 = 2;
/// Check for a leftover crash marker, then (re)write a fresh one either
/// way. So one crash is never reported twice, and a later crash in this
/// run is still detected. Returns (crashed, consecutive-crash count).
fn take_crash_marker() -> (bool, u32) {
    let lock = session_lock_path();
    let count_path = crash_count_path();
    if let Some(parent) = lock.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let crashed = lock.exists();
    if crashed {
        let _ = std::fs::remove_file(&lock);
    }
    if let Err(e) = std::fs::write(&lock, std::process::id().to_string()) {
        debug!(error = %e, "crash marker unwritable");
    }
    let count = if crashed {
        let content = std::fs::read_to_string(&count_path).unwrap_or_default();
        let n = parse_crash_count(&content, env!("CARGO_PKG_VERSION")).saturating_add(1);
        let _ = std::fs::write(
            &count_path,
            format_crash_count(env!("CARGO_PKG_VERSION"), n),
        );
        n
    } else {
        0
    };
    (crashed, count)
}

/// Reset the crash counter (clean exit only, an unanswered offer keeps
/// counting, so ignoring the bar still escalates).
fn reset_crash_count() {
    let _ = std::fs::write(
        crash_count_path(),
        format_crash_count(env!("CARGO_PKG_VERSION"), 0),
    );
}

/// Parse the crash-counter file for this build (`{version}` newline
/// `{count}`, bare number is legacy). Counts under other versions parse
/// as zero (fixed bugs must not keep escalating).
fn parse_crash_count(content: &str, current_version: &str) -> u32 {
    let mut lines = content.lines();
    match (lines.next(), lines.next()) {
        (Some(version), Some(count)) => {
            if version.trim() == current_version {
                count.trim().parse::<u32>().unwrap_or(0)
            } else {
                0
            }
        }
        (Some(only), None) => only.trim().parse::<u32>().unwrap_or(0),
        _ => 0,
    }
}

/// Counter file rendering (version-scoped, see [`parse_crash_count`]).
fn format_crash_count(version: &str, n: u32) -> String {
    format!("{version}\n{n}")
}

/// Last crash-log line plus whether it looks like a genuine panic (stamped
/// within a day) or an external kill or power loss (stale or absent log).
/// Heuristic, labeled as such wherever shown.
fn last_crash_hint() -> (String, bool) {
    let line = std::fs::read_to_string(crate::profiles::crash_log_path())
        .ok()
        .and_then(|d| d.lines().last().map(|l| l.to_owned()))
        .unwrap_or_default();
    let recent = line
        .split([':', ' '])
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|t| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().saturating_sub(t) < 86_400)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    (line.chars().take(300).collect(), recent)
}

/// Parse a session document without loading tabs: count, (url, title)
/// pairs in file order, and the active index. Empty when missing or corrupt
/// (mirrors the loader tolerance: version plus url-length checks). Pure:
/// callers load the document (decrypted, migration replayed) and toast on
/// quarantine themselves.
fn parse_session_doc(doc: Option<&str>) -> (u32, Vec<(String, String)>, usize) {
    let empty = (0, Vec::new(), 0);
    let v: serde_json::Value = match doc.and_then(|d| serde_json::from_str(d).ok()) {
        Some(v) => v,
        None => return empty,
    };
    if v.get("version").and_then(|x| x.as_u64()) != Some(1) {
        return empty;
    }
    let tabs: Vec<(String, String)> = v
        .get("tabs")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .take(200)
                .filter_map(|t| {
                    let url = t.get("url")?.as_str()?;
                    if url.is_empty() || url.len() > 2048 {
                        return None;
                    }
                    Some((
                        url.to_owned(),
                        t.get("title")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let active = (v.get("active").and_then(|x| x.as_u64()).unwrap_or(0) as usize)
        .min(tabs.len().saturating_sub(1));
    (tabs.len() as u32, tabs, active)
}

/// Resolve the device master key: load, else generate-and-store. Returns
/// the key plus whether it is durably stored. A generated-but-unstorable
/// key is returned as non-durable: callers must not back persistent files
/// with it (they would become unopenable time bombs tomorrow).
fn resolve_master_key() -> (Option<Vec<u8>>, bool, Option<String>) {
    match ferrite_vault::keyring::load_master_key() {
        Ok(Some(k)) => (Some(k), true, None),
        Ok(None) => match ferrite_vault::keyring::generate_master_key() {
            Ok(k) => match ferrite_vault::keyring::store_master_key(&k) {
                Ok(()) => (Some(k), true, None),
                Err(e) => {
                    warn!(error = %e, "keyring store failed: no persistent store may open this session");
                    (
                        Some(k),
                        false,
                        Some(
                            "The device key couldn't be stored, so nothing may be persisted this session."
                                .to_owned(),
                        ),
                    )
                }
            },
            Err(e) => {
                warn!(error = %e, "vault unavailable: master key cannot be generated");
                (None, false, None)
            }
        },
        Err(e) => {
            warn!(error = %e, "vault unavailable: keyring unreadable");
            (None, false, None)
        }
    }
}

/// Resume an interrupted rotation at boot: converge every store to the
/// journaled next key, promote it, delete the journal. Returns Ok(true)
/// when a journal existed and fully converged (caller re-resolves the key
/// and proceeds to a normal open), Ok(false) when no journal exists, and
/// Err(reason) when convergence failed (journal kept for the next boot;
/// the gate blocks with the reason).
fn maybe_resume_rotation() -> Result<bool, String> {
    use ferrite_vault::keyring;
    let Some(next) =
        keyring::load_next_key().map_err(|e| format!("rotation journal unreadable: {e}"))?
    else {
        return Ok(false);
    };
    let Some(old) =
        keyring::load_master_key().map_err(|e| format!("rotation master unreadable: {e}"))?
    else {
        return Err(
            "rotation journal exists but the master key is gone; resolve the keyring first"
                .to_owned(),
        );
    };
    let profile = crate::profiles::Profile::default_profile();
    let (session_enc, session_legacy) = crate::session::session_paths();
    let (settings_enc, settings_legacy) = crate::settings_file::settings_paths();
    let out = resume_inner(
        &profile,
        &session_enc,
        &session_legacy,
        &settings_enc,
        &settings_legacy,
        &old,
        &next,
    );
    // Scrub both keys whatever happened; keyring holds the truth.
    let mut old = old;
    let mut next = next;
    old.zeroize();
    next.zeroize();
    out
}

fn resume_inner(
    profile: &crate::profiles::Profile,
    session_enc: &std::path::Path,
    session_legacy: &std::path::Path,
    settings_enc: &std::path::Path,
    settings_legacy: &std::path::Path,
    old: &[u8],
    next: &[u8],
) -> Result<bool, String> {
    use ferrite_vault::keyring;
    let vault_path = crate::profiles::vault_path(profile)
        .to_string_lossy()
        .into_owned();
    let archive_path = crate::profiles::archive_path(profile)
        .to_string_lossy()
        .into_owned();
    // Files first (idempotent probes, no live handles needed).
    crate::rotation::reseal_session_file(session_enc, session_legacy, old, next)
        .map_err(|e| format!("session: {e}"))?;
    crate::rotation::reseal_settings_file(settings_enc, settings_legacy, old, next)
        .map_err(|e| format!("settings: {e}"))?;
    // Databases via fresh opens: new key first (already rotated), else
    // old key plus live rekey through the fresh handle.
    resume_one_db(
        "vault",
        &vault_path,
        old,
        next,
        |path, key| {
            ferrite_vault::Vault::open(ferrite_vault::VaultConfig::new(path, key))
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |key| {
            let v = ferrite_vault::Vault::open(ferrite_vault::VaultConfig::new(
                vault_path.clone(),
                key,
            ))
            .map_err(|e| e.to_string())?;
            v.rekey(next).map_err(|e| e.to_string())
        },
    )?;
    resume_one_db(
        "archive",
        &archive_path,
        old,
        next,
        |path, key| {
            let cfg = ferrite_archive::ArchiveConfig {
                path,
                ..Default::default()
            };
            ferrite_archive::Archive::open(cfg, &key)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |key| {
            let cfg = ferrite_archive::ArchiveConfig {
                path: archive_path.clone(),
                ..Default::default()
            };
            let a = ferrite_archive::Archive::open(cfg, &key).map_err(|e| e.to_string())?;
            a.rekey(next).map_err(|e| e.to_string())
        },
    )?;
    // All converged: promote the journaled key, then delete the journal.
    // A crash between the two replays promotion harmlessly next boot.
    keyring::store_master_key(next).map_err(|e| format!("rotation promote failed: {e}"))?;
    keyring::delete_next_key().map_err(|e| format!("rotation journal delete failed: {e}"))?;
    Ok(true)
}

/// Converge one database file to the journaled key: a fresh open under it
/// (which also recreates a missing file, logged) converges immediately;
/// else open under the old key and rekey, then verify under the new key.
/// Every step is idempotent, so crash-and-retry converges.
fn resume_one_db<O, R>(
    label: &str,
    path: &str,
    old_key: &[u8],
    new_key: &[u8],
    opens_under: O,
    rekey_old: R,
) -> Result<(), String>
where
    O: Fn(String, Vec<u8>) -> Result<(), String>,
    R: FnOnce(Vec<u8>) -> Result<(), String>,
{
    if opens_under(path.to_owned(), new_key.to_vec()).is_ok() {
        return Ok(());
    }
    if !std::path::Path::new(path).exists() {
        tracing::debug!(label, "rotation resume: store file absent, recreating");
    }
    rekey_old(old_key.to_vec()).map_err(|e| format!("{label}: {e}"))?;
    opens_under(path.to_owned(), new_key.to_vec()).map_err(|e| format!("{label} verify: {e}"))?;
    Ok(())
}

/// Outcome of one persistent-store open attempt. `reasons` is empty on
/// full success; any entry means the boot gate must block.
struct StoreOutcome {
    vault: Option<std::sync::Arc<ferrite_vault::Vault>>,
    archive: Option<std::sync::Arc<ferrite_archive::Archive>>,
    reasons: Vec<String>,
}

/// Open vault plus archive under one resolved key. Self-contained for
/// boot and for Retry (fresh keyring read each attempt). Takes ownership
/// of the key and zeroizes it before return; the vault keeps its own
/// clone under its Drop discipline, the archive only borrows.
fn open_persistent_stores(
    master_key: Option<Vec<u8>>,
    key_durable: bool,
    key_reason: Option<String>,
) -> StoreOutcome {
    let mut reasons: Vec<String> = key_reason.into_iter().collect();
    let (vault, archive) = match (master_key, key_durable) {
        (Some(key), true) => {
            let (v, vn) = open_named_vault(key.clone());
            reasons.extend(vn);
            let profile = crate::profiles::Profile::default_profile();
            let cfg = ferrite_archive::ArchiveConfig {
                path: crate::profiles::archive_path(&profile)
                    .to_string_lossy()
                    .into_owned(),
                ..Default::default()
            };
            let (a, an) = match ferrite_archive::Archive::open(cfg, &key) {
                Ok(store) => (Some(std::sync::Arc::new(store)), None),
                Err(e) => {
                    error!(error = %e, "archive unavailable: offline snapshot fallback disabled");
                    (None, Some(archive_degraded_notice(&e.to_string())))
                }
            };
            reasons.extend(an);
            // The working copy served its purpose (vault cloned what it
            // keeps and zeroizes on drop; archive borrowed). Scrub it.
            let mut owned = key;
            owned.zeroize();
            (v, a)
        }
        _ => (None, None),
    };
    // A missing vault or archive with no reason yet means the key itself
    // was unusable (keyring failure, already warned above): the gate still
    // needs words, so say the plain truth.
    if vault.is_none() && !reasons.iter().any(|r| r.contains("Vault")) {
        reasons.push(
            "Vault unavailable: the device key couldn't be read, so nothing may be persisted this session."
                .to_owned(),
        );
    }
    if archive.is_none() && !reasons.iter().any(|r| r.contains("Archive")) {
        reasons.push(
            "Archive unavailable: visited pages can't be kept offline this session.".to_owned(),
        );
    }
    StoreOutcome {
        vault,
        archive,
        reasons,
    }
}

/// Open the file vault, falling back to in-memory (session-only imports).
/// Default profile reuses the legacy `ferrite.db` (zero-migration). Other
/// profiles get their own file via [`crate::profiles::vault_path`].
/// Returns the vault plus, when the file failed and memory took over, a
/// plain-language notice for one user-visible toast (the log keeps the
/// technical error; a silent persistence loss is the thing to avoid).
fn open_named_vault(
    key: Vec<u8>,
) -> (Option<std::sync::Arc<ferrite_vault::Vault>>, Option<String>) {
    let path = crate::profiles::vault_path(&crate::profiles::Profile::default_profile())
        .to_string_lossy()
        .into_owned();
    match ferrite_vault::Vault::open(ferrite_vault::VaultConfig::new(path, key)) {
        Ok(v) => (Some(std::sync::Arc::new(v)), None),
        Err(e) => {
            warn!(error = %e, "file vault failed, trying in-memory");
            match ferrite_vault::Vault::open_in_memory() {
                Ok(v) => {
                    warn!("vault running in-memory: imports last for this session only");
                    (
                        Some(std::sync::Arc::new(v)),
                        Some(vault_degraded_notice(&e.to_string())),
                    )
                }
                Err(e) => {
                    warn!(error = %e, "vault unavailable: import/export disabled");
                    (None, None)
                }
            }
        }
    }
}

/// Plain-language vault fallback notice. The log keeps paths and error
/// shapes; the toast must not leak either, and must say what the user
/// loses. Pure for tests.
fn vault_degraded_notice(err: &str) -> String {
    let kind = if err.contains("legacy") {
        "the vault file is from an older version and can't be opened"
    } else if err.contains("sqlcipher") {
        "this build can't open the encrypted vault (needs the SQLCipher build)"
    } else {
        "the vault file couldn't be opened"
    };
    format!("Vault couldn't open: {kind}. History and passwords won't be saved this session.")
}

/// Plain-language archive fallback notice. Same disclosure discipline as
/// the vault one: what broke, what the user loses, no paths.
fn archive_degraded_notice(err: &str) -> String {
    let kind = if err.contains("legacy") {
        "the archive file is from an older version and can't be opened"
    } else if err.contains("sqlcipher") {
        "this build can't open the encrypted archive (needs the SQLCipher build)"
    } else {
        "the archive file couldn't be opened"
    };
    format!("Archive unavailable: {kind}. Visited pages won't be kept offline.")
}

/// Dialog-free import core behind the settings-page and welcome-page
/// buttons. Kinds: `bookmarks` (Chromium JSON, Firefox places JSON,
/// Safari plist, or Netscape HTML, sniffed/dispatched), `history`
/// (Chrome then Firefox), `safari-history`, `safari-bookmarks`. Returns
/// exact counts; failures are errors, never silent zeros.
pub(crate) fn import_from_path(
    vault: &ferrite_vault::Vault,
    kind: &str,
    path: &std::path::Path,
) -> ferrite_vault::VaultResult<ferrite_vault::import::ImportReport> {
    use ferrite_vault::import as vault_import;
    let p = path.to_string_lossy().into_owned();
    match kind {
        "bookmarks" => vault_import::import_bookmarks_file(&p, vault),
        "safari-bookmarks" => vault_import::import_safari_plist(&p, vault),
        "history" => vault_import::import_chrome_history(&p, vault)
            .or_else(|_| vault_import::import_firefox_history(&p, vault)),
        "safari-history" => vault_import::import_safari_history(&p, vault),
        other => Err(ferrite_vault::VaultError::Import(format!(
            "unknown import kind: {other}"
        ))),
    }
}

/// One scanner-reported field (viewport coords, autocomplete attr).
#[derive(Debug, Clone, Default)]
struct ScanField {
    kind: String,
    name: String,
    id: String,
    ac: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// One imported password colliding with a stored entry (same origin and
/// username, different password). Queued at import, resolved on the
/// passwords page. Plaintext held only until resolved, zeroized on drop.
struct PendingPasswordConflict {
    origin: String,
    username: String,
    password: String,
}

impl Drop for PendingPasswordConflict {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// Restore tallies (reported in one honest toast, never markup).
#[derive(Debug, Default)]
struct RestoreCounts {
    bookmarks: u64,
    history: u64,
    passwords: u64,
    skipped: u64,
    conflicts: u64,
    cards: u64,
    addresses: u64,
    settings: bool,
}

/// One pending fill offer (built at menu open, consumed at pick).
#[derive(Debug, Clone)]
struct PendingFill {
    record_id: String,
    comp: String,
    selector: String,
    tab: Uuid,
}

/// One pending credential pair fill (username plus password together).
/// Either selector may be missing (unidentifiable sibling). The runner
/// fills what is there rather than guessing targets.
#[derive(Debug, Clone)]
struct PendingCredFill {
    cred_id: String,
    origin: String,
    user_selector: Option<String>,
    pass_selector: String,
    tab: Uuid,
}

/// One pending form-history fill (plain value, no record).
#[derive(Debug, Clone)]
struct PendingFormFill {
    selector: String,
    value: String,
    tab: Uuid,
}

/// One pending TOTP code fill (code generated at pick time, never
/// stored, a stale code is just rejected by the site).
#[derive(Debug, Clone)]
struct PendingTotpFill {
    cred_id: String,
    origin: String,
    selector: String,
    tab: Uuid,
}

/// One reader article (sanitized HTML only. Never raw page content).
#[derive(Debug, Clone, Default)]
struct ReaderArticle {
    title: String,
    byline: String,
    site: String,
    minutes: u32,
    html: String,
}

/// Reassembly for chunked reader bodies, per extracting tab.
#[derive(Debug, Clone, Default)]
struct ReaderAssembly {
    title: String,
    byline: String,
    site: String,
    length: usize,
    total: usize,
    src: String,
    parts: Vec<Option<String>>,
}

/// Content right-click payload: viewport CSS px plus engine-resolved targets.
/// Stored while the overlay menu is open. Actions run against this, never
/// against later page-supplied data.
#[derive(Debug, Clone, Default)]
struct CtxPayload {
    tab: Uuid,
    x: f64,
    y: f64,
    href: String,
    src: String,
    selection: String,
    /// Right-click landed in an editable field. The native text-edit menu
    /// owns that surface (spell suggestions, Add to Dictionary). Ferrite
    /// rows stand down except for vault fills and multi-word search.
    editable: bool,
}

/// Flip an overlay above its anchor when it would run off the bottom
/// edge and there is room above. Otherwise keep it below (the clamp in
/// `ctx_overlay_rect` handles the hopeless case). Pure geometry for tests.
fn flip_y(y_below: f64, field_top: f64, h: f64, top: f64, win_h: f64) -> f64 {
    if y_below + h > win_h && field_top - h >= top {
        field_top - h
    } else {
        y_below
    }
}

/// Dedup key for imported addresses: name+street+postal+country,
/// lowercased and whitespace-folded. Email/phone excluded (people reuse
/// them across moves; the address is the identity).
fn address_dedupe_key(name: &str, street: &str, postal: &str, country: &str) -> String {
    [name, street, postal, country]
        .into_iter()
        .map(|s| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Map a Chromium address row onto the address book shape. Pure for
/// tests; the import loop owns saving and dedup.
fn address_fields_from_chrome(
    a: &ferrite_vault::import::ChromeAddress,
) -> ferrite_credentials::AddressFields {
    ferrite_credentials::AddressFields {
        label: if a.name.trim().is_empty() {
            "Imported address".to_owned()
        } else {
            format!("Imported: {}", a.name.trim())
        },
        name: a.name.clone(),
        street: a.street.clone(),
        city: a.city.clone(),
        region: a.region.clone(),
        postal: a.postal.clone(),
        country: a.country.clone(),
        phone: a.phone.clone(),
        email: a.email.clone(),
    }
}

/// Percent-encode a text-fragment snippet (unreserved set stays bare,
/// everything else `%XX`, spaces included). Pure for tests.
fn fragment_encode(s: &str) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if UNRESERVED.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Discrete zoom presets (percent). Steps move through this table, not by
/// fixed arithmetic. Linear steps feel wrong at both extremes.
const ZOOM_PRESETS: &[u32] = &[
    25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500,
];

/// Next preset from `current_pct` in `dir` (plus 1 or minus 1), clamped at the ends.
/// Pure for tests.
fn zoom_step(current_pct: u32, dir: i64) -> u32 {
    if dir >= 0 {
        ZOOM_PRESETS
            .iter()
            .find(|p| **p > current_pct)
            .copied()
            .unwrap_or(500)
    } else {
        ZOOM_PRESETS
            .iter()
            .rev()
            .find(|p| **p < current_pct)
            .copied()
            .unwrap_or(25)
    }
}

/// Save-As targets: http(s) fetches only. data and blob URLs route to
/// their own paths (local decode, page-side rescue). file reads are a
/// separate policy. Both stay out of the generic fetcher.
fn saveable_url(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Capped string field from a content IPC object (menu payloads).
fn capped_field(v: &serde_json::Value, key: &str, cap: usize) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .chars()
        .take(cap)
        .collect()
}

/// Minimal percent-encoding for query values (unreserved kept bare).
fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Classify a child process from its argv (`--type=` renderer, gpu, or utility
/// on Chromium, WebKit names otherwise). Unknown stays unknown. The page
/// says so rather than guessing wrong.
fn classify_child(cmd: &str) -> &'static str {
    if cmd.contains("--type=renderer") {
        "renderer"
    } else if cmd.contains("--type=gpu") {
        "gpu"
    } else if cmd.contains("--type=utility") {
        "utility"
    } else if cmd.contains("--type=") {
        "helper"
    } else {
        "unknown"
    }
}

/// Import-source probe result for the welcome page (histories, Chromium
/// bookmark files, Netscape HTML for bookmarks, CSV for passwords, each
/// labeled honestly in the UI).
#[derive(Debug, Clone)]
struct ImportSource {
    id: &'static str,
    label: &'static str,
    path: String,
}

/// Detect other browser data files under explicit base dirs.
/// ponytail: Chrome, Edge, and Firefox only (no Safari backend, no keychain
/// reads). Split out for tests, the real wrapper reads OS env below.
fn detect_browsers_in(localdata: &str, appdata: &str, home: &str) -> Vec<ImportSource> {
    let mut out: Vec<ImportSource> = Vec::new();
    let join = |base: &str, parts: &[&str]| {
        let mut p = std::path::PathBuf::from(base);
        for part in parts {
            p.push(part);
        }
        p.to_string_lossy().into_owned()
    };
    for (id, label, candidates) in [
        (
            "chrome",
            "Chrome history",
            vec![
                join(
                    localdata,
                    &["Google", "Chrome", "User Data", "Default", "History"],
                ),
                join(
                    home,
                    &[
                        "Library",
                        "Application Support",
                        "Google",
                        "Chrome",
                        "Default",
                        "History",
                    ],
                ),
                join(home, &[".config", "google-chrome", "Default", "History"]),
            ],
        ),
        (
            "edge",
            "Edge history",
            vec![
                join(
                    localdata,
                    &["Microsoft", "Edge", "User Data", "Default", "History"],
                ),
                join(
                    home,
                    &[
                        "Library",
                        "Application Support",
                        "Microsoft Edge",
                        "Default",
                        "History",
                    ],
                ),
                join(home, &[".config", "microsoft-edge", "Default", "History"]),
            ],
        ),
    ] {
        if let Some(p) = candidates
            .iter()
            .find(|p| std::path::Path::new(p).is_file())
        {
            out.push(ImportSource {
                id,
                label,
                path: p.clone(),
            });
        }
    }
    // Chromium `Bookmarks` files (JSON, not SQLite. that is only the
    // `History` file). Same profile dirs, `Bookmarks` leaf instead.
    for (id, label, candidates) in [
        (
            "chrome-bookmarks",
            "Chrome bookmarks",
            vec![
                join(
                    localdata,
                    &["Google", "Chrome", "User Data", "Default", "Bookmarks"],
                ),
                join(
                    home,
                    &[
                        "Library",
                        "Application Support",
                        "Google",
                        "Chrome",
                        "Default",
                        "Bookmarks",
                    ],
                ),
                join(home, &[".config", "google-chrome", "Default", "Bookmarks"]),
            ],
        ),
        (
            "edge-bookmarks",
            "Edge bookmarks",
            vec![
                join(
                    localdata,
                    &["Microsoft", "Edge", "User Data", "Default", "Bookmarks"],
                ),
                join(
                    home,
                    &[
                        "Library",
                        "Application Support",
                        "Microsoft Edge",
                        "Default",
                        "Bookmarks",
                    ],
                ),
                join(home, &[".config", "microsoft-edge", "Default", "Bookmarks"]),
            ],
        ),
    ] {
        if let Some(p) = candidates
            .iter()
            .find(|p| std::path::Path::new(p).is_file())
        {
            out.push(ImportSource {
                id,
                label,
                path: p.clone(),
            });
        }
    }
    // Firefox profiles live one level down. Scan, don't guess names.
    for base in [
        join(appdata, &["Mozilla", "Firefox", "Profiles"]),
        join(
            home,
            &["Library", "Application Support", "Firefox", "Profiles"],
        ),
        join(home, &[".mozilla", "firefox"]),
    ] {
        if let Ok(dir) = std::fs::read_dir(&base) {
            let mut found: Vec<String> = dir
                .flatten()
                .map(|e| {
                    e.path()
                        .join("places.sqlite")
                        .to_string_lossy()
                        .into_owned()
                })
                .filter(|p| std::path::Path::new(p).is_file())
                .collect();
            found.sort();
            if let Some(p) = found.into_iter().next() {
                out.push(ImportSource {
                    id: "firefox",
                    label: "Firefox history",
                    path: p,
                });
                break;
            }
        }
    }
    // Safari (macOS): History.db + Bookmarks.plist under ~/Library/Safari.
    // Paths only exist there; elsewhere the file checks simply miss.
    let safari = join(home, &["Library", "Safari"]);
    for (id, label, leaf) in [
        ("safari-history", "Safari history", "History.db"),
        ("safari-bookmarks", "Safari bookmarks", "Bookmarks.plist"),
    ] {
        let p = join(&safari, &[leaf]);
        if std::path::Path::new(&p).is_file() {
            out.push(ImportSource { id, label, path: p });
        }
    }
    // Chromium `Web Data` (site searches + addresses): same profile dirs
    // as History, `Web Data` leaf instead.
    for (id, label, candidates) in [
        (
            "chrome-engines",
            "Chrome search engines",
            chromium_profile_file(
                localdata,
                home,
                &["Google", "Chrome"],
                &["Google", "Chrome"],
                "google-chrome",
                "Web Data",
            ),
        ),
        (
            "chrome-addresses",
            "Chrome addresses",
            chromium_profile_file(
                localdata,
                home,
                &["Google", "Chrome"],
                &["Google", "Chrome"],
                "google-chrome",
                "Web Data",
            ),
        ),
        (
            "edge-engines",
            "Edge search engines",
            chromium_profile_file(
                localdata,
                home,
                &["Microsoft", "Edge"],
                &["Microsoft Edge"],
                "microsoft-edge",
                "Web Data",
            ),
        ),
        (
            "edge-addresses",
            "Edge addresses",
            chromium_profile_file(
                localdata,
                home,
                &["Microsoft", "Edge"],
                &["Microsoft Edge"],
                "microsoft-edge",
                "Web Data",
            ),
        ),
    ] {
        if let Some(p) = candidates
            .iter()
            .find(|p| std::path::Path::new(p).is_file())
        {
            out.push(ImportSource {
                id,
                label,
                path: p.clone(),
            });
        }
    }
    out
}

/// Chromium profile file across OSes: Windows vendor path, macOS
/// `~/Library/Application Support`, Linux `~/.config`. `win`/`mac` are
/// the vendor subpaths, `linux` the config dir, `leaf` the file.
fn chromium_profile_file(
    localdata: &str,
    home: &str,
    win: &[&str],
    mac: &[&str],
    linux: &str,
    leaf: &str,
) -> Vec<String> {
    let mut out = Vec::with_capacity(3);
    let mut w: Vec<&str> = win.to_vec();
    w.extend_from_slice(&["User Data", "Default", leaf]);
    let mut p = std::path::PathBuf::from(localdata);
    for part in &w {
        p.push(part);
    }
    out.push(p.to_string_lossy().into_owned());
    let mut m = std::path::PathBuf::from(home);
    m.push("Library");
    m.push("Application Support");
    for part in mac {
        m.push(part);
    }
    m.push("Default");
    m.push(leaf);
    out.push(m.to_string_lossy().into_owned());
    let mut l = std::path::PathBuf::from(home);
    l.push(".config");
    l.push(linux);
    l.push("Default");
    l.push(leaf);
    out.push(l.to_string_lossy().into_owned());
    out
}

/// OS default browser, best effort: any failure (or unknown value)
/// yields None and the welcome list stays unordered. Read-only OS
/// queries; nothing is changed. Pure parsers below are unit-tested;
/// the runners just shell out to platform tools.
fn default_browser_id() -> Option<&'static str> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice",
                "/v",
                "ProgId",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        parse_windows_prog_id(&text).and_then(|t| browser_id_for_token(&t))
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("defaults")
            .args([
                "read",
                "com.apple.LaunchServices/com.apple.launchservices.secure",
                "LSHandlers",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        parse_macos_handlers(&text).and_then(|t| browser_id_for_token(&t))
    }
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("xdg-settings")
            .args(["get", "default-web-browser"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        browser_id_for_token(text.trim())
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Map an OS default-browser token to a welcome source id.
fn browser_id_for_token(token: &str) -> Option<&'static str> {
    let t = token.to_lowercase();
    if t.contains("chrome") {
        Some("chrome")
    } else if t.contains("msedge") || t.contains("microsoft-edge") || t.contains("microsoft edge") {
        Some("edge")
    } else if t.contains("firefox") {
        Some("firefox")
    } else if t.contains("safari") {
        Some("safari")
    } else {
        None
    }
}

/// `reg query ... /v ProgId` output: the data value on the ProgId line.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_windows_prog_id(output: &str) -> Option<String> {
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("ProgId") && parts.next() == Some("REG_SZ") {
            return parts.next().map(str::to_owned);
        }
    }
    None
}

/// `defaults read ... LSHandlers` output: the RoleAll bundle id of the
/// first http/https handler block. Line-scanned (not a plist parse):
/// anything unexpected yields None.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_macos_handlers(output: &str) -> Option<String> {
    let mut scheme: Option<String> = None;
    let mut role: Option<String> = None;
    let mut found: Option<String> = None;
    for line in output.lines().map(str::trim) {
        if line == "{" {
            scheme = None;
            role = None;
        } else if line == "}," || line == "}" {
            if let (Some(s), Some(r)) = (scheme.take(), role.take()) {
                if (s == "http" || s == "https") && found.is_none() {
                    found = Some(r);
                }
            }
        } else if let Some(v) = line
            .strip_prefix("LSHandlerURLScheme =")
            .map(|v| v.trim().trim_matches(['"', ';', ' ']).to_owned())
        {
            scheme = Some(v);
        } else if let Some(v) = line
            .strip_prefix("LSHandlerRoleAll =")
            .map(|v| v.trim().trim_matches(['"', ';', ' ']).to_owned())
        {
            role = Some(v);
        }
    }
    found
}

/// OS base dirs for detection (env only, no new deps for this).
fn detect_browsers() -> Vec<ImportSource> {
    #[cfg(windows)]
    let (localdata, appdata, home) = (
        std::env::var("LOCALAPPDATA").unwrap_or_default(),
        std::env::var("APPDATA").unwrap_or_default(),
        std::env::var("USERPROFILE").unwrap_or_default(),
    );
    #[cfg(not(windows))]
    let (localdata, appdata, home) = (
        String::new(),
        String::new(),
        std::env::var("HOME").unwrap_or_default(),
    );
    detect_browsers_in(&localdata, &appdata, &home)
}

/// Naive host slice for display and filter (engine-resolved hrefs are already
/// IDN-safe. This never validates, only feeds substring matches).
fn host_slice(href: &str) -> &str {
    let s = href.split("://").nth(1).unwrap_or(href);
    let s = s.rsplit('@').next().unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or(s);
    if let Some(end) = s.strip_prefix('[') {
        // Bracketed IPv6: keep through `]`, drop any port.
        return match end.find(']') {
            Some(i) => &s[..i + 2],
            None => s,
        };
    }
    s.split(':').next().unwrap_or(s)
}

/// Message types an unprivileged (non-`ferrite://`) page may post.
/// Pure for tests. Every admitted type is validated again inside its
/// own arm. The allowlist is breadth, per-arm checks are depth.
fn content_page_allowed(msg_type: &str) -> bool {
    matches!(
        msg_type,
        // Telemetry + overlay UIs (pre-existing).
        "audio"
            | "pip"
            | "pip_error"
            | "share_meta"
            | "link_hover"
            | "link_leave"
            | "find_result"
            | "context_menu"
            | "autofill_scan"
            | "console_error"
            | "reader_result"
            | "reader_chunk"
            | "title"
            | "nav"
            | ""
            | "palette"
            | "ctx_activate"
            | "ctx_dismiss"
            // Form/autofill flows: focus opens the vault-gated dropdown,
            // af_* only steer an open dropdown, dismiss closes menus.
            | "autofill_focus"
            | "autofill_dismiss"
            | "af_nav"
            | "af_pick"
            | "af_filter"
            // Form history + password-save candidate: validated + gated
            // (sensitive keys, digit runs, store-exists, user confirm).
            | "form_history_save"
            | "password_save_candidate"
            // Cosmetic + risk reporters: read-only lookups, same-tab
            // answers, TTL-bounded engine state.
            | "cosmetic_query"
            | "form_risk"
            // Lifecycle reporters: form-dirty latches and call liveness
            // feed the sleep exemptions (a tab holding unsaved work or a
            // live call never sleeps on its own). Booleans only.
            | "form_dirty"
            | "live_call"
            // Read-only reporters with validated arms: translation and
            // media-monitors post progress/failure states only (field
            // caps, active-tab gating, toasts, no mutations).
            | "translate_state"
            | "media_drm_blocked"
            // Page-language + scroll reporters: validated tag / finite
            // clamped numbers, stored on the tab, never executed.
            | "lang"
            | "scroll"
            // Icon-link reporter: absolute icon URL only (validated,
            // fetched Rust-side, applied with staleness checks).
            | "favicon"
            // Boot-gate decision screen: reasons request plus the three
            // choices plus phrase recovery. Arms re-validate the decision
            // tab and active block.
            | "bootdecision_data"
            | "bootdecision"
            | "bootrecovery"
    )
}

/// Clean relay URL input (settings field or boot read): comma,
/// whitespace, or newline separated; `ws://`/`wss://` only, 256 chars
/// each, max 8, deduped, order kept. Anything else is refused, never
/// truncated or repaired (a mangled URL would dial the wrong host).
fn clean_relay_urls(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split([',', '\n', '\r', ' ', '\t']) {
        let u = part.trim();
        if u.is_empty() || u.len() > 256 {
            continue;
        }
        let lower = u.to_ascii_lowercase();
        if !(lower.starts_with("ws://") || lower.starts_with("wss://")) {
            continue;
        }
        if !out.iter().any(|x| x == u) {
            out.push(u.to_owned());
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

/// Singleton URL match: the bare page or any of its fragments. Never a
/// longer page that merely shares the prefix (`settings2`, `settingsx`).
fn singleton_match(url: &str, base: &str) -> bool {
    url == base || url.starts_with(&format!("{base}#"))
}

/// Frecency score for one history row as a top-sites candidate. One
/// visit scores roughly one hour of recency: dailies outrank one-offs
/// from minutes ago, while month-old giants fade below fresh regulars.
/// Pure for tests; the tile ordering lives or dies here, not in the push.
fn tile_score(visit_count: i32, last_visit_ms: i64) -> i64 {
    visit_count as i64 * 3_600_000 + last_visit_ms
}

/// Manager view for one bookmarks tab: selected folder plus search text.
/// Stored so cross-surface mutations can re-push the right slice.
#[derive(Debug, Clone, Default)]
struct BmView {
    folder: Option<String>,
    query: String,
}

/// Deep link for a settings catalogue entry through the page own fragment
/// routing (`#privacy` to `#cat-privacy`). Autofill has no short link and
/// uses the direct anchor. About entries point at the about page.
fn palette_setting_url(s: &ferrite_tabs::protocol::SettingDef) -> String {
    if s.category == "about" {
        return "ferrite://about".to_owned();
    }
    let frag = match s.category {
        "autofill" => format!("cat-{}", s.category),
        other => other.to_owned(),
    };
    format!("ferrite://settings#{frag}")
}

/// Same mapping by catalogue id (the palette posts ids, not indices).
fn palette_setting_url_by_id(id: &str) -> String {
    ferrite_tabs::protocol::SETTINGS_CATALOGUE
        .iter()
        .find(|s| s.id == id)
        .map(palette_setting_url)
        .unwrap_or_else(|| "ferrite://settings".to_owned())
}

/// Persist one setting into the sealed settings file (JSON-merge so
/// privacy toggles are preserved; encryption plus atomic replace live in
/// the settings module). Keyless boots cannot persist: values stay in
/// memory and the next persist retries. Refused outright in ephemeral
/// (guest) mode: nothing may be written, even encrypted.
fn persist_setting(key: &str, value: serde_json::Value) {
    if is_ephemeral() {
        tracing::debug!(key, "setting persist refused: ephemeral session");
        return;
    }
    let mut keyring = ShellApp::cred_key();
    let mut v = crate::settings_file::read_settings_value(keyring.as_deref());
    if let Some(obj) = v.as_object_mut() {
        obj.insert(key.to_owned(), value);
    }
    let ok = match keyring.as_deref() {
        Some(k) => crate::settings_file::write_settings_value(&v, k),
        None => false,
    };
    if let Some(k) = keyring.as_mut() {
        k.zeroize();
    }
    if !ok {
        tracing::debug!(key, "setting persist deferred: no device key");
    }
}

/// Delete one setting key from the sealed settings file (migration
/// cleanup). Same key and ephemeral discipline as the writer.
fn clear_setting(key: &str) {
    if is_ephemeral() {
        tracing::debug!(key, "setting clear refused: ephemeral session");
        return;
    }
    let mut keyring = ShellApp::cred_key();
    let mut v = crate::settings_file::read_settings_value(keyring.as_deref());
    let removed = v
        .as_object_mut()
        .is_some_and(|obj| obj.remove(key).is_some());
    if removed {
        let ok = match keyring.as_deref() {
            Some(k) => crate::settings_file::write_settings_value(&v, k),
            None => false,
        };
        if !ok {
            tracing::debug!(key, "setting clear deferred: no device key");
        }
    }
    if let Some(k) = keyring.as_mut() {
        k.zeroize();
    }
}
/// Persist the selected search engine id.
fn persist_omnibox_engine(engine_id: &str) {
    persist_setting(
        "omnibox_engine",
        serde_json::Value::String(engine_id.to_owned()),
    );
}
/// Merge an imported settings object over the stored document: present
/// keys win, absent keys keep their stored values. Pure for tests. Both
/// sides must be objects; anything else leaves `doc` untouched.
fn merge_settings_doc(doc: &mut serde_json::Value, incoming: &serde_json::Value) {
    if let (Some(dst), Some(src)) = (doc.as_object_mut(), incoming.as_object()) {
        for (key, val) in src {
            dst.insert(key.clone(), val.clone());
        }
    }
}

/// Escape a push payload for `evaluate_script` transport: neutralizes
/// `</script` in any case so JSON can never break out of a script
/// context, while leaving server-rendered SVG (`</svg>`) intact for
/// `innerHTML` rendering. Pure for tests. Byte-safe over UTF-8
/// (`to_ascii_lowercase` preserves byte length, indexes advance by chars).
fn escape_script_close(s: &str) -> String {
    let lowered = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if lowered[i..].starts_with("</script") {
            out.push_str("<\\/");
            out.push_str(&s[i + 2..i + 8]);
            i += 8;
        } else {
            let ch = s[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Build the `__ferriteSyncCode` push payload: the pairing code plus the
/// same code as server-rendered SVG (rects only, via the share-page QR
/// path: no new dependency, encoded on this device). `qr` is null when
/// absent or unencodable; the text code always carries the pairing either
/// way. `expires_at_ms` rides along so the page can count the code down
/// and retire it without another push. Pure for tests.
fn sync_code_payload(code: Option<&str>, expires_at_ms: Option<i64>) -> String {
    let qr = code.and_then(|c| ferrite_tabs::share::qr_svg(c).ok());
    let payload = serde_json::json!({ "code": code, "qr": qr, "expires_at_ms": expires_at_ms });
    escape_script_close(&serde_json::to_string(&payload).unwrap_or_default())
}

/// Second-instance guard. Holds an exclusive OS file lock beside the
/// stores for process lifetime (returned guard must be kept alive by the
/// caller). Retries ~3s with backoff so a boot-gate relaunch racing its
/// exiting parent still acquires; a live holder after that is a genuine
/// second instance. `None` on any failure to even create the file.
fn acquire_instance_lock(name: &std::path::Path) -> Option<std::fs::File> {
    use fs2::FileExt;
    if let Some(parent) = name.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(name)
        .ok()?;
    for _ in 0..30 {
        match file.try_lock_exclusive() {
            Ok(()) => return Some(file),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    None
}

/// Second-instance dialog: plain text, no new instance. Shown only when
/// there was nothing forwardable (or the handover itself failed): a
/// forwarded URL exits silently with a log line instead.
fn single_instance_dialog() {
    tracing::warn!("second instance refused: Ferrite is already running");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title("Ferrite is already running")
        .set_description(
            "Another Ferrite window is already open.\n\nUse the running window instead. \
             A second instance would fight it over your data files, so this one is exiting.",
        )
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

pub fn run(config: ShellConfig) -> ShellResult<()> {
    // Self-update boot: provision the release root (operator env step;
    // without it every check fails closed), then apply any verified staged
    // artifact BEFORE the shell starts (the running exe is file-locked on
    // Windows, so runtime swaps are impossible by construction).
    if let Ok(exe) = std::env::current_exe() {
        match crate::update::provision_root_from_env() {
            Ok(true) => {
                if let Some(v) = crate::update::apply_staged_at_boot(&exe) {
                    tracing::info!(version = %v, "staged update applied at boot");
                    // Marker for the next boot's banner: from = this
                    // (old) binary's version, to = the staged one.
                    crate::update::note_updated_at_boot(env!("CARGO_PKG_VERSION"), &v);
                }
            }
            Ok(false) => {
                tracing::warn!("no release root provisioned - update checks fail closed")
            }
            Err(e) => tracing::warn!(error = %e, "release root provisioning failed"),
        }
    }
    // Data root first: every persistent file below lives under it. One-time
    // migration moves pre-data-root launch-directory files (byte-verified,
    // originals renamed aside, never deleted) before anything reads them.
    for moved in crate::profiles::migrate_cwd_to_data_root() {
        tracing::info!(file = %moved, "migrated launch-directory file to data root");
    }
    // Single instance: one browser per profile per data dir. The lock lives
    // inside the data root (same model as every Ferrite file) and releases
    // with the process, so crashes never wedge it. Retry-with-backoff covers
    // boot-gate relaunch racing the exiting parent; a live holder after that
    // means a real second instance. Its URLs are handed to the live window
    // over the loopback forwarder when possible (same mode AND profile,
    // token-authed); otherwise it exits with a dialog. Incognito windows
    // take their own lock: they share no stores with the main window, so
    // they may run alongside it. A second incognito still exits.
    let profile_id = config
        .profile
        .as_deref()
        .and_then(crate::profiles::sanitize)
        .unwrap_or_else(|| crate::profiles::DEFAULT_ID.to_owned());
    crate::profiles::set_process_profile(&profile_id);
    let lock_name = crate::profiles::instance_lock_path(config.incognito, &profile_id);
    let _instance = match acquire_instance_lock(&lock_name) {
        Some(guard) => guard,
        None => {
            let handover: Vec<String> = config.initial_url.clone().into_iter().collect();
            if crate::forward::try_forward_to_live(config.incognito, &profile_id, &handover) {
                tracing::info!("second instance: URL handed to running window");
                return Ok(());
            }
            single_instance_dialog();
            return Ok(());
        }
    };
    let event_loop = EventLoop::<ShellWake>::with_user_event().build()?;
    let wake = event_loop.create_proxy();
    let forward_queue = crate::forward::ForwardQueue::default();
    crate::forward::start_forward_server(
        wake.clone(),
        &forward_queue,
        config.incognito,
        &profile_id,
    );
    let mut app = ShellApp::new(config, Some(wake), forward_queue);
    // Live sync starts with the window (bind + advertise + browse, then an
    // immediate round): refused platforms run transport-off, logged, with
    // the trust UI unaffected.
    app.start_sync_engine();
    event_loop.run_app(&mut app)?;
    // Rollback relaunch: the restore page asked to restart into the
    // previous release (marker written by the rollback action, loop
    // exited from about_to_wait). Spawn the restored binary exactly once.
    if let Some(version) = crate::update::take_relaunch_marker(&crate::profiles::data_root()) {
        tracing::info!(version = %version, "relaunching after rollback");
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(&exe).spawn();
        }
    }
    Ok(())
}

#[cfg(test)]
mod frame_tests {
    use super::hex_to_colorref;

    #[test]
    fn hex_to_colorref_swaps_rgb_to_bgr() {
        assert_eq!(hex_to_colorref("#ff8c00"), Some(0x008cff));
        assert_eq!(hex_to_colorref("#000000"), Some(0x000000));
        assert_eq!(hex_to_colorref("#ffffff"), Some(0xffffff));
        assert_eq!(hex_to_colorref("nope"), None);
        assert_eq!(hex_to_colorref("#fff"), None);
        assert_eq!(hex_to_colorref("#gggggg"), None);
    }
}

#[cfg(test)]
mod settings_apply_tests {
    use super::*;

    fn test_app() -> ShellApp {
        ShellApp::new(
            ShellConfig::default(),
            None,
            crate::forward::ForwardQueue::default(),
        )
    }

    #[test]
    fn merge_overwrites_present_and_keeps_absent() {
        let mut doc = serde_json::json!({"a": 1, "b": 2});
        merge_settings_doc(&mut doc, &serde_json::json!({"b": 3, "c": 4}));
        assert_eq!(doc, serde_json::json!({"a": 1, "b": 3, "c": 4}));
        // Non-objects never clobber: a partial file cannot wipe prefs.
        merge_settings_doc(&mut doc, &serde_json::json!([1, 2]));
        assert_eq!(doc, serde_json::json!({"a": 1, "b": 3, "c": 4}));
        let mut scalar = serde_json::json!(1);
        merge_settings_doc(&mut scalar, &serde_json::json!({"a": 9}));
        assert_eq!(scalar, serde_json::json!(1));
    }

    #[test]
    fn apply_doc_takes_effect_live_without_disk() {
        let mut app = test_app();
        // Diverge engine state first: a partial doc must not clobber it.
        app.privacy.update_settings(|s| s.adblock_enabled = false);
        app.apply_settings_doc(&serde_json::json!({"chrome_pack": "tenebra"}));
        assert!(
            !app.privacy.settings().adblock_enabled,
            "absent keys keep live values"
        );
        app.apply_settings_doc(&serde_json::json!({
            "chrome_pack": "tenebra",
            "home_url": "https://example.com",
            "suggestions": false,
            "suggestions_remote": true,
            "restore_always": true,
            "startup_mode": "urls",
            "startup_urls": ["https://example.com", "not a url"],
            "profile_picker": false,
            "history_retention_days": 30,
            "tabs_vertical": true,
            "sleep_timeout_min": 5,
            "sleep_exceptions": ["example.com", "not a host!!"],
            "onboarding_completed": true,
            "viewsource_wrap": true,
            "autofill_address": false,
            "autofill_payment": false,
            "autofill_form": true,
            "translate_enabled": false,
            "translate_target": "de",
            "reader_size": 120,
            "reader_serif": true,
            "reader_wide": true,
            "reader_theme": "sepia",
            "bar_mode": "always",
            "bar_seen": true,
            "panel_open": true,
            "panel_section": "history",
            "panel_w": 400,
            "ask_downloads": true,
            "pip_auto": true,
            "notifications_global": false,
            "save_last_dir": "/tmp/ferrite-test",
            "omnibox_engine": "duckduckgo",
            "adblock_enabled": true,
            "gpc_enabled": false,
            "doh_endpoint": "https://dns.example/dns-query",
        }));
        assert_eq!(app.theme_pack, ThemePack::Tenebra);
        assert_eq!(app.home_url, "https://example.com");
        assert!(!app.suggestions_enabled);
        assert!(app.remote_enabled);
        assert!(app.restore_always);
        assert_eq!(app.startup_mode, "urls");
        assert_eq!(app.startup_urls, vec!["https://example.com".to_owned()]);
        assert!(!app.profile_picker);
        assert_eq!(app.history_retention_days, 30);
        assert!(app.tabs_vertical);
        assert_eq!(app.sleep_timeout_min, 5);
        assert_eq!(app.sleep_exceptions, vec!["example.com".to_owned()]);
        assert!(app.onboarding_completed);
        assert!(app.viewsource_wrap);
        assert!(!app.autofill_address);
        assert!(!app.autofill_payment);
        assert!(app.autofill_form);
        assert!(!app.translate_enabled);
        assert_eq!(app.translate_target, "de");
        assert_eq!(app.reader_size, 120);
        assert!(app.reader_serif);
        assert!(app.reader_wide);
        assert_eq!(app.reader_theme, "sepia");
        assert_eq!(app.bar_mode, "always");
        assert!(app.bar_seen);
        assert!(app.panel_open);
        assert_eq!(app.panel_section, "history");
        assert_eq!(app.panel_w, 400);
        assert!(app.ask_downloads.load(std::sync::atomic::Ordering::Relaxed));
        assert!(app.pip_auto);
        assert!(!app.permissions.notifications_global());
        assert_eq!(
            app.save_last_dir,
            Some(std::path::PathBuf::from("/tmp/ferrite-test"))
        );
        assert!(app.privacy.settings().adblock_enabled, "present keys apply");
        assert!(!app.privacy.settings().gpc_enabled);
        assert_eq!(
            app.privacy.settings().doh_endpoint.as_deref(),
            Some("https://dns.example/dns-query"),
            "valid DoH applies"
        );
        // Invalid values are refused, hostile values clamped, never stored.
        app.apply_settings_doc(&serde_json::json!({
            "doh_endpoint": "http://evil.test/",
            "history_retention_days": 1,
            "reader_size": 999,
            "bar_mode": "sideways",
            "chrome_pack": "nope",
        }));
        assert_eq!(
            app.privacy.settings().doh_endpoint.as_deref(),
            Some("https://dns.example/dns-query"),
            "invalid DoH refused, current kept"
        );
        assert_eq!(app.history_retention_days, 7, "clamped, not stored raw");
        assert_eq!(app.reader_size, 150);
        assert_eq!(app.bar_mode, "always", "unknown mode ignored");
        assert_eq!(app.theme_pack, ThemePack::Tenebra, "unknown pack ignored");
    }

    #[test]
    fn reset_restores_every_default_live() {
        let mut app = test_app();
        app.suggestions_enabled = false;
        app.home_url = "https://example.com".to_owned();
        app.startup_mode = "urls".to_owned();
        app.sleep_timeout_min = 5;
        app.autofill_payment = false;
        app.translate_target = "de".to_owned();
        app.privacy.update_settings(|s| {
            s.adblock_enabled = false;
            s.doh_endpoint = Some("https://dns.example/dns-query".to_owned());
        });
        app.reset_to_defaults();
        assert_eq!(app.theme_pack, ThemePack::Tenebra);
        assert!(app.suggestions_enabled);
        assert!(!app.remote_enabled);
        assert_eq!(app.home_url, "ferrite://newtab");
        assert_eq!(app.startup_mode, "restore");
        assert!(app.startup_urls.is_empty());
        assert_eq!(app.history_retention_days, 90);
        assert!(!app.tabs_vertical);
        assert_eq!(app.sleep_timeout_min, 30);
        assert!(app.sleep_exceptions.is_empty());
        assert!(app.autofill_payment);
        assert!(app.autofill_form);
        assert_eq!(
            app.translate_target,
            ferrite_tabs::translate::DEFAULT_TARGET
        );
        assert_eq!(app.reader_size, 100);
        assert_eq!(app.reader_theme, "light");
        assert_eq!(app.bar_mode, "never");
        assert!(!app.panel_open);
        assert_eq!(app.panel_section, "bookmarks");
        assert_eq!(app.panel_w, 320);
        assert!(app.profile_picker);
        assert!(app.permissions.notifications_global());
        assert!(app.privacy.settings().adblock_enabled);
        assert!(app.privacy.settings().doh_endpoint.is_none());
        assert_eq!(app.app.omnibox.registry.current_id(), "duckduckgo");
    }

    #[test]
    fn escape_script_close_keeps_svg_kills_breakout() {
        // Server-rendered SVG passes through intact for innerHTML.
        let svg = "<svg viewBox=\"0 0 8 8\"><rect /></svg>";
        assert_eq!(escape_script_close(svg), svg);
        // Script breakouts die in every case; surrounding text survives.
        assert_eq!(escape_script_close("a</script>b"), "a<\\/script>b");
        assert_eq!(escape_script_close("a</ScRiPt>b"), "a<\\/ScRiPt>b");
        // Unicode (device names) passes through byte-intact.
        assert_eq!(
            escape_script_close("Zaud 📱</script>"),
            "Zaud 📱<\\/script>"
        );
        assert_eq!(escape_script_close("plain base64+/=="), "plain base64+/==");
    }

    #[test]
    fn sync_code_payload_carries_qr_or_degrades() {
        // Absent code: nulls, never an empty render.
        let v: serde_json::Value =
            serde_json::from_str(&sync_code_payload(None, None)).expect("payload is JSON");
        assert!(v.get("code").is_none_or(|c| c.is_null()));
        assert!(v.get("qr").is_none_or(|q| q.is_null()));
        assert!(v.get("expires_at_ms").is_none_or(|e| e.is_null()));
        // Realistic pairing length (~300B base64): code plus scannable SVG.
        let code: String = "eyJ".to_owned() + &"A".repeat(300);
        let v: serde_json::Value =
            serde_json::from_str(&sync_code_payload(Some(&code), Some(12345)))
                .expect("payload is JSON");
        assert_eq!(v.get("code").and_then(|c| c.as_str()), Some(code.as_str()));
        assert_eq!(v.get("expires_at_ms").and_then(|e| e.as_i64()), Some(12345));
        let qr = v.get("qr").and_then(|q| q.as_str()).expect("qr rendered");
        assert!(qr.contains("viewBox"), "matrix units");
        assert!(qr.contains("</svg>"), "closer survived transport escaping");
        assert!(!qr.contains("<\\/"), "no broken escapes in art");
        // Deterministic: same code, same modules.
        assert_eq!(
            sync_code_payload(Some(&code), Some(12345)),
            sync_code_payload(Some(&code), Some(12345))
        );
        // Over-long: text code still carries the pairing, art degrades.
        let huge = "x".repeat(2001);
        let v: serde_json::Value =
            serde_json::from_str(&sync_code_payload(Some(&huge), None)).expect("payload is JSON");
        assert_eq!(v.get("code").and_then(|c| c.as_str()), Some(huge.as_str()));
        assert!(v.get("qr").is_none_or(|q| q.is_null()));
    }

    #[test]
    fn pairing_repair_warns_only_on_key_rotation() {
        // Fresh pair: silent. Same key re-approval: silent.
        assert_eq!(pairing_repair_text(None, "Phone", &[1u8; 32]), None);
        assert_eq!(
            pairing_repair_text(Some(("Phone", [1u8; 32].as_slice())), "Phone", &[1u8; 32]),
            None
        );
        // Same id, new key: loud, naming both sides, pointing at revoke.
        let msg = pairing_repair_text(Some(("Phone", [1u8; 32].as_slice())), "Phone", &[2u8; 32])
            .expect("rotation warns");
        assert!(msg.contains("new key"), "says what happened: {msg}");
        assert!(msg.contains("revoke"), "says what to do: {msg}");
        assert!(!msg.contains("paired."), "never the happy-path copy: {msg}");
    }

    #[test]
    fn catching_up_needs_back_to_back_active_rounds() {
        assert!(!catching_up(None, 5, 1000), "first round never claims");
        assert!(
            !catching_up(Some((0, 900)), 5, 1000),
            "quiet before reads done"
        );
        assert!(
            !catching_up(Some((5, 900)), 0, 1000),
            "quiet now reads done"
        );
        assert!(catching_up(Some((5, 900)), 3, 1000), "streak reads syncing");
        assert!(
            !catching_up(Some((5, 1000)), 3, 200_000),
            "stale streaks expire"
        );
        assert!(
            !catching_up(Some((5, 200_000)), 3, 1000),
            "clock skew never claims"
        );
    }
}
#[cfg(test)]
mod theme_tests {
    use super::{ThemePack, full_theme_css};
    use crate::theme::ThemeConfig;

    #[test]
    fn full_theme_css_layers_base_pack_and_scheme() {
        let css = full_theme_css(&ThemeConfig::default(), ThemePack::Tenebra);
        // Base tokens first, pack wins overlaps, scheme tracks the pack.
        assert!(css.contains("--f-bg:#0d0a06"), "tenebra base {css}");
        assert!(css.contains("--f-accent:#ffc670"), "tenebra flare");
        assert!(css.contains(":root{color-scheme:dark}"), "dark pack");
        let light = full_theme_css(&ThemeConfig::default(), ThemePack::PrismVoid);
        assert!(light.contains(":root{color-scheme:light}"), "light pack");
        // Boot and switch share this constructor by construction: the
        // factory seed and the live push cannot drift.
        assert!(css.contains("--f-border:"), "border contract");
        assert!(css.contains("--f-font-ui:"), "font contract");
    }
}

#[cfg(test)]
mod share_tests {
    use super::*;

    #[test]
    fn fragment_link_encodes_selections() {
        assert_eq!(
            ShellApp::fragment_link("https://example.com/a", "hello world"),
            Some("https://example.com/a#:~:text=hello%20world".to_owned())
        );
        // Existing fragments are replaced, not stacked.
        assert_eq!(
            ShellApp::fragment_link("https://example.com/a#old", "hi there"),
            Some("https://example.com/a#:~:text=hi%20there".to_owned())
        );
        // Whitespace collapses; punctuation-only selections link nowhere.
        assert_eq!(
            ShellApp::fragment_link("https://example.com/", "  a   b  "),
            Some("https://example.com/#:~:text=a%20b".to_owned())
        );
        assert_eq!(ShellApp::fragment_link("https://example.com/", "… :"), None);
        assert_eq!(ShellApp::fragment_link("https://example.com/", ""), None);
        // Non-web schemes never fragment.
        assert_eq!(ShellApp::fragment_link("ferrite://history", "hello"), None);
        assert_eq!(ShellApp::fragment_link("file:///x", "hello"), None);
        // Long selections cap on a word boundary (decode %20 back to
        // measure the raw snippet, not the encoding expansion).
        let long = "word ".repeat(100);
        let link = ShellApp::fragment_link("https://example.com/", &long).unwrap();
        let frag = link.split("#:~:text=").nth(1).unwrap();
        let raw = frag.replace("%20", " ");
        assert!(raw.len() <= 300, "capped snippet");
        assert!(raw.ends_with("word"), "no partial tail");
    }

    #[test]
    fn fragment_encode_keeps_unreserved_bare() {
        assert_eq!(fragment_encode("abc-_.~019"), "abc-_.~019");
        assert_eq!(fragment_encode("a b"), "a%20b");
        assert_eq!(fragment_encode("100%"), "100%25");
        assert_eq!(fragment_encode("ä"), "%C3%A4");
    }
}

#[cfg(test)]
mod boot_log_tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-bootlog-{}-{}",
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
    fn boot_log_appends_caps_and_reads_last() {
        let dir = scratch();
        assert_eq!(read_boot_last_in(&dir), None);
        append_boot_log_in(&dir, "0.2.0", Some(120), Some(900));
        append_boot_log_in(&dir, "0.2.0", None, Some(950));
        assert_eq!(
            read_boot_last_in(&dir).as_deref(),
            Some("v=0.2.0 stores=? paint=950")
        );
        // Cap: 200 lines max, oldest shed first.
        for _ in 0..250 {
            append_boot_log_in(&dir, "0.2.0", Some(1), Some(2));
        }
        let text = std::fs::read_to_string(dir.join(BOOT_LOG_NAME)).unwrap();
        assert_eq!(text.lines().count(), BOOT_LOG_LINES);
        assert!(text.lines().last().unwrap().starts_with("v=0.2.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_size_sums_files_skips_symlinks_and_caps() {
        let dir = scratch();
        std::fs::write(dir.join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), vec![0u8; 50]).unwrap();
        assert_eq!(dir_size_capped(&dir, 100_000), 150);
        assert_eq!(dir_size_capped(&dir.join("missing"), 100_000), 0);
        // Zero budget measures nothing (cap honored before traversal).
        assert_eq!(dir_size_capped(&dir, 0), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod master_prefs_tests {
    use super::*;

    #[test]
    fn master_prefs_seed_valid_keys_only() {
        let empty = serde_json::Value::Object(serde_json::Map::new());
        let master = serde_json::json!({
            "homepage": "https://start.test",
            "startup_mode": "urls",
            "startup_urls": ["https://a.test", "javascript:alert(1)", "https://a.test"],
            "search_engine": "brave",
            "unknown_future_key": true,
        });
        let out = apply_master_prefs(empty, &master);
        assert_eq!(
            out.get("home_url").and_then(|x| x.as_str()),
            Some("https://start.test")
        );
        assert_eq!(
            out.get("startup_mode").and_then(|x| x.as_str()),
            Some("urls")
        );
        assert_eq!(
            out.get("startup_urls")
                .and_then(|x| x.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            out.get("omnibox_engine").and_then(|x| x.as_str()),
            Some("brave")
        );
        assert!(out.get("unknown_future_key").is_none());
        // Garbage values fall back, never poison.
        let bad = serde_json::json!({
            "homepage": "javascript:alert(1)",
            "startup_mode": "nuke",
            "startup_urls": "not-an-array",
            "search_engine": "",
        });
        let out = apply_master_prefs(serde_json::Value::Object(serde_json::Map::new()), &bad);
        assert!(out.get("home_url").is_none());
        assert_eq!(
            out.get("startup_mode").and_then(|x| x.as_str()),
            Some("restore")
        );
        assert!(out.get("startup_urls").is_none());
        assert!(out.get("omnibox_engine").is_none());
        // Non-object settings pass through untouched.
        let scalar = serde_json::Value::Null;
        assert_eq!(apply_master_prefs(scalar.clone(), &master), scalar);
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;

    fn test_vault() -> std::sync::Arc<ferrite_vault::Vault> {
        std::sync::Arc::new(ferrite_vault::Vault::open_in_memory().unwrap())
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ferrite_shell_import_{name}"))
    }

    #[test]
    fn bookmarks_round_trip_through_dialog_core() {
        let vault = test_vault();
        let path = temp_path("bookmarks.html");
        std::fs::write(
            &path,
            r#"<!DOCTYPE NETSCAPE-Bookmark-file-1><DL><DT><A HREF="https://example.com">Example</A></DL>"#,
        )
        .unwrap();
        let r = import_from_path(&vault, "bookmarks", &path).unwrap();
        assert_eq!(r.imported, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_and_unknown_kind_are_errors_not_panics() {
        let vault = test_vault();
        let missing = temp_path("definitely-not-here.html");
        let _ = std::fs::remove_file(&missing);
        assert!(import_from_path(&vault, "bookmarks", &missing).is_err());
        assert!(import_from_path(&vault, "history", &missing).is_err());
        assert!(import_from_path(&vault, "nope", &missing).is_err());
    }

    #[test]
    fn tile_score_is_frecency_not_recency() {
        let now = 1_790_000_000_000i64;
        let day = 86_400_000i64;
        // A 30-visit daily from yesterday outranks a one-off from now.
        assert!(tile_score(30, now - day) > tile_score(1, now));
        // A 200-visit giant from a month ago fades below a fresh daily.
        assert!(tile_score(200, now - 30 * day) < tile_score(30, now));
        // Ties break newest-first.
        assert!(tile_score(5, now) > tile_score(5, now - day));
    }

    #[test]
    fn bookmarks_dispatch_sniffs_content_not_extension() {
        // Chromium JSON through the same "bookmarks" kind, nameless file.
        let vault = test_vault();
        let path = temp_path("chromium_bookmarks_nameless");
        std::fs::write(
            &path,
            r#"{"roots":{"bookmark_bar":{"type":"folder","children":[
                {"type":"url","name":"Example","url":"https://example.com"}
            ]},"other":{"type":"folder","children":[]},"synced":{"type":"folder","children":[]}}}"#,
        )
        .unwrap();
        let r = import_from_path(&vault, "bookmarks", &path).unwrap();
        assert_eq!(r.imported, 1);
        // Repeat is a duplicate, not a second copy.
        let again = import_from_path(&vault, "bookmarks", &path).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped_duplicate, 1);
        // Unknown kind errors instead of importing nothing.
        assert!(import_from_path(&vault, "profile", &path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn singleton_match_covers_fragments_not_prefixes() {
        let base = "ferrite://settings";
        assert!(singleton_match("ferrite://settings", base));
        assert!(singleton_match("ferrite://settings#privacy", base));
        assert!(singleton_match("ferrite://settings#cat-advanced", base));
        assert!(!singleton_match("ferrite://settings2", base));
        assert!(!singleton_match("ferrite://newtab", base));
        assert!(!singleton_match("", base));
    }

    #[test]
    fn detect_browsers_finds_seeded_files() {
        let root = std::env::temp_dir().join("ferrite_detect_test");
        let _ = std::fs::remove_dir_all(&root);
        let chrome_hist = root.join("Google/Chrome/User Data/Default/History");
        let ff_prof = root.join("Mozilla/Firefox/Profiles/abc123.default");
        std::fs::create_dir_all(chrome_hist.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&ff_prof).unwrap();
        std::fs::write(&chrome_hist, b"sqlite").unwrap();
        std::fs::write(ff_prof.join("places.sqlite"), b"sqlite").unwrap();
        let base = root.to_string_lossy().into_owned();
        let found = detect_browsers_in(&base, &base, "/nonexistent-home");
        let ids: Vec<&str> = found.iter().map(|s| s.id).collect();
        assert!(ids.contains(&"chrome"), "chrome history: {ids:?}");
        assert!(ids.contains(&"firefox"), "firefox places: {ids:?}");
        assert!(!ids.contains(&"edge"), "no edge seeded");
        assert!(!ids.contains(&"chrome-bookmarks"), "no bookmarks seeded");
        // A Chromium Bookmarks JSON file is detected separately from History.
        let chrome_marks = root.join("Google/Chrome/User Data/Default/Bookmarks");
        std::fs::write(&chrome_marks, b"{}").unwrap();
        let ids2: Vec<&str> = detect_browsers_in(&base, &base, "/nonexistent-home")
            .iter()
            .map(|s| s.id)
            .collect();
        assert!(
            ids2.contains(&"chrome-bookmarks"),
            "chrome bookmarks: {ids2:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        // Empty dirs find nothing, never panic.
        assert!(
            detect_browsers_in("/nonexistent-a", "/nonexistent-b", "/nonexistent-c").is_empty()
        );
    }

    #[test]
    fn detect_browsers_finds_webdata_and_safari() {
        let root = std::env::temp_dir().join("ferrite_detect_test2");
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("home");
        let wd = base.join(".config/google-chrome/Default/Web Data");
        std::fs::create_dir_all(wd.parent().unwrap()).unwrap();
        std::fs::write(wd, b"sqlite").unwrap();
        let saf = base.join("Library/Safari");
        std::fs::create_dir_all(&saf).unwrap();
        std::fs::write(saf.join("History.db"), b"sqlite").unwrap();
        std::fs::write(saf.join("Bookmarks.plist"), b"plist").unwrap();
        let home = base.to_string_lossy().into_owned();
        let ids: Vec<&str> = detect_browsers_in("/nonexistent-a", "/nonexistent-b", &home)
            .iter()
            .map(|s| s.id)
            .collect();
        assert!(ids.contains(&"chrome-engines"), "engines: {ids:?}");
        assert!(ids.contains(&"chrome-addresses"), "addresses: {ids:?}");
        assert!(ids.contains(&"safari-history"), "safari history: {ids:?}");
        assert!(ids.contains(&"safari-bookmarks"), "safari books: {ids:?}");
        assert!(!ids.contains(&"edge-engines"), "no edge seeded");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_browser_parsers_map_tokens() {
        // Windows reg output.
        let reg = "HKEY_CURRENT_USER\\test\r\n    ProgId    REG_SZ    ChromeHTML\r\n";
        assert_eq!(parse_windows_prog_id(reg).as_deref(), Some("ChromeHTML"));
        assert_eq!(parse_windows_prog_id("empty\n"), None);
        assert_eq!(browser_id_for_token("ChromeHTML"), Some("chrome"));
        assert_eq!(browser_id_for_token("MSEdgeHTM"), Some("edge"));
        assert_eq!(browser_id_for_token("microsoft-edge-stable"), Some("edge"));
        assert_eq!(
            browser_id_for_token("FirefoxURL-308046B0AF4A39CB"),
            Some("firefox")
        );
        assert_eq!(browser_id_for_token("com.apple.safari"), Some("safari"));
        assert_eq!(browser_id_for_token("OperaStable"), None);
        // macOS defaults output: first http handler wins.
        let mac = "(\n    {\n        LSHandlerRoleAll = \"com.microsoft.edgemac\";\n        LSHandlerURLScheme = https;\n    },\n    {\n        LSHandlerRoleAll = \"com.google.chrome\";\n        LSHandlerURLScheme = http;\n    }\n)\n";
        assert_eq!(
            parse_macos_handlers(mac).as_deref(),
            Some("com.microsoft.edgemac")
        );
        assert_eq!(parse_macos_handlers("garbage\n"), None);
        // Linux desktop file id.
        assert_eq!(
            browser_id_for_token("google-chrome.desktop"),
            Some("chrome")
        );
        assert_eq!(browser_id_for_token("firefox.desktop"), Some("firefox"));
    }

    #[test]
    fn chrome_address_maps_to_book_shape() {
        let a = ferrite_vault::import::ChromeAddress {
            name: " Ada L ".to_owned(),
            street: "1 Main St".to_owned(),
            city: "Dessau".to_owned(),
            region: "ST".to_owned(),
            postal: "06844".to_owned(),
            country: "DE".to_owned(),
            phone: "+49123".to_owned(),
            email: "ada@test.example".to_owned(),
        };
        let f = address_fields_from_chrome(&a);
        assert_eq!(f.label, "Imported: Ada L");
        assert_eq!(f.postal, "06844");
        assert_eq!(
            address_dedupe_key(&f.name, &f.street, &f.postal, &f.country),
            address_dedupe_key("ada l", "1 main st", "06844", "de")
        );
    }

    #[test]
    fn encode_query_keeps_unreserved_escapes_rest() {
        assert_eq!(encode_query("invoice 2024"), "invoice%202024");
        assert_eq!(encode_query("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_query("x/y?z"), "x%2Fy%3Fz");
        assert_eq!(encode_query(""), "");
    }

    #[test]
    fn host_slice_extracts_filter_host() {
        assert_eq!(host_slice("https://example.com/a?b=1"), "example.com");
        assert_eq!(host_slice("https://user@example.com:8080/x"), "example.com");
        assert_eq!(host_slice("https://[::1]/"), "[::1]");
        assert_eq!(host_slice("https://[::1]:8080/x"), "[::1]");
        assert_eq!(host_slice("not a url"), "not a url");
        assert_eq!(host_slice(""), "");
    }

    #[test]
    fn crash_counter_is_version_scoped() {
        // Same version accumulates. A fixed bug must not keep escalating.
        assert_eq!(parse_crash_count("1.2.3\n2", "1.2.3"), 2);
        assert_eq!(parse_crash_count("1.2.2\n9", "1.2.3"), 0);
        // Legacy bare-number files count for the current build.
        assert_eq!(parse_crash_count("4", "1.2.3"), 4);
        assert_eq!(parse_crash_count("", "1.2.3"), 0);
        assert_eq!(parse_crash_count("garbage", "1.2.3"), 0);
        assert_eq!(parse_crash_count("1.2.3\nnot-a-number", "1.2.3"), 0);
        // Round trip.
        assert_eq!(
            parse_crash_count(&format_crash_count("1.2.3", 5), "1.2.3"),
            5
        );
    }

    #[test]
    fn allowlist_admits_validated_flows_never_mutations() {
        // Form/autofill prompts must reach the shell from content pages
        // (each arm re-validates). Vault mutations and settings writes
        // stay privileged-only.
        for t in [
            "autofill_scan",
            "autofill_focus",
            "autofill_dismiss",
            "af_nav",
            "af_pick",
            "af_filter",
            "form_history_save",
            "password_save_candidate",
            "cosmetic_query",
            "form_risk",
            "form_dirty",
            "live_call",
            "translate_state",
            "media_drm_blocked",
            "lang",
            "scroll",
            "favicon",
            "bootdecision_data",
            "bootdecision",
            "bootrecovery",
        ] {
            assert!(content_page_allowed(t), "{t} admitted");
        }
        for t in [
            "passwords_add",
            "passwords_delete",
            "perm_grant_revoke",
            "certs_trust",
            "threat_trust",
            "exclusion_add",
            "dl_open",
            "cookies_query",
            // Site view arms are privileged-only (ferrite://site): the
            // forget arm deletes cross-store user data, so no content
            // page may post it even though the query arm only reads.
            "site_query",
            "site_forget",
        ] {
            assert!(!content_page_allowed(t), "{t} denied");
        }
    }

    #[test]
    fn site_origin_normalizes_to_scheme_host() {
        // Bare hosts assume https; URLs collapse to scheme://host with
        // default ports stripped, userinfo/paths dropped.
        assert_eq!(
            clean_site_origin("example.com"),
            Some("https://example.com".to_owned())
        );
        assert_eq!(
            clean_site_origin("https://example.com/a?b=1#x"),
            Some("https://example.com".to_owned())
        );
        assert_eq!(
            clean_site_origin("https://example.com:443/"),
            Some("https://example.com".to_owned())
        );
        assert_eq!(
            clean_site_origin("http://example.com:80/"),
            Some("http://example.com".to_owned())
        );
        // Non-default ports stay (a different store key).
        assert_eq!(
            clean_site_origin("https://example.com:8443/"),
            Some("https://example.com:8443".to_owned())
        );
        // Refusals: whitespace smuggling, non-http schemes, bare junk.
        assert_eq!(clean_site_origin("java\tscript:evil"), None);
        assert_eq!(clean_site_origin("file:///etc/passwd"), None);
        assert_eq!(clean_site_origin(""), None);
        assert_eq!(clean_site_origin("ferrite://settings"), None);
        assert_eq!(site_origin_host("https://example.com"), "example.com");
    }

    #[test]
    fn window_icon_decodes_brand_favicon() {
        let (rgba, w, h) =
            ShellConfig::window_icon_pixels().expect("brand/favicon.png must decode");
        assert!(w >= 16 && h >= 16, "usable icon size: {w}x{h}");
        assert_eq!(rgba.len(), (w * h * 4) as usize, "rgba buffer matches");
        // Not a blank image: the mark carries real pixels.
        assert!(rgba.chunks_exact(4).any(|p| p[3] > 0), "non-empty alpha");
        assert!(ShellConfig::window_icon().is_some(), "icon builds");
    }

    #[test]
    fn degraded_notices_name_loss_without_leaking_paths() {
        for (notice, store) in [
            (
                vault_degraded_notice("legacy vault file at C:\\vault\\f.db: pre-SQLCipher format"),
                "Vault",
            ),
            (
                vault_degraded_notice("vault key configured but built without `sqlcipher` feature"),
                "Vault",
            ),
            (vault_degraded_notice("database corrupt: btree"), "Vault"),
            (
                archive_degraded_notice("legacy archive file at /tmp/a.db: pre-SQLCipher format"),
                "Archive",
            ),
            (
                archive_degraded_notice("archive key configured but built without `sqlcipher`"),
                "Archive",
            ),
        ] {
            assert!(notice.starts_with(store), "{notice}");
            assert!(
                notice.contains("won't be saved") || notice.contains("won't be kept"),
                "{notice}"
            );
            assert!(!notice.contains("C:\\"), "{notice}");
            assert!(!notice.contains("/tmp"), "{notice}");
        }
    }

    #[test]
    fn save_candidate_gate() {
        // Happy path.
        assert!(ShellApp::valid_save_candidate(
            "https://bank.test/login",
            "user@test",
            "s3cret!"
        ));
        // Non-web origins never prompt.
        assert!(!ShellApp::valid_save_candidate(
            "ferrite://settings",
            "u",
            "p"
        ));
        assert!(!ShellApp::valid_save_candidate("not a url", "u", "p"));
        assert!(!ShellApp::valid_save_candidate(
            "https://evil.test/x y",
            "u",
            "p"
        ));
        // Empty username/password never prompt. Caps bound IPC.
        assert!(!ShellApp::valid_save_candidate("https://a.test", "", "p"));
        assert!(!ShellApp::valid_save_candidate("https://a.test", "u", ""));
        assert!(!ShellApp::valid_save_candidate(
            "https://a.test",
            &"u".repeat(257),
            "p"
        ));
        assert!(!ShellApp::valid_save_candidate(
            "https://a.test",
            "u",
            &"p".repeat(4097)
        ));
    }

    #[test]
    fn export_writes_netscape_html() {
        let vault = test_vault();
        let src = temp_path("export_src.html");
        std::fs::write(
            &src,
            r#"<!DOCTYPE NETSCAPE-Bookmark-file-1><DL><p><DT><H3>Stuff</H3><DL><p><DT><A HREF="https://example.com">Example</A></DL><p></DL><p>"#,
        )
        .unwrap();
        let r = import_from_path(&vault, "bookmarks", &src).unwrap();
        assert_eq!(r.imported, 1);
        let html = ferrite_vault::import::export_bookmarks_netscape(&vault);
        // Folders nest, links escape, the file re-imports cleanly.
        assert!(html.contains(">Stuff</H3>"));
        assert!(html.contains("<DL><p>"));
        assert!(html.contains("https://example.com"));
        let out = temp_path("export_out.html");
        std::fs::write(&out, &html).unwrap();
        let back = ferrite_vault::import::import_netscape_bookmarks(out.to_str().unwrap(), &vault)
            .unwrap();
        assert_eq!(back.imported, 0, "same vault: duplicates skip");
        assert_eq!(back.skipped_duplicate, 1);
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    fn test_app() -> ShellApp {
        ShellApp::new(
            ShellConfig::default(),
            None,
            crate::forward::ForwardQueue::default(),
        )
    }

    #[test]
    fn settings_push_masks_realtime_key() {
        let app = test_app();
        app.privacy.update_settings(|s| {
            s.realtime_api_key = Some("k-secret".to_owned());
        });
        let masked = app.masked_privacy_for_push();
        assert_eq!(
            masked.get("realtime_api_key_set").and_then(|v| v.as_bool()),
            Some(true),
            "page learns presence, never the secret"
        );
        assert!(
            masked.get("realtime_api_key").is_none_or(|v| v.is_null()),
            "secret must not reach page state"
        );
        let text = serde_json::to_string(&masked).unwrap_or_default();
        assert!(!text.contains("k-secret"), "secret must not serialize");
        app.privacy.update_settings(|s| s.realtime_api_key = None);
        let cleared = app.masked_privacy_for_push();
        assert_eq!(
            cleared
                .get("realtime_api_key_set")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn relay_urls_cleaned_never_repaired() {
        assert!(clean_relay_urls("").is_empty());
        assert_eq!(
            clean_relay_urls("wss://r.example.com/sync"),
            vec!["wss://r.example.com/sync".to_owned()]
        );
        // Separators, case, and dedupe.
        assert_eq!(
            clean_relay_urls("ws://a:1/x, wss://b:2/y\nws://a:1/x"),
            vec!["ws://a:1/x".to_owned(), "wss://b:2/y".to_owned()]
        );
        // Non-ws schemes refused, never repaired into something dialable.
        assert!(clean_relay_urls("https://evil.test/, gopher://x").is_empty());
        // Over-long entries dropped, not truncated (truncation corrupts).
        assert!(clean_relay_urls(&format!("wss://h/{}", "x".repeat(300))).is_empty());
        // Cap of 8 keeps the failover list bounded.
        let many = (0..20)
            .map(|i| format!("ws://h{i}:1/s"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(clean_relay_urls(&many).len(), 8);
    }

    #[test]
    fn setting_urls_use_page_fragments() {
        assert_eq!(
            palette_setting_url_by_id("privacy-adblock"),
            "ferrite://settings#privacy"
        );
        assert_eq!(
            palette_setting_url_by_id("autofill-address"),
            "ferrite://settings#cat-autofill"
        );
        assert_eq!(
            palette_setting_url_by_id("about-version"),
            "ferrite://about"
        );
        assert_eq!(palette_setting_url_by_id("nope"), "ferrite://settings");
    }

    #[test]
    fn snapshot_carries_all_three_kinds_plus_recency() {
        let mut app = test_app();
        app.app
            .tabs
            .new_discarded_tab("https://example.com".into(), "Example".into(), false)
            .unwrap();
        app.palette_recent.push_back("print".into());
        let v: serde_json::Value =
            serde_json::from_str(&app.palette_snapshot()).expect("snapshot is JSON");
        let tabs = v.get("tabs").and_then(|x| x.as_array()).expect("tabs");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].get("kind").and_then(|x| x.as_str()), Some("tab"));
        let commands = v
            .get("commands")
            .and_then(|x| x.as_array())
            .expect("commands");
        assert_eq!(commands.len(), ferrite_chrome::commands::COMMANDS.len());
        assert!(
            commands
                .iter()
                .all(|c| c.get("kind").and_then(|x| x.as_str()) == Some("command"))
        );
        let settings = v
            .get("settings")
            .and_then(|x| x.as_array())
            .expect("settings");
        assert_eq!(
            settings.len(),
            ferrite_tabs::protocol::SETTINGS_CATALOGUE.len()
        );
        assert!(settings.iter().all(|s| {
            s.get("url")
                .and_then(|x| x.as_str())
                .is_some_and(|u| u.starts_with("ferrite://"))
        }));
        assert_eq!(
            v.get("recent").and_then(|x| x.as_array()).map(|r| r.len()),
            Some(1)
        );
    }

    #[test]
    fn registry_every_id_executes() {
        for c in ferrite_chrome::commands::COMMANDS {
            assert!(
                ferrite_chrome::commands::palette_command(c.id).is_some(),
                "palette id with no execution: {}",
                c.id
            );
        }
    }

    #[test]
    fn recency_is_mru_capped_and_deduped() {
        let mut app = test_app();
        app.record_palette_use("print");
        app.record_palette_use("history");
        app.record_palette_use("print");
        let ids: Vec<&str> = app.palette_recent.iter().map(|s| s.as_str()).collect();
        assert_eq!(ids, ["print", "history"]);
        for i in 0..20 {
            app.record_palette_use(&format!("cmd-{i}"));
        }
        assert_eq!(app.palette_recent.len(), 12);
        assert_eq!(
            app.palette_recent.front().map(|s| s.as_str()),
            Some("cmd-19")
        );
    }

    #[test]
    fn run_unknown_rows_are_ignored_never_panics() {
        let mut app = test_app();
        app.run_palette_row("command", "nope");
        assert!(app.palette_recent.is_empty());
        app.run_palette_row("setting", "nope");
        app.run_palette_row("tab", "not-a-uuid");
        app.run_palette_row("nonsense", "x");
    }

    #[test]
    fn closed_stack_reopens_by_display_index() {
        let mut app = test_app();
        app.remember_closed_bare("https://a.com".to_owned(), "A".to_owned());
        app.remember_closed_bare("https://b.com".to_owned(), "B".to_owned());
        // Display is most-recent-first; index 0 is the latest close.
        let disp = app.closed_display();
        assert_eq!(disp.len(), 2);
        assert_eq!(disp[0].0, "https://b.com");
        assert_eq!(disp[1].0, "https://a.com");
        // Middle removal keeps the rest; out-of-range is None, never panic.
        assert_eq!(
            app.take_closed_at(1).map(|e| e.url),
            Some("https://a.com".to_owned())
        );
        assert_eq!(app.take_closed_at(5), None);
        assert_eq!(app.closed_tabs.len(), 1);
        // Restore-all drains oldest-first (caller opens in order, newest
        // ends active).
        let all = app.drain_closed_all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].url, "https://b.com");
        assert!(app.closed_tabs.is_empty());
    }

    #[test]
    fn private_tabs_never_reach_the_closed_stack() {
        let mut app = test_app();
        app.remember_closed(
            "https://secret.test".to_owned(),
            "Secret".to_owned(),
            NavigationHistory::default(),
            (0.0, 0.0),
            false,
            true,
        );
        assert!(app.closed_tabs.is_empty(), "private close leaves no trace");
        assert!(app.closed_display().is_empty());
        app.remember_closed(
            "https://plain.test".to_owned(),
            "Plain".to_owned(),
            NavigationHistory::default(),
            (0.0, 0.0),
            false,
            false,
        );
        assert_eq!(app.closed_tabs.len(), 1);
    }

    #[test]
    fn closed_stack_caps_oldest_first() {
        let mut app = test_app();
        for i in 0..(CLOSED_CAP + 5) {
            app.remember_closed_bare(format!("https://site{i}.com"), format!("S{i}"));
        }
        assert_eq!(app.closed_tabs.len(), CLOSED_CAP);
        assert_eq!(app.closed_tabs[0].url, "https://site5.com");
        assert_eq!(
            app.closed_tabs[CLOSED_CAP - 1].url,
            format!("https://site{}.com", CLOSED_CAP + 4)
        );
    }

    #[test]
    fn graft_restores_history_scroll_pin_and_title() {
        use ferrite_tabs::{MockHandle, TabState, WebViewHandle};
        use std::rc::Rc;
        let mut history = NavigationHistory::new();
        history.push("https://a.test/1", "https://a.test/2");
        history.push("https://a.test/2", "https://a.test/3");
        let closed = ClosedTab {
            url: "https://a.test/3".to_owned(),
            title: "Kept Title".to_owned(),
            history: history.clone(),
            scroll: (0.0, 420.0),
            pinned: true,
        };
        let mut tab = TabState::new(
            uuid::Uuid::new_v4(),
            "https://a.test/3".to_owned(),
            Rc::new(MockHandle::default()) as Rc<dyn WebViewHandle>,
        );
        ShellApp::graft_closed_state(&mut tab, &closed);
        assert_eq!(tab.history, history, "back stacks survive the close");
        assert!(tab.history.can_go_back());
        assert_eq!(tab.scroll_position, (0.0, 420.0));
        assert!(tab.is_pinned, "pin restores");
        assert_eq!(tab.title, "Kept Title");
        assert!(tab.suppress_next_history, "open must not append");
    }

    #[test]
    fn failed_reopen_keeps_the_entry() {
        // Headless: no window, so every open refuses. The entry must
        // survive for the next attempt instead of being lost.
        let mut app = test_app();
        app.remember_closed_bare("https://a.test".to_owned(), "A".to_owned());
        app.reopen_closed_tab();
        assert_eq!(app.closed_tabs.len(), 1);
        assert_eq!(app.closed_tabs[0].url, "https://a.test");
    }

    #[test]
    fn scoped_omnibox_filters_to_one_source() {
        let mut app = test_app();
        app.app
            .tabs
            .new_discarded_tab("https://example.com".into(), "Example".into(), false)
            .unwrap();
        app.app.omnibox.set_input("@tabs example".to_owned());
        let state = app.chrome_state();
        assert_eq!(state.omni_scope.as_deref(), Some("tabs"));
        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(state.suggestions[0].kind, "tab");
        // Unscoped keeps the mixed budget and no scope marker.
        app.app.omnibox.set_input("example".to_owned());
        let state = app.chrome_state();
        assert_eq!(state.omni_scope, None);
        assert!(state.suggestions.iter().any(|s| s.kind == "tab"));
        assert!(state.suggestions.iter().any(|s| s.kind == "search"));
    }

    #[test]
    fn run_tab_and_setting_rows_resolve_without_window() {
        let mut app = test_app();
        app.app
            .tabs
            .new_discarded_tab("https://a.test".into(), "A".into(), false)
            .unwrap();
        let id = app.app.tabs.iter().next().unwrap().id.to_string();
        // Headless: no window, so no switch happens. But nothing panics
        // and nothing records recency for non-command rows.
        app.run_palette_row("tab", &id);
        app.run_palette_row("setting", "privacy-adblock");
        assert!(app.palette_recent.is_empty());
    }
}

#[cfg(test)]
mod chrome_state_tests {
    use super::*;

    #[test]
    fn content_slot_sits_below_chrome_bars() {
        assert_eq!(CHROME_TOP_PX, 88.0);
        // Vertical rail contract (template + geometry must agree):
        // 232px rail, 48px omnibar row on top.
        assert_eq!(VTAB_W_PX, 232.0);
        assert_eq!(CHROME_TOP_VTAB_PX, 48.0);
    }

    #[test]
    fn chrome_state_serializes_for_push() {
        let state = ChromeState::empty();
        let script = push_script(&state);
        assert!(script.starts_with("__ferriteChrome("));
        // Session contract: single source of truth for the badge.
        assert!(script.contains("\"session_kind\":\"user\""));
        assert!(script.contains("\"incognito\":false"));
    }

    #[test]
    fn proxy_workaround_roster_mirrors_protocol_pages() {
        // server.rs keeps its own copy (the proxy crate cannot depend on
        // tabs): any drift either dials internal pages upstream or serves
        // local stubs for foreign hosts. This fails on either side's edit.
        let mut proxy = ferrite_proxy::server::INTERNAL_WORKAROUND_PAGES.to_vec();
        let mut served = ferrite_tabs::protocol::INTERNAL_PAGE_NAMES.to_vec();
        proxy.sort_unstable();
        served.sort_unstable();
        assert_eq!(proxy, served);
    }

    #[test]
    fn chrome_telemetry_falls_through_instead_of_rejecting() {
        // Init-script reporters post {type:...} in every webview including
        // chrome (boot logged 8 of these as rejections): they must reach
        // the shared path, not die in the strict command parser.
        assert!(chrome_payload_is_telemetry(
            r#"{"type":"lang","lang":"en-US"}"#
        ));
        assert!(chrome_payload_is_telemetry(r#"{"type":"console_error"}"#));
        assert!(chrome_payload_is_telemetry(
            r#"{"type":"audio","playing":false}"#
        ));
        assert!(chrome_payload_is_telemetry(
            r#"{"type":"scroll","x":0,"y":0}"#
        ));
        assert!(chrome_payload_is_telemetry(
            r#"{"type":"favicon","url":"https://x/y.ico"}"#
        ));
        // Commands stay on the command path even when they also parse as
        // JSON: `cmd` without `type` is not telemetry.
        assert!(!chrome_payload_is_telemetry(r#"{"cmd":"reload"}"#));
        assert!(!chrome_payload_is_telemetry(
            r#"{"cmd":"menu_item","id":"settings"}"#
        ));
        // Garbage stays rejected: non-JSON, empty, and type-less shapes.
        assert!(!chrome_payload_is_telemetry("not json"));
        assert!(!chrome_payload_is_telemetry(""));
        assert!(!chrome_payload_is_telemetry(r#"{"foo":1}"#));
        assert!(!chrome_payload_is_telemetry(r#"{"type":42}"#));
    }

    #[test]
    fn commit_trusts_only_navigable_suggestions() {
        // History/bookmark/tab rows carry real URLs: verbatim is correct.
        assert!(is_navigable_suggestion("https://example.com/a"));
        assert!(is_navigable_suggestion("http://a.test/"));
        assert!(is_navigable_suggestion("ferrite://history?q=x"));
        assert!(is_navigable_suggestion("  HTTPS://EXAMPLE.COM  "));
        // Answer rows carry raw expressions, remote rows bare phrases:
        // both must fall through to classify, never navigate verbatim.
        assert!(!is_navigable_suggestion("2+2"));
        assert!(!is_navigable_suggestion("rust book"));
        assert!(!is_navigable_suggestion(""));
        assert!(!is_navigable_suggestion("javascript:alert(1)"));
        assert!(!is_navigable_suggestion("data:text/html,x"));
        assert!(!is_navigable_suggestion("file:///etc/passwd"));
        assert!(!is_navigable_suggestion("example.com"));
    }

    #[test]
    fn math_answer_pins_above_all_rows() {
        // Regression: the parser passed while the UI showed no answer,
        // because wholesale `suggestions` assignment dropped it.
        let mut app = ShellApp::new(
            ShellConfig::default(),
            None,
            crate::forward::ForwardQueue::default(),
        );
        app.app.omnibox.set_input("241*17".to_owned());
        let state = app.chrome_state();
        let first = state.suggestions.first().expect("math answer present");
        assert_eq!(first.kind, "answer");
        assert_eq!(first.title, "= 4097");
    }

    #[test]
    fn sleep_host_normalizer_rejects_garbage() {
        assert_eq!(
            clean_sleep_host("Example.COM"),
            Some("example.com".to_owned())
        );
        assert_eq!(
            clean_sleep_host("  .mail.example.com/path  "),
            Some("mail.example.com".to_owned())
        );
        assert_eq!(
            clean_sleep_host("example.com:8080"),
            Some("example.com".to_owned())
        );
        assert_eq!(clean_sleep_host("localhost"), Some("localhost".to_owned()));
        assert_eq!(clean_sleep_host(""), None);
        assert_eq!(clean_sleep_host("   "), None);
        assert_eq!(clean_sleep_host("exa mple.com"), None);
        assert_eq!(clean_sleep_host("a:b:c"), None);
        assert_eq!(clean_sleep_host(&"x".repeat(300)), None);
    }

    #[test]
    fn startup_mode_and_urls_clean_defensively() {
        assert_eq!(clean_startup_mode("restore"), "restore");
        assert_eq!(clean_startup_mode("newtab"), "newtab");
        assert_eq!(clean_startup_mode("urls"), "urls");
        assert_eq!(clean_startup_mode(""), "restore");
        assert_eq!(clean_startup_mode("nuke"), "restore");
        assert_eq!(clean_startup_mode(" Restore "), "restore");
        let v = serde_json::json!([
            "https://a.test",
            "  https://a.test  ",
            "javascript:alert(1)",
            "",
            "ferrite://history",
        ]);
        assert_eq!(
            clean_startup_urls(&v),
            vec!["https://a.test".to_owned(), "ferrite://history".to_owned()]
        );
        assert!(clean_startup_urls(&serde_json::Value::Null).is_empty());
        let many: Vec<serde_json::Value> = (0..30)
            .map(|i| serde_json::Value::String(format!("https://x.test/{i}")))
            .collect();
        assert_eq!(
            clean_startup_urls(&serde_json::Value::Array(many)).len(),
            MAX_STARTUP_URLS
        );
    }

    #[test]
    fn translate_offer_needs_enabled_foreign_http() {
        // Same primary (incl. regional variants) never offers.
        assert!(!translate_offer(true, "https://a.de", Some("de"), "de"));
        assert!(!translate_offer(true, "https://a.br", Some("pt-BR"), "pt"));
        assert!(translate_offer(true, "https://a.de", Some("de-AT"), "en"));
        assert!(translate_offer(true, "https://a.de", Some("fr"), "en"));
        // Plain HTTP is a real page too: it offers (with the insecure mark).
        assert!(translate_offer(true, "http://a.de", Some("de"), "en"));
        // Disabled, privileged, missing, or target-less: no offer.
        assert!(!translate_offer(false, "https://a.de", Some("de"), "en"));
        assert!(!translate_offer(
            true,
            "ferrite://history",
            Some("de"),
            "en"
        ));
        assert!(!translate_offer(true, "https://a.de", None, "en"));
        assert!(!translate_offer(true, "https://a.de", Some("de"), ""));
    }

    #[test]
    fn omni_scope_parses_exact_tokens() {
        assert_eq!(omni_scope("@tabs foo"), (Some("tabs"), "foo".to_owned()));
        assert_eq!(omni_scope("@TABS  foo  "), (Some("tabs"), "foo".to_owned()));
        assert_eq!(omni_scope("@tabs"), (Some("tabs"), String::new()));
        assert_eq!(
            omni_scope("@bookmarks x"),
            (Some("bookmarks"), "x".to_owned())
        );
        assert_eq!(omni_scope("@history x"), (Some("history"), "x".to_owned()));
        // Near-misses stay unscoped (exact first token only).
        assert_eq!(omni_scope("@tab x"), (None, "@tab x".to_owned()));
        assert_eq!(omni_scope("@tabsfoo"), (None, "@tabsfoo".to_owned()));
        assert_eq!(omni_scope("hello @tabs"), (None, "hello @tabs".to_owned()));
        assert_eq!(omni_scope(""), (None, String::new()));
    }

    #[test]
    fn memory_pressure_is_use_fraction_or_nothing() {
        assert_eq!(memory_pressure(16, 8), Some(0.5));
        assert_eq!(memory_pressure(16, 16), Some(0.0));
        assert_eq!(memory_pressure(16, 0), Some(1.0));
        // Unknown/lying readings keep the previous value (None).
        assert_eq!(memory_pressure(0, 0), None);
        assert_eq!(memory_pressure(8, 16), None);
        assert!(MEM_SAMPLE_INTERVAL.as_secs() >= 5, "slow path only");
    }

    #[test]
    fn downloading_tabs_newest_event_wins() {
        use ferrite_tabs::{DownloadEvent, DownloadStateKind};
        use std::collections::VecDeque;
        let ev = |id: &str, tab: Option<&str>, state: DownloadStateKind| DownloadEvent {
            id: id.to_owned(),
            url: "https://x.test/f".to_owned(),
            filename: "f".to_owned(),
            path: None,
            tab_id: tab.map(str::to_owned),
            bytes_total: None,
            bytes_received: 0,
            state,
        };
        let t1 = uuid::Uuid::new_v4();
        let t2 = uuid::Uuid::new_v4();
        let log: VecDeque<DownloadEvent> = vec![
            ev("dl-1", Some(&t1.to_string()), DownloadStateKind::Started),
            ev("dl-2", Some(&t2.to_string()), DownloadStateKind::Progress),
            ev("dl-1", Some(&t1.to_string()), DownloadStateKind::Completed),
            ev("dl-3", None, DownloadStateKind::Progress),
        ]
        .into();
        let got = ShellApp::downloading_tabs(&log);
        // dl-1 completed (newest wins), dl-3 unattributed: only t2 stays.
        assert_eq!(got.len(), 1);
        assert!(got.contains(&t2));
        assert!(ShellApp::downloading_tabs(&VecDeque::new()).is_empty());
    }

    #[test]
    fn remote_fetch_never_fires_from_private_tabs() {
        assert!(remote_fetch_allowed(true, false, 5, true));
        assert!(remote_fetch_allowed(true, false, 2, true));
        assert!(!remote_fetch_allowed(true, false, 1, true));
        assert!(!remote_fetch_allowed(false, false, 9, true));
        // The point: a global opt-in must not exfiltrate private keystrokes.
        assert!(!remote_fetch_allowed(true, true, 9, true));
        assert!(!remote_fetch_allowed(true, true, 200, true));
        // Completions come from DuckDuckGo only: a Google/Brave default must
        // not send keystrokes to an engine the toolbar does not name.
        assert!(!remote_fetch_allowed(true, false, 9, false));
    }

    #[test]
    fn shield_page_host_matches_engine_attribution() {
        // Same key the engine bills under for top-level documents: badge,
        // dropdown, and pause switch agree by construction.
        assert_eq!(
            page_host_for_shield("https://Example.COM/a?b=1").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            page_host_for_shield("http://127.0.0.1:8080/x").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(page_host_for_shield("ferrite://settings"), None);
        assert_eq!(page_host_for_shield("ferrite://newtab"), None);
        assert_eq!(page_host_for_shield("not a url"), None);
        assert_eq!(page_host_for_shield(""), None);
    }

    #[test]
    fn session_temp_downloads_never_persist() {
        assert!(ShellApp::is_session_temp_download(Some(
            "C:\\Users\\a\\AppData\\Local\\Temp\\ferrite\\guest-123\\downloads\\a.zip"
        )));
        assert!(ShellApp::is_session_temp_download(Some(
            "/tmp/ferrite/private-456/file.pdf"
        )));
        assert!(!ShellApp::is_session_temp_download(Some(
            "C:\\Users\\a\\Downloads\\a.zip"
        )));
        assert!(!ShellApp::is_session_temp_download(None));
    }

    #[test]
    fn unknown_chrome_envelope_is_rejected() {
        assert!(ChromeCommand::parse(r#"{"cmd":"eval","js":"x"}"#).is_err());
    }
}

#[cfg(test)]
mod sync_trust_tests {
    use super::*;

    #[test]
    fn pubkey_fingerprint_matches_identity_shape() {
        let fp = fingerprint_pubkey(&[9u8; 32]);
        assert_eq!(fp.len(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        // Same shape as `DeviceIdentity::fingerprint`: both ends of a phone
        // call compare like with like.
        let id = ferrite_sync::DeviceIdentity::generate().unwrap();
        assert_eq!(fingerprint_pubkey(&id.public_key_bytes()), id.fingerprint());
    }

    #[test]
    fn lan_ip_is_empty_or_literal() {
        // Empty means "no listener info" downstream; anything else must be a
        // parseable literal (never a hostname that re-resolves).
        let ip = best_lan_ip();
        assert!(
            ip.is_empty() || ip.parse::<std::net::IpAddr>().is_ok(),
            "unexpected: {ip}"
        );
    }

    #[test]
    fn device_rows_sort_newest_first_missing_last() {
        let row = |at: Option<i64>| {
            let mut m = serde_json::Map::new();
            if let Some(at) = at {
                m.insert("first_paired_at".into(), at.into());
            }
            serde_json::Value::Object(m)
        };
        let mut rows = vec![row(Some(1)), row(None), row(Some(9)), row(Some(5))];
        sort_device_rows(&mut rows);
        let stamps: Vec<Option<i64>> = rows
            .iter()
            .map(|r| r.get("first_paired_at").and_then(|v| v.as_i64()))
            .collect();
        assert_eq!(stamps, vec![Some(9), Some(5), Some(1), None]);
    }

    #[test]
    fn pairing_code_round_trips_through_shell_gates() {
        // What `sync_pair_submit` accepts: decodable, keyed, bounded,
        // known version, fresh stamp.
        let id = ferrite_sync::DeviceIdentity::generate().unwrap();
        let payload = ferrite_sync::PairingPayload {
            ip: String::new(),
            port: 0,
            device_id: id.device_id.to_string(),
            public_key: id.public_key_bytes(),
            device_name: "test peer".to_owned(),
            version: ferrite_sync::pairing::PAIRING_VERSION,
            created_at_ms: Some(now_millis()),
        };
        let code = payload.encode();
        let back = ferrite_sync::PairingPayload::decode(&code).expect("decodable");
        assert_eq!(back.device_id, id.device_id.to_string());
        assert_eq!(back.public_key, id.public_key_bytes());
        assert!(back.is_fresh(now_millis()));
        assert!(ferrite_sync::PairingPayload::decode("!!!").is_none());
        assert!(ferrite_sync::PairingPayload::decode(&"x".repeat(5000)).is_none());
    }
}

/// Whether a suggestion URL is safe to navigate verbatim at commit time:
/// already a full URL on a navigable scheme. Anything else (bare phrases,
/// expressions, fragments) must go through classify instead.
fn is_navigable_suggestion(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("ferrite://")
}

/// Normalize a sleep-exception host: lowercase, no leading dot, no
/// path/port tail, no whitespace. None = unusable (empty, overlong,
/// spaces, controls). Single labels (`localhost`, intranet names) are
/// allowed; matching is exact-or-suffix in the manager.
fn clean_sleep_host(raw: &str) -> Option<String> {
    let mut h = raw.trim().trim_start_matches('.').to_lowercase();
    if let Some(slash) = h.find('/') {
        h.truncate(slash);
    }
    // Strip :port (digits only, single colon). Anything still carrying a
    // colon is unusable: IPv6 literals never suffix-match, so keeping one
    // would be a dead exception that pretends to protect.
    if let Some((head, port)) = h.split_once(':')
        && !head.contains(':')
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
    {
        h = head.to_owned();
    }
    if h.is_empty()
        || h.len() > 253
        || h.contains(':')
        || h.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    Some(h)
}

/// Omnibox source scope: `@tabs`, `@bookmarks`, `@history` as the first
/// token (case-insensitive), query is the rest. Returns (scope, query);
/// scope None means unscoped. `@tab`/`@tabsfoo` are not scopes (exact
/// token match only); `@tabs` alone scopes with an empty query (lists).
/// Pure for tests; the assembly below owns the budgets.
fn omni_scope(input: &str) -> (Option<&'static str>, String) {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('@') {
        return (None, input.to_owned());
    }
    let token: String = trimmed[1..]
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    let scope = match token.as_str() {
        "tabs" => Some("tabs"),
        "bookmarks" => Some("bookmarks"),
        "history" => Some("history"),
        _ => None,
    };
    match scope {
        Some(s) => {
            let rest = trimmed[1 + token.len()..].trim().to_owned();
            (Some(s), rest)
        }
        None => (None, input.to_owned()),
    }
}

/// Whether a remote-suggestion fetch may fire: explicit opt-in, a real
/// query, never from a private tab (global toggles must not leak private
/// keystrokes), and only when DuckDuckGo is the effective engine (the sole
/// suggestion backend: see `suggest`). Pure for tests; the arm owns the I/O.
fn remote_fetch_allowed(
    remote_enabled: bool,
    private_tab: bool,
    query_chars: usize,
    effective_is_ddg: bool,
) -> bool {
    remote_enabled && !private_tab && effective_is_ddg && query_chars >= 2
}

/// Translate offer for the toolbar button: translation enabled, a real
/// page (never privileged chrome), a validated page language whose
/// primary differs from the target's primary. Regional variants of the
/// target never offer (`pt` target silences `pt-BR` pages). Pure.
fn translate_offer(enabled: bool, url: &str, page_lang: Option<&str>, target: &str) -> bool {
    if !enabled {
        return false;
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    let src = page_lang.map(ferrite_tabs::translate::primary_lang);
    let tgt = ferrite_tabs::translate::primary_lang(target);
    match src {
        Some(s) if !s.is_empty() && !tgt.is_empty() => s != tgt,
        _ => false,
    }
}

/// Shield-attribution host for a tab URL: lowercase host of http(s) pages,
/// `None` for internal pages (nothing to attribute) and unparseable input.
/// Matches the engine's `page_host` for top-level documents (referrer-free),
/// so badge, dropdown, and pause switch all key identically.
fn page_host_for_shield(url: &str) -> Option<String> {
    let lower = url.trim().to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    url::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
        .filter(|h| !h.is_empty())
}

/// Zoom key for a tab URL: the origin for web pages (shared across the
/// site, subdomains distinct), the bare page for `ferrite://` internals
/// (`ferrite://settings`, query/fragment stripped so one archive view does
/// not mint a row per document). `None` for anything else. Pure.
fn zoom_key_for(url: &str) -> Option<String> {
    if let Some(key) = ferrite_tabs::origin_of(url) {
        return Some(key);
    }
    let lower = url.trim().to_lowercase();
    let rest = lower.strip_prefix("ferrite://")?;
    let page = rest.split(['/', '?', '#']).next().unwrap_or("");
    if page.is_empty() || page.contains(['@', ':', ' ']) {
        return None;
    }
    Some(format!("ferrite://{page}"))
}

/// Merge tab-switch rows with history rows: one URL never appears twice.
/// Tab rows win (switch, do not duplicate). Exact URL match only.
// ponytail: no trailing-slash or case normalization. Add when dupes show up.
fn dedup_suggestions(tabs: &[SugRow], history: Vec<SugRow>) -> Vec<SugRow> {
    let tab_urls: std::collections::HashSet<&str> = tabs.iter().map(|s| s.url.as_str()).collect();
    history
        .into_iter()
        .filter(|s| !tab_urls.contains(s.url.as_str()))
        .collect()
}

#[cfg(test)]
mod suggestion_merge_tests {
    use super::*;

    fn row(url: &str, kind: &str, tab: Option<&str>) -> SugRow {
        SugRow {
            url: url.to_owned(),
            title: url.to_owned(),
            tab: tab.map(|t| t.to_owned()),
            kind: kind.to_owned(),
            provider: None,
        }
    }

    #[test]
    fn tab_row_wins_over_same_url_history() {
        let tabs = vec![row("https://example.com", "tab", Some("t1"))];
        let history = vec![
            row("https://example.com", "history", None),
            row("https://other.test", "history", None),
        ];
        let merged = dedup_suggestions(&tabs, history);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].url, "https://other.test");
    }

    #[test]
    fn no_overlap_passes_through() {
        let tabs = vec![row("https://a.test", "tab", Some("t1"))];
        let history = vec![row("https://b.test", "history", None)];
        assert_eq!(dedup_suggestions(&tabs, history).len(), 1);
    }

    #[test]
    fn bookmark_dup_of_history_row_drops() {
        let existing = vec![
            row("https://open.test", "tab", Some("t1")),
            row("https://seen.test", "history", None),
        ];
        let bookmarks = vec![
            row("https://seen.test", "bookmark", None),
            row("https://fresh.test", "bookmark", None),
        ];
        let merged = dedup_suggestions(&existing, bookmarks);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].url, "https://fresh.test");
    }
}

#[cfg(test)]
mod af_dropdown_tests {
    use super::*;

    fn field(kind: &str, ac: &str, name: &str, id: &str) -> ScanField {
        ScanField {
            kind: kind.to_owned(),
            name: name.to_owned(),
            id: id.to_owned(),
            ac: ac.to_owned(),
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 20.0,
        }
    }

    #[test]
    fn sensitive_keys_refused() {
        for k in [
            "password",
            "passwd",
            "ssn",
            "cc-number",
            "cvc",
            "auth-token",
            "bank-account",
            "ACCOUNT",
            "Card Number",
        ] {
            assert!(ShellApp::sensitive_field_key(k), "{k}");
        }
        for k in ["q", "email", "username", "city", "color", "search"] {
            assert!(!ShellApp::sensitive_field_key(k), "{k}");
        }
    }

    #[test]
    fn form_key_prefers_ac_then_name_then_id() {
        assert_eq!(
            ShellApp::form_field_key("q", "search", "s"),
            Some("q".to_owned())
        );
        assert_eq!(
            ShellApp::form_field_key("", "search", "s"),
            Some("search".to_owned())
        );
        assert_eq!(ShellApp::form_field_key("", "", "s"), Some("s".to_owned()));
        assert_eq!(ShellApp::form_field_key("", "", ""), None);
    }

    #[test]
    fn selector_prefers_id_then_quoted_name() {
        assert_eq!(
            ShellApp::field_selector(&field("other", "", "", "u")),
            Some("#u".to_owned())
        );
        assert_eq!(
            ShellApp::field_selector(&field("other", "", "q", "")),
            Some("[name=\"q\"]".to_owned())
        );
        assert_eq!(ShellApp::field_selector(&field("other", "", "", "")), None);
    }
    #[test]
    fn flip_y_moves_above_only_when_needed() {
        // Fits below: unchanged.
        assert_eq!(flip_y(500.0, 480.0, 100.0, 88.0, 800.0), 500.0);
        // Overflows with room above: above the anchor.
        assert_eq!(flip_y(750.0, 746.0, 100.0, 88.0, 800.0), 646.0);
        // Overflows with no room above either: below (clamp copes).
        assert_eq!(flip_y(750.0, 120.0, 700.0, 88.0, 800.0), 750.0);
    }

    #[test]
    fn zoom_steps_through_presets_not_arithmetic() {
        assert_eq!(zoom_step(100, 1), 110);
        assert_eq!(zoom_step(100, -1), 90);
        assert_eq!(zoom_step(500, 1), 500);
        assert_eq!(zoom_step(25, -1), 25);
        // Mid-preset values snap to the next table entry.
        assert_eq!(zoom_step(120, 1), 125);
        assert_eq!(zoom_step(120, -1), 110);
        assert_eq!(zoom_step(33, 1), 50);
    }

    #[test]
    fn zoom_keys_split_origins_and_internal_pages() {
        // Web pages key by origin (subdomains and schemes stay distinct).
        assert_eq!(
            zoom_key_for("https://example.com/a").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            zoom_key_for("https://sub.example.com/").as_deref(),
            Some("https://sub.example.com")
        );
        assert_eq!(
            zoom_key_for("http://example.com/").as_deref(),
            Some("http://example.com")
        );
        // Internal pages key by bare page (queries never mint rows).
        assert_eq!(
            zoom_key_for("ferrite://settings").as_deref(),
            Some("ferrite://settings")
        );
        assert_eq!(
            zoom_key_for("ferrite://archive/view?url=https://x.test").as_deref(),
            Some("ferrite://archive")
        );
        assert_eq!(
            zoom_key_for("ferrite://newtab").as_deref(),
            Some("ferrite://newtab")
        );
        // Anything else has no key (chip stays hidden, shortcuts no-op).
        assert_eq!(zoom_key_for("about:blank"), None);
        assert_eq!(zoom_key_for(""), None);
        assert_eq!(zoom_key_for("ferrite://"), None);
    }

    #[test]
    fn af_filter_matches_labels_and_pins_footer() {
        let items = vec![
            serde_json::json!({"id": "af-cred:0", "label": "Login as alice"}),
            serde_json::json!({"id": "af-cred:1", "label": "Login as bob"}),
            serde_json::json!({"id": "af-manage", "label": "Manage passwords..."}),
        ];
        assert_eq!(ShellApp::af_visible(&items, "").len(), 3);
        let filtered = ShellApp::af_visible(&items, "ALI");
        assert_eq!(filtered.len(), 2);
        assert_eq!(
            filtered[0].get("id").and_then(|s| s.as_str()),
            Some("af-cred:0")
        );
        // Footer pins even when nothing matches.
        let only_footer = ShellApp::af_visible(&items, "zzz");
        assert_eq!(only_footer.len(), 1);
        assert_eq!(
            only_footer[0].get("id").and_then(|s| s.as_str()),
            Some("af-manage")
        );
    }

    #[test]
    fn saveable_url_allows_http_only() {
        assert!(saveable_url("https://a.test/x.png"));
        assert!(saveable_url("http://a.test/x"));
        assert!(!saveable_url("data:image/png;base64,AAA"));
        assert!(!saveable_url("blob:https://a.test/1"));
        assert!(!saveable_url("file:///C:/x.png"));
        assert!(!saveable_url("javascript:alert(1)"));
        assert!(!saveable_url(""));
    }
}
