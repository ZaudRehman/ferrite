//! Internal routes (`ferrite://`).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FerriteRoute {
    NewTab,
    Settings,
    Archive,
    Passwords,
    Crashed,
    Blocked,
    Unknown(String),
}

impl FerriteRoute {
    pub fn parse(uri: &str) -> Option<Self> {
        let trimmed = uri.trim();
        let lower = trimmed.to_lowercase();
        if !lower.starts_with("ferrite://") {
            return None;
        }
        let path = lower
            .trim_start_matches("ferrite://")
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        let route = match path.as_str() {
            "" | "newtab" => Self::NewTab,
            "settings" => Self::Settings,
            "archive" => Self::Archive,
            "passwords" => Self::Passwords,
            "crashed" => Self::Crashed,
            "blocked" => Self::Blocked,
            other => Self::Unknown(other.to_owned()),
        };
        Some(route)
    }

    pub fn is_internal(uri: &str) -> bool {
        Self::parse(uri).is_some()
    }

    pub fn placeholder_html(&self) -> String {
        let title = match self {
            Self::NewTab => "New Tab - Ferrite",
            Self::Settings => "Settings - Ferrite",
            Self::Archive => "Archive - Ferrite",
            Self::Passwords => "Passwords - Ferrite",
            Self::Crashed => "Tab Crashed",
            Self::Blocked => "Blocked - Ferrite",
            Self::Unknown(s) => return format!("<h1>404 - ferrite://{}</h1>", s),
        };
        format!(
            r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>{}</title>
<style>html,body{{margin:0;height:100%;background:#0A0A0F;color:#E0E0E0;font-family:system-ui;display:flex;align-items:center;justify-content:center}}</style>
</head><body><h2>{}</h2></body></html>"#,
            title, title
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_routes() {
        assert_eq!(
            FerriteRoute::parse("ferrite://newtab"),
            Some(FerriteRoute::NewTab)
        );
        assert_eq!(
            FerriteRoute::parse("ferrite://settings"),
            Some(FerriteRoute::Settings)
        );
        assert_eq!(
            FerriteRoute::parse("ferrite://archive"),
            Some(FerriteRoute::Archive)
        );
        assert_eq!(
            FerriteRoute::parse("ferrite://passwords"),
            Some(FerriteRoute::Passwords)
        );
        assert_eq!(
            FerriteRoute::parse("ferrite://crashed"),
            Some(FerriteRoute::Crashed)
        );
        assert_eq!(
            FerriteRoute::parse("ferrite://blocked"),
            Some(FerriteRoute::Blocked)
        );
    }

    #[test]
    fn parses_with_query_and_case_insensitive() {
        assert_eq!(
            FerriteRoute::parse("ferrite://Settings?tab=privacy"),
            Some(FerriteRoute::Settings)
        );
        assert_eq!(
            FerriteRoute::parse("FERRITE://NEWTAB"),
            Some(FerriteRoute::NewTab)
        );
    }

    #[test]
    fn unknown_route() {
        assert_eq!(
            FerriteRoute::parse("ferrite://foobar"),
            Some(FerriteRoute::Unknown("foobar".to_owned()))
        );
    }

    #[test]
    fn non_ferrite_is_none() {
        assert_eq!(FerriteRoute::parse("https://example.com"), None);
        assert_eq!(FerriteRoute::parse("example.com"), None);
    }

    #[test]
    fn placeholder_contains_title() {
        assert!(FerriteRoute::NewTab.placeholder_html().contains("New Tab"));
        assert!(
            FerriteRoute::Unknown("x".to_owned())
                .placeholder_html()
                .contains("404")
        );
    }

    #[test]
    fn is_internal() {
        assert!(FerriteRoute::is_internal("ferrite://newtab"));
        assert!(!FerriteRoute::is_internal("https://a.com"));
    }
}
