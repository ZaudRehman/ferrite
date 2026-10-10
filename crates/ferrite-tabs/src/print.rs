//! Print backend. Reachable path is the engine dialog only. No silent path,
//! no settings panel: wry exposes neither. Guard plus preflight live here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Stale entries evict on next access, so a tab never wedges.
pub const PRINT_JOB_STALE: Duration = Duration::from_secs(5 * 60);

/// Max spooler bytes parsed per call.
pub const MAX_ENUM_BYTES: u32 = 1024 * 1024;

/// Max printer name chars kept. Bounds logs and UI.
pub const MAX_PRINTER_NAME_CHARS: usize = 256;

/// One in flight print per tab. Time injected for tests.
#[derive(Debug, Default)]
pub struct PrintGuard {
    in_flight: HashMap<uuid::Uuid, Instant>,
}

impl PrintGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the tab for a print run. Returns false when a live job is
    /// already in flight (caller toasts "already in progress").
    pub fn begin(&mut self, tab: uuid::Uuid, now: Instant) -> bool {
        self.evict_stale(now);
        if self.in_flight.contains_key(&tab) {
            return false;
        }
        self.in_flight.insert(tab, now);
        true
    }

    /// Release the tab (job failed to open, navigation committed).
    pub fn finish(&mut self, tab: &uuid::Uuid) {
        self.in_flight.remove(tab);
    }

    /// Whether the tab currently holds a live job.
    pub fn is_printing(&mut self, tab: &uuid::Uuid, now: Instant) -> bool {
        self.evict_stale(now);
        self.in_flight.contains_key(tab)
    }

    /// How long the tab's held job has been in flight (None when free).
    /// Lets callers tell "dialog open 5s" (wait) from "dialog dismissed
    /// an hour ago, engine never reported back" (retry): wry exposes no
    /// print result, so age is the only dismissal signal available.
    pub fn held_for(&mut self, tab: &uuid::Uuid, now: Instant) -> Option<Duration> {
        self.evict_stale(now);
        self.in_flight
            .get(tab)
            .and_then(|t| now.checked_duration_since(*t))
    }

    fn evict_stale(&mut self, now: Instant) {
        self.in_flight
            .retain(|_, started| now.duration_since(*started) < PRINT_JOB_STALE);
    }
}

/// One installed printer (name only: the dialog owns every other property,
/// so capabilities are deliberately not modeled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterInfo {
    pub name: String,
}

/// List installed printers (local plus connections). `Some` means the
/// spooler answered (empty means none installed). `None` means failed or
/// non-Windows: the caller proceeds silently, never toasting unknowns.
#[cfg(target_os = "windows")]
pub fn enumerate_printers() -> Option<Vec<PrinterInfo>> {
    use windows::Win32::Graphics::Printing::{
        PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_INFO_1W,
    };

    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut needed: u32 = 0;
    let mut returned: u32 = 0;
    // Size probe: failure here only tells us the buffer size.
    let _ = unsafe {
        windows::Win32::Graphics::Printing::EnumPrintersW(
            flags,
            windows::core::PCWSTR::null(),
            1,
            None,
            &mut needed,
            &mut returned,
        )
    };
    if needed == 0 || needed > MAX_ENUM_BYTES {
        return None;
    }
    let mut buf = vec![0u8; needed as usize];
    unsafe {
        windows::Win32::Graphics::Printing::EnumPrintersW(
            flags,
            windows::core::PCWSTR::null(),
            1,
            Some(buf.as_mut_slice()),
            &mut needed,
            &mut returned,
        )
    }
    .ok()?;
    let count = returned as usize;
    if count == 0 {
        return Some(Vec::new());
    }
    if (count * std::mem::size_of::<PRINTER_INFO_1W>()) > buf.len() {
        return None;
    }
    let infos =
        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_1W, count) };
    let mut out = Vec::with_capacity(count);
    for info in infos {
        let name: String = unsafe { info.pName.to_string() }.unwrap_or_default();
        let name: String = name.chars().take(MAX_PRINTER_NAME_CHARS).collect();
        if !name.is_empty() {
            out.push(PrinterInfo { name });
        }
    }
    Some(out)
}

/// Non-Windows spooler listing is unverified: report unknown, never empty.
#[cfg(not(target_os = "windows"))]
pub fn enumerate_printers() -> Option<Vec<PrinterInfo>> {
    None
}

/// Preflight copy for a known printer list. `None` means proceed silently
/// (printers present, or state unknown); `Some` carries the no-printer
/// orientation, which names the still-available PDF path.
pub fn no_printer_notice(printers: &Option<Vec<PrinterInfo>>) -> Option<String> {
    match printers {
        Some(list) if list.is_empty() => {
            Some("No printers installed. The print dialog can still save as PDF.".to_owned())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_blocks_double_fire_and_releases() {
        let mut g = PrintGuard::new();
        let tab = uuid::Uuid::new_v4();
        let t0 = Instant::now();
        assert!(g.begin(tab, t0), "first claim wins");
        assert!(!g.begin(tab, t0), "second trigger refused");
        assert!(g.is_printing(&tab, t0));
        g.finish(&tab);
        assert!(g.begin(tab, t0), "released tab prints again");
    }

    #[test]
    fn guard_scopes_tabs_independently() {
        let mut g = PrintGuard::new();
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let t0 = Instant::now();
        assert!(g.begin(a, t0));
        assert!(g.begin(b, t0), "other tab unaffected");
        g.finish(&a);
        assert!(!g.is_printing(&a, t0));
        assert!(g.is_printing(&b, t0));
    }

    #[test]
    fn guard_evicts_stale_jobs() {
        let mut g = PrintGuard::new();
        let tab = uuid::Uuid::new_v4();
        let t0 = Instant::now();
        assert!(g.begin(tab, t0));
        let later = t0 + PRINT_JOB_STALE + Duration::from_secs(1);
        assert!(!g.is_printing(&tab, later), "stale entry evicted");
        assert!(g.begin(tab, later), "wedged tab recovers");
    }

    #[test]
    fn held_for_distinguishes_fresh_from_abandoned() {
        let mut g = PrintGuard::new();
        let tab = uuid::Uuid::new_v4();
        let t0 = Instant::now();
        assert!(g.begin(tab, t0));
        assert_eq!(
            g.held_for(&tab, t0 + Duration::from_secs(5))
                .map(|d| d.as_secs()),
            Some(5),
            "recent dialog: wait, don't retry"
        );
        assert!(
            g.held_for(&tab, t0 + Duration::from_secs(31))
                .is_some_and(|d| d.as_secs() >= 30),
            "old hold: dialog was dismissed, retry is safe"
        );
        assert_eq!(
            g.held_for(&tab, t0 + PRINT_JOB_STALE + Duration::from_secs(1)),
            None,
            "stale entries evict instead of reporting age"
        );
    }

    #[test]
    fn preflight_only_speaks_for_known_empty() {
        assert_eq!(
            no_printer_notice(&Some(Vec::new())),
            Some("No printers installed. The print dialog can still save as PDF.".to_owned())
        );
        assert_eq!(
            no_printer_notice(&Some(vec![PrinterInfo {
                name: "Office".to_owned()
            }])),
            None
        );
        assert_eq!(no_printer_notice(&None), None, "unknown stays silent");
    }

    #[test]
    fn enumeration_never_fails_the_caller() {
        // Unknown is a valid answer (non-Windows, spooler down): the only
        // contract is that the call itself cannot panic.
        let _ = enumerate_printers();
    }
}
