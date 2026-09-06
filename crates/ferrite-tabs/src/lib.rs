//! Content layer: tabs, navigation, and webview abstraction.

pub mod downloads;
pub mod error;
pub mod ipc;
pub mod manager;
pub mod navigation;
pub mod protocol;
pub mod spatial;
pub mod tab;
pub mod webview;

pub use downloads::{
    DownloadEvent, DownloadManager, DownloadStateKind, default_download_dir, sanitize_filename,
};
pub use error::{TabError, TabResult};
pub use ipc::{
    IPC_MAX_PAYLOAD_BYTES, IPC_MAX_TITLE_CHARS, IPC_PER_TAB_PER_SEC, IPC_QUEUE_CAP, IpcBridge,
    SharedLimitsHandle,
};
pub use manager::TabManager;
pub use navigation::NavigationHistory;
pub use spatial::egui_rect_to_wry_rect;
pub use tab::{TabLifecycle, TabState};
pub use webview::{
    IpcMessage, MockFactory, MockHandle, WebViewFactory, WebViewHandle, WryFactory, WryHandle,
    audio_listener_js,
};
