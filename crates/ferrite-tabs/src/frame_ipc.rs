//! WebView2 child-frame IPC.
//!
//! wry installs its `window.ipc` shim in every frame but subscribes only to
//! the top-level `ICoreWebView2::WebMessageReceived`. WebView2 routes a child
//! frame's `chrome.webview.postMessage` to
//! `ICoreWebView2Frame2::WebMessageReceived`, so without this bridge a
//! shortcut pressed while focus is inside an iframe is captured by the
//! reporter and then silently lost. The same gap hides the iframe credential
//! scanner's origin-keyed save reports.
//!
//! Attach runs on the UI thread immediately after the webview is built and
//! before the event loop can create the first iframe. WebView2's top-level
//! `FrameCreated` reports direct children only; `ICoreWebView2Frame7` adds a
//! child-frame event, so attachment recurses through the frame tree. Every
//! frame message takes the same bounded, rate-limited, token-verified path as
//! a top-frame message, after a frame-scope type gate.
//!
//! Nested (second-level and deeper) iframe `postMessage` delivery is not
//! supported by the current WebView2 runtime: the frame is created and its
//! handlers attach, but the message never reaches the host. The recursion is
//! kept so that a runtime that adds delivery needs no code change; the
//! limitation is recorded in `tasks/keyboard-audit.md`.

use std::sync::Arc;

use tokio::sync::mpsc::Sender;
use tracing::{debug, warn};
use uuid::Uuid;
use webview2_com::{
    FrameChildFrameCreatedEventHandler, FrameCreatedEventHandler,
    FrameWebMessageReceivedEventHandler,
    Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2_4, ICoreWebView2Frame, ICoreWebView2Frame2,
        ICoreWebView2Frame7, ICoreWebView2FrameCreatedEventArgs,
        ICoreWebView2WebMessageReceivedEventArgs,
    },
    take_pwstr,
};
use windows::core::{Interface, PWSTR};
use wry::WebViewExtWindows;

use crate::ipc::{SharedLimitsHandle, deliver_webview_message};
use crate::webview::IpcMessage;

/// The only message types a child frame may inject into the tab's IPC stream.
///
/// A content page can already post every one of these from the top document;
/// the gate keeps a third-party iframe from spoofing top-frame-only reports
/// (`nav`, `page_loaded`, `title`, `scroll`, `form_dirty`, ...). The two
/// subframe init scripts are the shortcut reporter and the credential field
/// scanner; this is the subset of their vocabulary that `content_page_allowed`
/// admits (the scanner's `blob_*` types are privileged-only). Each type is
/// re-validated by its shell arm; this gate only narrows the sender set.
const FRAME_MESSAGE_TYPES: &[&str] = &[
    "shortcut",
    "autofill_scan",
    "autofill_focus",
    "autofill_dismiss",
    "af_nav",
    "af_pick",
    "af_filter",
    "form_history_save",
    "password_save_candidate",
];

/// Default-deny frame payload gate. Parse failures and non-string `type`
/// values are dropped; a frame cannot smuggle a top-frame report through a
/// malformed envelope.
fn frame_payload_allowed(payload: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    value
        .get("type")
        .and_then(|kind| kind.as_str())
        .is_some_and(|kind| FRAME_MESSAGE_TYPES.contains(&kind))
}

/// Attach the child-frame message bridge to a webview.
///
/// Must be called on the WebView2 UI thread directly after the webview is
/// built. Frames created after this point are caught by `FrameCreated`; the
/// initial navigation has not been pumped yet, so no frame can have been
/// created between `build_as_child` returning and this call.
pub(crate) fn attach(
    webview: &wry::WebView,
    tab_id: Uuid,
    sender: Sender<IpcMessage>,
    limits: SharedLimitsHandle,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
    tab_hint: String,
) -> windows::core::Result<()> {
    let core = webview.webview();
    let core4 = core.cast::<ICoreWebView2_4>()?;
    let tab_hint: Arc<str> = Arc::from(tab_hint);
    let handler = FrameCreatedEventHandler::create(Box::new({
        let sender = sender.clone();
        let limits = limits.clone();
        let waker = waker.clone();
        let tab_hint = Arc::clone(&tab_hint);
        move |_sender: Option<ICoreWebView2>, args: Option<ICoreWebView2FrameCreatedEventArgs>| {
            if let Some(args) = args
                && let Ok(frame) = unsafe { args.Frame() }
            {
                attach_frame(
                    &frame,
                    tab_id,
                    sender.clone(),
                    limits.clone(),
                    waker.clone(),
                    Arc::clone(&tab_hint),
                );
            }
            Ok(())
        }
    }));
    let mut token = 0;
    unsafe { core4.add_FrameCreated(&handler, &mut token) }?;
    debug!(tab_id = %tab_id, "frame IPC bridge attached");
    Ok(())
}

