//! System share dispatch. Native sheet on Windows and macOS, fallback page
//! elsewhere. This module never draws UI.

use std::sync::mpsc::Sender;

#[cfg(target_os = "macos")]
#[path = "share_mac.rs"]
mod platform_mac;
#[cfg(target_os = "windows")]
#[path = "share_win.rs"]
mod platform_win;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeShare {
    pub url: String,
    pub title: String,
    pub description: String,
}

impl From<ferrite_tabs::share::SharePayload> for NativeShare {
    fn from(p: ferrite_tabs::share::SharePayload) -> Self {
        Self {
            url: p.url,
            title: p.title,
            description: p.description,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShareError {
    #[error("system share failed: {0}")]
    Native(String),
    #[error("couldn't open your mail app")]
    MailOpen,
}

/// True on platforms with a real OS share sheet (Windows ShareUI via
/// `IDataTransferManagerInterop`, macOS `NSSharingServicePicker`).
/// Everywhere else the `ferrite://share` fallback page is the sheet.
pub fn system_share_available() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

/// Open the native sheet for `payload`, anchored to `window`. Never blocks
/// the loop: Windows runs ShareUI on a dedicated STA worker, macOS presents
/// synchronously on the main thread. Failures arrive as toasts.
pub fn show_native(payload: NativeShare, window: &winit::window::Window, notices: &Sender<String>) {
    #[cfg(target_os = "windows")]
    {
        match hwnd_of(window) {
            Some(hwnd) => platform_win::show(payload, hwnd, notices.clone()),
            None => {
                let _ = notices.send("Sharing isn't available right now.".to_owned());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        match ns_window_of(window) {
            Some(ptr) => {
                if let Err(e) = platform_mac::show(&payload, ptr) {
                    tracing::warn!(error = %e, "macos share failed");
                    let _ = notices.send(ShareError::Native(e).to_string());
                }
            }
            None => {
                let _ = notices.send("Sharing isn't available right now.".to_owned());
            }
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (payload, window, notices);
    }
}

/// Open a `mailto:` link in the default mail client. Restricted to the
/// `mailto:` scheme: page content must never gain an open-redirect into
/// arbitrary schemes through this path.
pub fn launch_mailto(mailto: &str) -> Result<(), ShareError> {
    if !mailto.starts_with("mailto:?") || mailto.chars().count() > 4096 {
        return Err(ShareError::MailOpen);
    }
    #[cfg(target_os = "windows")]
    {
        platform_win::open_url(mailto).map_err(|_| ShareError::MailOpen)
    }
    #[cfg(target_os = "macos")]
    {
        platform_mac::open_url(mailto).map_err(|_| ShareError::MailOpen)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(mailto)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| ShareError::MailOpen)?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn hwnd_of(window: &winit::window::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn ns_window_of(window: &winit::window::Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::AppKit(h) => Some(h.ns_window.as_ptr() as *mut _),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_matches_platform() {
        assert_eq!(
            system_share_available(),
            cfg!(any(target_os = "windows", target_os = "macos"))
        );
    }

    #[test]
    fn native_payload_converts() {
        let p = ferrite_tabs::share::share_payload("https://example.com/", "T", None).unwrap();
        let n = NativeShare::from(p);
        assert_eq!(n.url, "https://example.com/");
        assert_eq!(n.title, "T");
    }

    #[test]
    fn mailto_rejects_non_mailto() {
        assert!(launch_mailto("https://example.com/").is_err());
        assert!(launch_mailto("mailto:evil@example.com").is_err());
        assert!(launch_mailto(&format!("mailto:?{}", "x".repeat(5000))).is_err());
    }

    #[test]
    fn mailto_error_displays() {
        assert_eq!(
            format!("{}", ShareError::MailOpen),
            "couldn't open your mail app"
        );
        assert_eq!(
            format!("{}", ShareError::Native("x".to_owned())),
            "system share failed: x"
        );
    }
}
