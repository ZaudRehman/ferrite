//! Filter lists and matching.

use adblock::{Engine, FilterSet, lists::ParseOptions};
use sha2::{Digest, Sha256};

use crate::error::{PrivacyError, PrivacyResult};
use ferrite_proxy::{RequestMeta, ResourceType, Verdict};

pub struct FilterEngine {
    engine: Engine,
    last_good: Option<String>,
}

impl std::fmt::Debug for FilterEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterEngine")
            .field("last_good_len", &self.last_good.as_ref().map(|s| s.len()))
            .finish_non_exhaustive()
    }
}

impl FilterEngine {
    pub fn new() -> PrivacyResult<Self> {
        Ok(Self {
            engine: Engine::default(),
            last_good: None,
        })
    }

    pub fn load(
        &mut self,
        lists: &[(&str, &str)],
        expected_sha256: Option<&str>,
    ) -> PrivacyResult<()> {
        // Normalized integrity contract: each list is `trim_end + "\n"`.
        let mut combined = String::new();
        let mut normalized: Vec<String> = Vec::with_capacity(lists.len());
        for (_name, text) in lists {
            let norm = format!("{}\n", text.trim_end_matches(['\n', '\r']));
            combined.push_str(&norm);
            normalized.push(norm);
        }

        if let Some(expected) = expected_sha256 {
            let actual: String = Sha256::digest(combined.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if actual.to_lowercase() != expected.to_lowercase() {
                return Err(PrivacyError::IntegrityMismatch {
                    list: "filter_lists".to_string(),
                    expected: expected.to_owned(),
                    actual,
                });
            }
        }

        // Build the new engine BEFORE touching self: callers swap under a
        // brief write lock (RCU-style) so hourly updates do not freeze reads.
        let mut set = FilterSet::new(false);
        for norm in &normalized {
            set.add_filter_list(norm.clone(), ParseOptions::default());
        }
        let new_engine = Engine::new_with_filter_set(set);

        self.engine = new_engine;
        self.last_good = Some(combined);
        Ok(())
    }

    pub fn check(&self, meta: &RequestMeta) -> Option<Verdict> {
        let request_type = resource_type_to_request_type(meta.resource);
        // Real URL (with path/query) is required for path, query and
        // `$third-party` matching. Source is the referrer, else the URL
        // itself (first-party) — never a synthetic invalid host.
        let url = if meta.request_url.is_empty() {
            format!("https://{}/", meta.request_domain)
        } else {
            meta.request_url.clone()
        };
        let source_url = meta.referrer.as_deref().unwrap_or(&url);
        // `Request::new` parses and classifies; fallback to allow on parse failure.
        let req = match adblock::request::Request::new(&url, source_url, request_type, "get") {
            Ok(r) => r,
            Err(_) => return None,
        };
        let res = self.engine.check_network_request(&req);
        if !res.should_block() {
            return None;
        }
        if res.redirect.is_some() && res.filter.is_none() {
            return None;
        }
        if res.filter.is_none() && res.redirect.is_none() {
            return None;
        }
        Some(match meta.resource {
            ResourceType::Script
            | ResourceType::Image
            | ResourceType::Stylesheet
            | ResourceType::Media
            | ResourceType::Font => Verdict::BlockAd,
            _ => Verdict::BlockTracker,
        })
    }

    pub fn selectors_for_url(&self, url: &str) -> Vec<String> {
        let resources = self.engine.url_cosmetic_resources(url);
        let mut out = Vec::new();
        out.extend(resources.hide_selectors);
        out
    }

    pub fn get_csp(&self, req: &adblock::request::Request) -> Option<String> {
        self.engine.get_csp_directives(req)
    }

    pub fn verify_sha256(data: &[u8], expected_hex: &str) -> PrivacyResult<()> {
        let actual: String = Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if actual.to_lowercase() != expected_hex.to_lowercase() {
            return Err(PrivacyError::IntegrityMismatch {
                list: "download".to_string(),
                expected: expected_hex.to_owned(),
                actual,
            });
        }
        Ok(())
    }
}

fn resource_type_to_request_type(rt: ResourceType) -> &'static str {
    match rt {
        ResourceType::Document => "document",
        ResourceType::Script => "script",
        ResourceType::Image => "image",
        ResourceType::Stylesheet => "stylesheet",
        ResourceType::Xhr => "xmlhttprequest",
        ResourceType::Media => "media",
        ResourceType::Font => "font",
        ResourceType::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_check() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[("easylist", "||ads.example^\n||tracker.test^")], None)
            .unwrap();
        assert_eq!(
            e.check(&RequestMeta::new("ads.example", ResourceType::Script)),
            Some(Verdict::BlockAd)
        );
        assert_eq!(
            e.check(&RequestMeta::new("sub.tracker.test", ResourceType::Other)),
            Some(Verdict::BlockTracker)
        );
        assert_eq!(
            e.check(&RequestMeta::new("example.com", ResourceType::Document)),
            None
        );
    }

    #[test]
    fn path_rules_require_real_url() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[("t", "||example.com/ads/*")], None).unwrap();
        // Domain-only meta cannot match a path rule.
        assert_eq!(
            e.check(&RequestMeta::new("example.com", ResourceType::Image)),
            None
        );
        // Real URL with the path matches.
        let meta = RequestMeta::new("example.com", ResourceType::Image)
            .with_url("https://example.com/ads/banner.png");
        assert_eq!(e.check(&meta), Some(Verdict::BlockAd));
        // Unrelated path on the same host does not match.
        let clean = RequestMeta::new("example.com", ResourceType::Image)
            .with_url("https://example.com/content/photo.png");
        assert_eq!(e.check(&clean), None);
    }

    #[test]
    fn media_and_font_map_to_ad() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[("t", "||cdn.example^\n||fonts.example^")], None)
            .unwrap();
        let media = RequestMeta::new("cdn.example", ResourceType::Media)
            .with_url("https://cdn.example/clip.mp4");
        assert_eq!(e.check(&media), Some(Verdict::BlockAd));
        let font = RequestMeta::new("fonts.example", ResourceType::Font)
            .with_url("https://fonts.example/a.woff2");
        assert_eq!(e.check(&font), Some(Verdict::BlockAd));
    }

    #[test]
    fn integrity_mismatch() {
        let mut e = FilterEngine::new().unwrap();
        assert!(
            e.load(
                &[("t", "||ads.test^")],
                Some("0000000000000000000000000000000000000000000000000000000000000000")
            )
            .is_err()
        );
    }

    #[test]
    fn integrity_normalizes_trailing_newlines() {
        let mut e = FilterEngine::new().unwrap();
        let text = "||ads.test^";
        let good: String = Sha256::digest(format!("{text}\n").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        // Both forms normalize to the same hash.
        assert!(e.load(&[("t", text)], Some(&good)).is_ok());
        assert!(
            e.load(&[("t", &format!("{text}\n\n"))], Some(&good))
                .is_ok()
        );
    }

    #[test]
    fn verify_sha256_helper() {
        let data = b"hello";
        let hash: String = Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(FilterEngine::verify_sha256(data, &hash).is_ok());
        assert!(FilterEngine::verify_sha256(data, "bad").is_err());
    }
}
