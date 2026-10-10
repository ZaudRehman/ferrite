//! OS clipboard backend. Plain writes for public text. Sensitive writes
//! (vault secrets) take Windows history and cloud opt-outs plus 45s
//! auto-clear. All writes happen in Rust from explicit UI actions.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use zeroize::Zeroize;

/// Seconds a sensitive copy lives before auto-clear (password-manager
/// convention is 30 to 60s).
pub const SENSITIVE_CLEAR_SECS: u64 = 45;
/// Window after a sensitive copy in which an unexpected replacement is
/// surfaced (pastejacking signal, not the full TTL).
const CLOBBER_WATCH_SECS: u64 = 2;
/// Largest single copy. Secrets are tens to hundreds of bytes. Anything
/// bigger through here is a compromised page stuffing the clipboard.
pub const MAX_COPY_BYTES: usize = 16 * 1024;
/// Windows opt-out formats, exact names. Presence-only flag first, DWORD-0
/// pair after. Set all three, never assume one covers the other.
#[cfg(windows)]
pub const PRIVACY_FORMATS: [&str; 3] = [
    "ExcludeClipboardContentFromMonitorProcessing",
    "CanIncludeInClipboardHistory",
    "CanUploadToCloudClipboard",
];

static EPOCH: OnceLock<AtomicU64> = OnceLock::new();
static CLOBBER: OnceLock<AtomicBool> = OnceLock::new();

fn epoch() -> &'static AtomicU64 {
    EPOCH.get_or_init(|| AtomicU64::new(0))
}

fn clobber() -> &'static AtomicBool {
    CLOBBER.get_or_init(|| AtomicBool::new(false))
}

/// Next clipboard generation. Every Rust-side write bumps it so a stale
/// auto-clear timer can never wipe a newer copy.
pub(crate) fn bump_epoch() -> u64 {
    epoch().fetch_add(1, Ordering::SeqCst) + 1
}

fn current_epoch() -> u64 {
    epoch().load(Ordering::SeqCst)
}

/// Empty/oversize gate shared by both write paths. Empty is a no-op `false`,
/// never an error popup.
pub(crate) fn check_copyable(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_COPY_BYTES
}

/// Plain write: links, URLs, diagnostics, usernames. No privacy flags, no
/// timer. The default for everything non-secret.
pub fn copy_plain(text: &str) -> bool {
    if !check_copyable(text) {
        return false;
    }
    bump_epoch();
    write_text(text)
}

/// Sensitive write: vault passwords, card PANs, TOTP codes. Private formats
/// on Windows, 45s clear-if-ours everywhere, plus a clobber watch that flags
/// an unexpected replacement within seconds (the pastejacking signal).
pub fn copy_sensitive(text: &str) -> bool {
    if !check_copyable(text) {
        return false;
    }
    if !write_private(text) {
        return false;
    }
    let generation = bump_epoch();
    let mut expected = text.to_owned();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(CLOBBER_WATCH_SECS));
        if current_epoch() == generation {
            let replaced = read_text().is_none_or(|cur| cur != expected);
            if replaced {
                clobber().store(true, Ordering::SeqCst);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(
            SENSITIVE_CLEAR_SECS - CLOBBER_WATCH_SECS,
        ));
        if current_epoch() == generation
            && let Some(cur) = read_text()
            && cur == expected
        {
            clear();
        }
        expected.zeroize();
    });
    true
}

/// Drain the clobber flag (shell tick turns it into one toast). One-shot.
pub fn take_clobber_notice() -> bool {
    clobber().swap(false, Ordering::SeqCst)
}

/// Auto-clear horizon, for toast copy ("clears in 45s").
pub fn sensitive_clear_secs() -> u64 {
    SENSITIVE_CLEAR_SECS
}

fn write_text(text: &str) -> bool {
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        Ok(()) => {
            tracing::debug!("copied to clipboard");
            true
        }
        Err(e) => {
            tracing::debug!(error = %e, "clipboard copy failed");
            false
        }
    }
}

fn read_text() -> Option<String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .ok()
}

fn clear() {
    #[cfg(windows)]
    if clear_windows() {
        return;
    }
    if let Ok(mut c) = arboard::Clipboard::new()
        && let Err(e) = c.clear()
    {
        tracing::debug!(error = %e, "clipboard auto-clear failed");
    }
}

#[cfg(windows)]
fn write_private(text: &str) -> bool {
    write_windows_private(text)
}

#[cfg(not(windows))]
fn write_private(text: &str) -> bool {
    write_text(text)
}

