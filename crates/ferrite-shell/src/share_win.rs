//! Windows native share via `IDataTransferManagerInterop` (`GetForWindow`
//! plus `ShowShareUIForWindow` with our HWND). Dedicated STA worker, never
//! the winit loop. Failures return as toast text.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};

use windows::{
    ApplicationModel::DataTransfer::{DataPackage, DataRequestedEventArgs, DataTransferManager},
    Foundation::{TypedEventHandler, Uri},
    Win32::{
        Foundation::HWND,
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::{
            Shell::{IDataTransferManagerInterop, ShellExecuteW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, PCWSTR, factory},
};

use super::NativeShare;

/// RAII COM apartment: STA for the worker lifetime, uninitialized after.
struct ComApartment;

impl ComApartment {
    /// # Safety
    /// Must run on a thread whose COM apartment is still uninitialized,
    /// callers guarantee this by using a freshly spawned worker.
    unsafe fn enter() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

/// One sheet at a time: a second `ShowShareUIForWindow` for the same HWND
/// while the first is still open fails inside the interop ("service is
/// already registered"). The claim is process-wide because the sheet is
/// window-modal; rapid double-clicks and keyboard repeats land here.
static SHARE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Try to claim the sheet. `true` means the caller owns it and must call
/// [`release_share`] when the worker finishes, come what may.
fn try_claim_share() -> bool {
    SHARE_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
}

fn release_share() {
    SHARE_IN_FLIGHT.store(false, Ordering::Release);
}

/// Show the system share sheet. Returns immediately. The worker owns the
/// modal UI lifetime and reports failures through `notices`.
pub fn show(payload: NativeShare, hwnd: isize, notices: Sender<String>) {
    if !try_claim_share() {
        let _ = notices.send("The share sheet is already open.".to_owned());
        return;
    }
    let fallback = notices.clone();
    let worker = std::thread::Builder::new()
        .name("ferrite-share".into())
        .spawn(move || {
            if let Err(e) = show_blocking(&payload, hwnd) {
                tracing::warn!(error = %e, "windows share failed");
                let _ = notices.send(super::ShareError::Native(e.to_string()).to_string());
            }
            release_share();
        });
    if worker.is_err() {
        release_share();
        let _ = fallback.send("Sharing isn't available right now.".to_owned());
    }
}

fn show_blocking(p: &NativeShare, hwnd: isize) -> windows::core::Result<()> {
    // SAFETY: fresh worker thread, no COM apartment exists here yet.
    let _apt = unsafe { ComApartment::enter()? };
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    let interop: IDataTransferManagerInterop =
        factory::<DataTransferManager, IDataTransferManagerInterop>()?;
    // SAFETY: `hwnd` is our live winit window; the manager outlives the
    // modal `ShowShareUIForWindow` call below.
    let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd)? };
    let title = HSTRING::from(p.title.as_str());
    let text = HSTRING::from(p.url.as_str());
    let desc = HSTRING::from(p.description.as_str());
    let html = HSTRING::from(html_link(&p.url, &p.title).as_str());
    let _token = manager.DataRequested(&TypedEventHandler::new(
        move |_, args: windows::core::Ref<'_, DataRequestedEventArgs>| {
            let args = args.ok()?;
            let req = args.Request()?;
            let package = DataPackage::new()?;
            let props = package.Properties()?;
            props.SetTitle(&title)?;
            if !desc.is_empty() {
                props.SetDescription(&desc)?;
            }
            // Rich (link), plain (text), and HTML renderings: targets pick
            // the richest format they understand, same content in each.
            package.SetWebLink(&Uri::CreateUri(&text)?)?;
            package.SetText(&text)?;
            package.SetHtmlFormat(&html)?;
            req.SetData(&package)?;
            Ok(())
        },
    ))?;
    // SAFETY: same live window as above; blocks until dismissal.
    unsafe {
        interop.ShowShareUIForWindow(hwnd)?;
    }
    Ok(())
}

/// Minimal HTML rendering of the link for `SetHtmlFormat`. Page-controlled
/// values are escaped, the picker hands this to other apps verbatim.
fn html_link(url: &str, title: &str) -> String {
    format!(
        "<a href=\"{}\">{}</a>",
        ferrite_tabs::protocol::html_escape(url),
        ferrite_tabs::protocol::html_escape(title)
    )
}

/// Open a URL with the default handler (the `mailto:` path). `None` parent:
/// the mail client owns its compose window, not us.
pub fn open_url(url: &str) -> windows::core::Result<()> {
    use windows::core::w;
    let file = HSTRING::from(url);
    // SAFETY: string args borrow for the call; `ShellExecuteW` is
    // synchronous in its argument use.
    let rc = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 are success (`HINSTANCE` semantics, not HRESULT).
    if (rc.0 as usize) > 32 {
        Ok(())
    } else {
        Err(windows::core::Error::from_win32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_claim_is_exclusive_until_released() {
        assert!(try_claim_share(), "first claim owns the sheet");
        assert!(!try_claim_share(), "second claim refused while open");
        release_share();
        assert!(try_claim_share(), "released sheet claims again");
        release_share();
    }
}
