//! Decision hook between proxy and privacy engine.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    BlockAd,
    BlockTracker,
    BlockMalware,
    BlockMixedContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMeta {
    pub request_domain: String,
    pub resource: ResourceType,
    pub is_secure: bool,
    pub referrer: Option<String>,
    /// Full absolute URL (`https://host/path?q`) when known. Empty for
    /// CONNECT tunnels where only the authority is visible. Adblock path,
    /// query and `$third-party` matching require this; domain-only synthesis
    /// silently disables most EasyList rules.
    pub request_url: String,
}

impl RequestMeta {
    pub fn new(request_domain: impl Into<String>, resource: ResourceType) -> Self {
        Self {
            request_domain: request_domain.into(),
            resource,
            is_secure: false,
            referrer: None,
            request_url: String::new(),
        }
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.request_url = url.into();
        self
    }

    /// Whether this is a mixed-content subresource (HTTP from HTTPS page).
    pub fn is_mixed_content(&self) -> bool {
        !self.is_secure
            && self
                .referrer
                .as_deref()
                .is_some_and(|r| r.starts_with("https://"))
    }
}

pub trait DecisionHook: Send + Sync + std::fmt::Debug {
    fn decide(&self, meta: &RequestMeta) -> Verdict;

    fn should_strip_cookies(&self, _host: &str) -> bool {
        false
    }

    fn should_send_gpc(&self) -> bool {
        true
    }

    fn should_upgrade_https(&self) -> bool {
        true
    }

    fn csp_directives(&self, _meta: &RequestMeta) -> Option<String> {
        None
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAllHook;

impl DecisionHook for AllowAllHook {
    fn decide(&self, _meta: &RequestMeta) -> Verdict {
        Verdict::Allow
    }
}

#[derive(Debug, Clone)]
pub struct BlockListHook {
    pub blocked: Vec<String>,
}

impl BlockListHook {
    pub fn new(blocked: &[&str]) -> Self {
        Self {
            blocked: blocked.iter().map(|s| s.to_lowercase()).collect(),
        }
    }
}

impl DecisionHook for BlockListHook {
    fn decide(&self, meta: &RequestMeta) -> Verdict {
        let host = meta.request_domain.to_lowercase();
        if self
            .blocked
            .iter()
            .any(|b| host == *b || host.ends_with(&format!(".{b}")))
        {
            Verdict::BlockTracker
        } else {
            Verdict::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ResourceType::*;

    #[test]
    fn allow_all_allows() {
        let h = AllowAllHook;
        assert_eq!(
            h.decide(&RequestMeta::new("x.test", Document)),
            Verdict::Allow
        );
    }

    #[test]
    fn block_list_blocks_exact_and_suffix() {
        let h = BlockListHook::new(&["doubleclick.net"]);
        assert_eq!(
            h.decide(&RequestMeta::new("doubleclick.net", Script)),
            Verdict::BlockTracker
        );
        assert_eq!(
            h.decide(&RequestMeta::new("ad.doubleclick.net", Image)),
            Verdict::BlockTracker
        );
        assert_eq!(
            h.decide(&RequestMeta::new("notdoubleclick.net", Xhr)),
            Verdict::Allow
        );
        assert_eq!(
            h.decide(&RequestMeta::new("DoubleClick.NET", Script)),
            Verdict::BlockTracker
        );
    }
    #[test]
    fn path_classification() {
        assert_eq!(resource_type_from_path("/a/b.js"), ResourceType::Script);
        assert_eq!(resource_type_from_path("/i.PNG?q=1"), ResourceType::Image);
        assert_eq!(resource_type_from_path("/s.css"), ResourceType::Stylesheet);
        assert_eq!(resource_type_from_path("/"), ResourceType::Document);
        assert_eq!(resource_type_from_path("/x.html"), ResourceType::Document);
        assert_eq!(resource_type_from_path("/api/data"), ResourceType::Other);
        assert_eq!(resource_type_from_path("/clip.mp4"), ResourceType::Media);
        assert_eq!(resource_type_from_path("/a.woff2"), ResourceType::Font);
        assert_eq!(resource_type_from_path("/photo.avif"), ResourceType::Image);
    }

    #[test]
    fn sec_fetch_dest_empty_falls_back_to_path() {
        // `empty` must NOT force Document (it poisoned the top-level gate).
        assert_eq!(resource_type_from_sec_fetch_dest("empty"), None);
        assert_eq!(
            resource_type_for_request(Some("empty"), "/a.js"),
            ResourceType::Script
        );
        assert_eq!(
            resource_type_from_sec_fetch_dest("audio"),
            Some(ResourceType::Media)
        );
        assert_eq!(
            resource_type_from_sec_fetch_dest("font"),
            Some(ResourceType::Font)
        );
        assert_eq!(
            resource_type_from_sec_fetch_dest("iframe"),
            Some(ResourceType::Document)
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Document,
    Script,
    Image,
    Stylesheet,
    Xhr,
    Media,
    Font,
    Other,
}

pub fn resource_type_from_sec_fetch_dest(value: &str) -> Option<ResourceType> {
    match value.trim().to_ascii_lowercase().as_str() {
        // Top-level navigation only. `empty` is fetch/XHR fallback and must
        // NOT become Document (it poisoned the top-level realtime gate).
        "document" => Some(ResourceType::Document),
        "empty" => None,
        "script" | "audioworklet" | "serviceworker" | "sharedworker" | "worker" => {
            Some(ResourceType::Script)
        }
        "image" => Some(ResourceType::Image),
        "style" => Some(ResourceType::Stylesheet),
        "xhr" | "fetch" => Some(ResourceType::Xhr),
        "audio" | "video" | "track" => Some(ResourceType::Media),
        "font" => Some(ResourceType::Font),
        // iframe/frame/object/embed/fencedframe etc. are sub-documents;
        // map to Document so top-level policy applies, not Other.
        "iframe" | "frame" | "fencedframe" | "object" | "embed" => Some(ResourceType::Document),
        "manifest" | "report" => Some(ResourceType::Other),
        _ => None,
    }
}

pub fn resource_type_from_path(path: &str) -> ResourceType {
    let lower = path.split('?').next().unwrap_or(path).to_lowercase();
    if lower.ends_with(".js") || lower.ends_with(".mjs") {
        ResourceType::Script
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".svg")
        || lower.ends_with(".avif")
        || lower.ends_with(".ico")
        || lower.ends_with(".bmp")
    {
        ResourceType::Image
    } else if lower.ends_with(".css") {
        ResourceType::Stylesheet
    } else if lower.ends_with(".mp4")
        || lower.ends_with(".webm")
        || lower.ends_with(".mp3")
        || lower.ends_with(".ogg")
        || lower.ends_with(".wav")
        || lower.ends_with(".m4a")
        || lower.ends_with(".avi")
        || lower.ends_with(".mov")
    {
        ResourceType::Media
    } else if lower.ends_with(".woff")
        || lower.ends_with(".woff2")
        || lower.ends_with(".ttf")
        || lower.ends_with(".otf")
        || lower.ends_with(".eot")
    {
        ResourceType::Font
    } else if lower.ends_with(".html") || lower.ends_with(".htm") || lower == "/" {
        ResourceType::Document
    } else {
        ResourceType::Other
    }
}

pub fn resource_type_for_request(sec_fetch_dest: Option<&str>, path: &str) -> ResourceType {
    if let Some(v) = sec_fetch_dest.and_then(resource_type_from_sec_fetch_dest) {
        return v;
    }
    resource_type_from_path(path)
}
