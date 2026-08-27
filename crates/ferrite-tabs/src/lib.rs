//! Content layer: tabs, navigation, and webview abstraction.

pub mod error;
pub mod ipc;
pub mod manager;
pub mod navigation;
pub mod protocol;
pub mod spatial;
pub mod tab;
pub mod webview;

pub use error::{TabError, TabResult};
pub use ipc::IpcBridge;
pub use manager::TabManager;
pub use navigation::NavigationHistory;
pub use spatial::egui_rect_to_wry_rect;
pub use tab::{TabLifecycle, TabState};
pub use webview::{
    IpcMessage, MockFactory, MockHandle, WebViewFactory, WebViewHandle, WryFactory, WryHandle,
    audio_listener_js,
};