#[cfg(windows)]
fn clear_windows() -> bool {
    use windows::Win32::System::DataExchange::*;
    // Best effort: if another app holds the clipboard the arboard
    // fallback below retries it instead.
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let ok = EmptyClipboard().is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// `CF_UNICODETEXT` (WinUser.h value 13). The `windows` crate does not
/// export the `CF_*` constants, so the value is pinned here with its source.
#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;

/// Release a `GlobalAlloc` handle the clipboard did not take ownership of.
/// (`GlobalFree` has no binding in `windows` 0.61. `GlobalAlloc` blocks come
/// from the process heap, so `HeapFree` on it is the documented pairing.)
#[cfg(windows)]
unsafe fn free_global(h: windows::Win32::Foundation::HGLOBAL) {
    use windows::Win32::System::Memory::*;
    unsafe {
        if let Ok(heap) = GetProcessHeap()
            && HeapFree(heap, HEAP_FLAGS(0), Some(h.0 as *const _)).is_err()
        {
            tracing::debug!("clipboard handle free failed");
        }
    }
}

/// Single `OpenClipboard` sequence: text plus all three opt-out formats
/// before `CloseClipboard`. Any failure fails closed (honest toast) rather
/// than writing the secret without privacy flags.
#[cfg(windows)]
fn write_windows_private(text: &str) -> bool {
    use windows::Win32::System::DataExchange::*;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = wide.len().saturating_mul(2);
    if byte_len <= 2 || byte_len > (MAX_COPY_BYTES + 1) * 2 {
        return false;
    }
    unsafe {
        // Another app may hold the clipboard. Retry briefly, then refuse
        // rather than fall back to an unflagged write.
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            tracing::debug!("clipboard busy, sensitive copy refused");
            return false;
        }
        let ok = write_sequence(&wide, byte_len);
        let _ = CloseClipboard();
        if !ok {
            tracing::debug!("sensitive clipboard write failed");
        }
        ok
    }
}

/// Caller holds the clipboard open. Every path below must reach
/// `CloseClipboard` in the caller. Text first, privacy formats after.
#[cfg(windows)]
unsafe fn write_sequence(wide: &[u16], byte_len: usize) -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;

    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let text_mem = match GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, byte_len) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let dst = GlobalLock(text_mem) as *mut u16;
        if dst.is_null() {
            free_global(text_mem);
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        let _ = GlobalUnlock(text_mem);
        if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(text_mem.0 as *mut _))).is_err() {
            free_global(text_mem);
            return false;
        }
        // Text is now owned by the clipboard. Privacy flags are best-effort
        // individually. One failing must not drop the text. Survivors still
        // narrow exposure.
        set_format_flag(PRIVACY_FORMATS[0], 0);
        set_format_flag(PRIVACY_FORMATS[1], 1);
        set_format_flag(PRIVACY_FORMATS[2], 1);
        true
    }
}

/// Register one opt-out format and set it: presence-only when `dwords` is 0
/// (1 zeroed byte, content irrelevant), explicit DWORD 0 otherwise.
#[cfg(windows)]
unsafe fn set_format_flag(name: &str, dwords: usize) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;
    use windows::core::PCWSTR;

    unsafe {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let format = RegisterClipboardFormatW(PCWSTR::from_raw(wide.as_ptr()));
        if format == 0 {
            return;
        }
        let size = if dwords == 0 { 1 } else { 4 * dwords };
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, size) else {
            return;
        };
        if SetClipboardData(format, Some(HANDLE(mem.0 as *mut _))).is_err() {
            free_global(mem);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_oversize_are_not_copyable() {
        assert!(!check_copyable(""));
        assert!(!check_copyable("   "));
        assert!(check_copyable("x"));
        assert!(!check_copyable(&"x".repeat(MAX_COPY_BYTES + 1)));
        assert!(check_copyable(&"x".repeat(MAX_COPY_BYTES)));
    }

    #[test]
    fn read_gating_contract_lives_in_the_broker() {
        // Clipboard reads prompt like camera/mic (broker owns the kinds);
        // writes have no kind, so nothing can queue one.
        assert_eq!(
            ferrite_tabs::permissions::kind_label("clipboard-read"),
            "Clipboard"
        );
    }

    #[test]
    fn sensitive_window_covers_convention() {
        assert!((30..=60).contains(&SENSITIVE_CLEAR_SECS));
        const {
            assert!(CLOBBER_WATCH_SECS < SENSITIVE_CLEAR_SECS);
        }
    }

    #[test]
    fn epoch_is_monotonic() {
        assert!(bump_epoch() < bump_epoch());
    }

    #[test]
    fn clobber_notice_is_one_shot() {
        clobber().store(true, Ordering::SeqCst);
        assert!(take_clobber_notice());
        assert!(!take_clobber_notice());
    }

    #[test]
    fn rejected_copies_touch_no_os_state() {
        assert!(!copy_plain(""));
        assert!(!copy_sensitive(""));
        assert!(!copy_sensitive(&"x".repeat(MAX_COPY_BYTES + 1)));
    }

    #[cfg(windows)]
    #[test]
    fn privacy_format_names_are_exact() {
        assert_eq!(
            PRIVACY_FORMATS,
            [
                "ExcludeClipboardContentFromMonitorProcessing",
                "CanIncludeInClipboardHistory",
                "CanUploadToCloudClipboard",
            ]
        );
    }
}
