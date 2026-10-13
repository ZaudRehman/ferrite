//! macOS native share via `NSSharingServicePicker` at the cursor. Main
//! thread only, called synchronously from winit event handlers.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSEvent, NSSharingServicePicker, NSView, NSWindow, NSWorkspace};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};

use super::NativeShare;

/// Show the system share sheet for `payload` in `ns_view`.
/// # Safety
/// `ns_view` must point to a live `NSView` on the main thread. The parent
/// window is resolved from the view, so the caller passes the content view
/// it already holds (raw-window-handle no longer exposes the window).
pub fn show(payload: &NativeShare, ns_view: *mut std::ffi::c_void) -> Result<(), String> {
    let mtm =
        MainThreadMarker::new().ok_or_else(|| "sharing must run on the main thread".to_owned())?;
    // SAFETY: caller guarantees a live main-thread view for the call.
    let view: &NSView = unsafe { &*ns_view.cast::<NSView>() };
    let window: Retained<NSWindow> = view
        .window()
        .ok_or_else(|| "view is not installed in a window".to_owned())?;

    let url_text = NSString::from_str(&payload.url);
    let url =
        NSURL::URLWithString(&url_text).ok_or_else(|| "link is not a valid URL".to_owned())?;
    let title = NSString::from_str(&payload.title);
    // Heterogeneous items share only `AnyObject`: the array literal is a
    // coercion site, so each element deref-coerces up its superclass chain.
    let items: [&AnyObject; 2] = [&*url, &*title];
    let array = NSArray::from_slice(&items);
    // SAFETY: every item is an `NSPasteboardWriting` type (`NSURL`,
    // `NSString`), satisfying `initWithItems:`'s contract.
    let picker = unsafe { NSSharingServicePicker::alloc().initWithItems(&array) };

    // Anchor at the cursor: screen point → window base → view coords, in a
    // 1×1 rect so the popover hugs the click that summoned it.
    let screen = NSEvent::mouseLocation();
    let in_window = window.convertPointFromScreen(screen);
    let in_view = view.convertPoint_fromView(in_window, None);
    let rect = NSRect {
        origin: NSPoint {
            x: in_view.x,
            y: in_view.y,
        },
        size: NSSize {
            width: 1.0,
            height: 1.0,
        },
    };
    picker.showRelativeToRect_ofView_preferredEdge(rect, view, NSRectEdge::NSMaxYEdge);
    let _ = mtm;
    Ok(())
}

/// Open a URL with the default handler (the `mailto:` path).
pub fn open_url(url: &str) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or_else(|| "must run on the main thread".to_owned())?;
    // Held for the call: AppKit requires the main thread even where the
    // generated bindings do not take the marker.
    let _ = mtm;
    let text = NSString::from_str(url);
    let nsurl = NSURL::URLWithString(&text).ok_or_else(|| "bad URL".to_owned())?;
    if NSWorkspace::sharedWorkspace().openURL(&nsurl) {
        Ok(())
    } else {
        Err("the system refused to open the link".to_owned())
    }
}