fn attach_frame(
    frame: &ICoreWebView2Frame,
    tab_id: Uuid,
    sender: Sender<IpcMessage>,
    limits: SharedLimitsHandle,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
    tab_hint: Arc<str>,
) {
    // The frame message interface is older than the nested-frame event;
    // attach it independently so an older runtime still gets first-level
    // iframe reports.
    if let Ok(frame2) = frame.cast::<ICoreWebView2Frame2>() {
        let handler = FrameWebMessageReceivedEventHandler::create(Box::new({
            let sender = sender.clone();
            let limits = limits.clone();
            let waker = waker.clone();
            let tab_hint = Arc::clone(&tab_hint);
            move |_sender: Option<ICoreWebView2Frame>,
                  args: Option<ICoreWebView2WebMessageReceivedEventArgs>| {
                let Some(args) = args else {
                    return Ok(());
                };
                let mut payload = PWSTR::null();
                if unsafe { args.TryGetWebMessageAsString(&mut payload) }.is_err() {
                    return Ok(());
                }
                let payload = take_pwstr(payload);
                if !frame_payload_allowed(&payload) {
                    debug!(tab_id = %tab_id, "frame IPC type not allowed");
                    return Ok(());
                }
                deliver_webview_message(
                    &sender,
                    &limits,
                    waker.as_ref(),
                    IpcMessage { tab_id, payload },
                    &tab_hint,
                );
                Ok(())
            }
        }));
        let mut token = 0;
        if let Err(e) = unsafe { frame2.add_WebMessageReceived(&handler, &mut token) } {
            warn!(tab_id = %tab_id, error = %e, "frame message bridge unavailable");
        }
    }

    // First-level FrameCreated only covers direct children. Recurse where the
    // runtime exposes the nested-frame event (WebView2 1.0.3240+).
    if let Ok(frame7) = frame.cast::<ICoreWebView2Frame7>() {
        let handler = FrameChildFrameCreatedEventHandler::create(Box::new({
            let sender = sender.clone();
            let limits = limits.clone();
            let waker = waker.clone();
            let tab_hint = Arc::clone(&tab_hint);
            move |_sender: Option<ICoreWebView2Frame>,
                  args: Option<ICoreWebView2FrameCreatedEventArgs>| {
                if let Some(args) = args
                    && let Ok(child) = unsafe { args.Frame() }
                {
                    attach_frame(
                        &child,
                        tab_id,
                        sender.clone(),
                        limits.clone(),
                        waker.clone(),
                        Arc::clone(&tab_hint),
                    );
                }
                Ok(())
            }
        }));
        let mut token = 0;
        if let Err(e) = unsafe { frame7.add_FrameCreated(&handler, &mut token) } {
            debug!(tab_id = %tab_id, error = %e, "nested frame events unavailable");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_gate_admits_only_subframe_reporter_types() {
        for kind in FRAME_MESSAGE_TYPES {
            let payload = format!(r#"{{"type":"{kind}"}}"#);
            assert!(frame_payload_allowed(&payload), "admitted: {kind}");
        }
        for kind in [
            "nav",
            "page_load",
            "page_loaded",
            "title",
            "scroll",
            "form_dirty",
            "console_error",
            "frame_load",
            "blob_chunk",
            "blob_end",
        ] {
            let payload = format!(r#"{{"type":"{kind}"}}"#);
            assert!(!frame_payload_allowed(&payload), "blocked: {kind}");
        }
        assert!(!frame_payload_allowed("not json"));
        assert!(!frame_payload_allowed(r#"{"type":1}"#));
        assert!(!frame_payload_allowed(r#"{"kind":"shortcut"}"#));
    }
}
