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
        let mut combined = String::new();
        for (_name, text) in lists {
            combined.push_str(text);
            combined.push('\n');
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

        // Build a fresh `FilterSet` → `Engine`. `FilterSet::new(false)` disables
        // debug source tracking (smaller memory, faster). Lists are added via
        // `add_filter_list` which honors `!`/`#` comments and network vs cosmetic
        // syntax automatically.
        let mut set = FilterSet::new(false);
        for (_name, text) in lists {
            set.add_filter_list(text.to_string(), ParseOptions::default());
        }
        let new_engine = Engine::new_with_filter_set(set);

        self.engine = new_engine;
        self.last_good = Some(combined);
        Ok(())
    }

    pub fn check(&self, meta: &RequestMeta) -> Option<Verdict> {
        let request_type = resource_type_to_request_type(meta.resource);
        // Source URL is the referrer when present, otherwise the request itself.
        // `adblock` uses source for `$third-party` and document scope.
        let source_url = meta
            .referrer
            .as_deref()
            .unwrap_or("https://ferrite.invalid/");
        // Construct a synthetic URL for the request. We only have host + resource,
        // so synthesize `https://host/` + path hint. Path classification already
        // produced `meta.resource`; the URL shape is enough for `Engine`.
        let url = format!("https://{}/", meta.request_domain);
        // `Request::new` parses and classifies; fallback to allow on parse failure.
        let req = match adblock::request::Request::new(&url, source_url, request_type, "get") {
            Ok(r) => r,
            Err(_) => return None,
        };
        let res = self.engine.check_network_request(&req);
        let should_block = res.important || (res.filter.is_some() && res.exception.is_none());
        if should_block {
            // Heuristic verdict: script/image/stylesheet → BlockAd else BlockTracker
            return Some(match meta.resource {
                ResourceType::Script | ResourceType::Image | ResourceType::Stylesheet => {
                    Verdict::BlockAd
                }
                _ => Verdict::BlockTracker,
            });
        }
        None
    }

    pub fn selectors_for_url(&self, url: &str) -> Vec<String> {
        let resources = self.engine.url_cosmetic_resources(url);
        let mut out = Vec::new();
        // `hide_selectors` is a set of CSS selectors for this hostname.
        out.extend(resources.hide_selectors);
        out
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
    fn integrity_mismatch() {
        let mut e = FilterEngine::new().unwrap();
        let text = "||ads.test^";
        let good: String = Sha256::digest(format!("{text}\n").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(e.load(&[("t", text)], Some(&good)).is_ok());
        assert!(
            e.load(
                &[("t", text)],
                Some("0000000000000000000000000000000000000000000000000000000000000000")
            )
            .is_err()
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
