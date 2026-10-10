//! Engine queries. Capabilities live in the engine, wry surfaces a subset,
//! Ferrite builds on wry. Launch checks query the loader directly.

/// Evergreen version, or None when no usable runtime is installed.
/// Windows only. Other platforms always return None. Null folder requests
/// the Evergreen lookup, so this matches what wry will do.
#[cfg(target_os = "windows")]
pub fn webview2_runtime_version() -> Option<String> {
    use webview2_com::take_pwstr;
    use windows::core::{PCWSTR, PWSTR};
    // SAFETY: null folder is the Evergreen lookup. Out string is consumed
    // once by take_pwstr. Missing loader or runtime surfaces as Err.
    unsafe {
        let mut version = PWSTR::null();
        webview2_com_sys::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString(
            PCWSTR::null(),
            &mut version,
        )
        .ok()?;
        let s = take_pwstr(version);
        (!s.trim().is_empty()).then_some(s)
    }
}

/// Other platforms have no such concept. The launch gate must not consult this.
#[cfg(not(target_os = "windows"))]
pub fn webview2_runtime_version() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dotted-numeric shape guard (`130.0.2849.80`): the About page and
    /// any future staleness check parse on dots, so assert the contract
    /// here rather than debugging a render or compare failure elsewhere.
    #[cfg(target_os = "windows")]
    #[test]
    fn runtime_version_is_dotted_when_present() {
        let Some(v) = webview2_runtime_version() else {
            // CI/dev machines carry the runtime; a bare machine reports
            // missing, which is the honest answer the gate acts on.
            return;
        };
        let parts: Vec<&str> = v.split('.').collect();
        assert!(parts.len() >= 3, "unexpected runtime version {v:?}");
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
            "non-numeric runtime version {v:?}"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn runtime_query_is_not_applicable_off_windows() {
        assert_eq!(webview2_runtime_version(), None);
    }
}
