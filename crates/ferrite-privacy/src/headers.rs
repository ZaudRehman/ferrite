//! GPC header and tracker cookie stripping predicates.

pub fn should_add_gpc(enabled: bool) -> bool {
    enabled
}

pub fn gpc_header() -> (&'static str, &'static str) {
    ("Sec-GPC", "1")
}

pub fn should_strip_cookies(is_tracker: bool) -> bool {
    is_tracker
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpc_enabled() {
        assert!(should_add_gpc(true));
        assert!(!should_add_gpc(false));
        assert_eq!(gpc_header(), ("Sec-GPC", "1"));
    }

    #[test]
    fn strip_tracker_only() {
        assert!(should_strip_cookies(true));
        assert!(!should_strip_cookies(false));
    }
}
