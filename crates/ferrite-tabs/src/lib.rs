//! Content layer: tabs, navigation, and webview abstraction.

pub mod a11y;
pub mod dialog;
#[cfg(target_os = "windows")]
pub mod dialog_win;
pub mod downloads;
pub mod engine;
pub mod error;
pub mod ipc;
pub mod manager;
pub mod media;
pub mod navigation;
pub mod pdf;
pub mod permissions;
pub mod pip;
pub mod print;
pub mod protocol;
pub mod share;
pub mod spatial;
pub mod tab;
pub mod translate;
pub mod webview;

pub use dialog::{
    BEFOREUNLOAD_TEXT, DIALOG_MESSAGE_CAP, DIALOG_QUEUE_CAP, DialogKind, DialogOpened, DialogPush,
    DialogRequest, DialogStore, SUPPRESS_WINDOW, origin_key,
};
pub use downloads::{
    DownloadEvent, DownloadManager, DownloadStateKind, decide_destination, default_download_dir,
    is_dangerous_ext, mark_zone_identifier, partial_path_for, promote_partial, sanitize_filename,
    sha256_file, should_block_mixed_content, unique_path, verify_zone_identifier,
};
pub use engine::webview2_runtime_version;
pub use error::{TabError, TabResult};
pub use ipc::{
    IPC_MAX_PAYLOAD_BYTES, IPC_MAX_TITLE_CHARS, IPC_PER_TAB_PER_SEC, IPC_QUEUE_CAP, IpcBridge,
    SharedLimitsHandle,
};
pub use manager::TabManager;
pub use navigation::NavigationHistory;
pub use permissions::{
    COOLDOWN_MS, GrantSource, PendingRequest, PermissionStore, SnapshotSink, StoredGrant,
    kind_label, origin_of,
};
pub use pip::{
    PIP_MAX_CANDIDATES, PIP_MIN_AREA, PIP_MIN_EDGE, PipCandidate, PipReport, candidate_host,
    host_is_trackerish, is_ad_hint, parse_report, pick_candidate, pip_enter_js, pip_exit_js,
    pip_report_js, score_candidate,
};
pub use spatial::{LogicalRect, logical_to_wry_rect};
pub use tab::{TabBackend, TabLifecycle, TabState, now_millis};
pub use webview::{
    DeniedDownload, IpcMessage, MockFactory, MockHandle, PdfIntercept, WebViewFactory,
    WebViewHandle, WryFactory, WryHandle, audio_listener_js, link_hover_js, live_theme_script,
};
