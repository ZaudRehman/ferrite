//! Filter lists and matching. Category comes from the matching list,
//! never the resource type. Scriptlets are parsed then dropped: only
//! hide selectors leave this module.

use adblock::{Engine, FilterSet, lists::ParseOptions};
use sha2::{Digest, Sha256};

use crate::error::{PrivacyError, PrivacyResult};
use ferrite_proxy::{RequestMeta, ResourceType, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListCategory {
    Ads,
    Tracking,
    /// Enforced as tracker, counted apart for the shield.
    Fingerprinting,
}

/// One named list. A hash mismatch rejects the whole swap.
pub struct FilterList<'a> {
    pub name: &'a str,
    pub category: ListCategory,
    pub text: &'a str,
    pub expected_sha256: Option<&'a str>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockTag {
    pub verdict: Verdict,
    pub category: ListCategory,
    pub list: usize,
}

/// Cap on merged selectors per page. Bounds injected CSS.
// ponytail: raise only with a measured page that needs more.
pub const SELECTOR_CAP: usize = 512;

struct ListEngine {
    name: String,
    category: ListCategory,
    engine: Engine,
    /// Raw list bytes (text Ferrite fed the parser; a floor for the
    /// compiled index, which the adblock crate never reports).
    bytes: usize,
    /// Rule lines (non-empty, not `!`/`[` comments).
    rules: usize,
}

pub struct FilterEngine {
    lists: Vec<ListEngine>,
    last_good: Option<String>,
}

impl std::fmt::Debug for FilterEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterEngine")
            .field(
                "lists",
                &self.lists.iter().map(|l| &l.name).collect::<Vec<_>>(),
            )
            .field("last_good_len", &self.last_good.as_ref().map(|s| s.len()))
            .finish_non_exhaustive()
    }
}

impl FilterEngine {
    pub fn new() -> PrivacyResult<Self> {
        Ok(Self {
            lists: Vec::new(),
            last_good: None,
        })
    }

    /// All or nothing. Any failure keeps the previous lists serving.
    pub fn load(&mut self, lists: &[FilterList<'_>]) -> PrivacyResult<()> {
        // Integrity is checked on `trim_end + "\n"` per list.
        let mut normalized: Vec<String> = Vec::with_capacity(lists.len());
        for list in lists {
            let norm = format!("{}\n", list.text.trim_end_matches(['\n', '\r']));
            if let Some(expected) = list.expected_sha256 {
                let actual: String = Sha256::digest(norm.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                if actual.to_lowercase() != expected.to_lowercase() {
                    return Err(PrivacyError::IntegrityMismatch {
                        list: list.name.to_string(),
                        expected: expected.to_owned(),
                        actual,
                    });
                }
            }
            normalized.push(norm);
        }

        // Build before swapping, so updates never freeze reads.
        let mut built = Vec::with_capacity(lists.len());
        for (list, norm) in lists.iter().zip(normalized.iter()) {
            let mut set = FilterSet::new(false);
            set.add_filter_list(norm.clone(), ParseOptions::default());
            built.push(ListEngine {
                name: list.name.to_string(),
                category: list.category,
                engine: Engine::new_with_filter_set(set),
                bytes: norm.len(),
                rules: norm
                    .lines()
                    .filter(|l| {
                        let t = l.trim();
                        !(t.is_empty() || t.starts_with('!') || t.starts_with('['))
                    })
                    .count(),
            });
        }

        let combined = normalized.concat();
        self.lists = built;
        self.last_good = Some(combined);
        Ok(())
    }

    /// Empty when out of range, never panics.
    pub fn list_name(&self, index: usize) -> &str {
        self.lists.get(index).map(|l| l.name.as_str()).unwrap_or("")
    }

    /// Per-list (name, bytes, rules) for the task manager. Bytes are
    /// the source text, not heap: the adblock crate exposes no memory
    /// API, so this is a documented floor, never a heap claim.
    pub fn list_stats(&self) -> Vec<(String, usize, usize)> {
        self.lists
            .iter()
            .map(|l| (l.name.clone(), l.bytes, l.rules))
            .collect()
    }

    fn check_one(engine: &Engine, meta: &RequestMeta) -> bool {
        // Real URL is required for path and third party matching.
        let url = if meta.request_url.is_empty() {
            format!("https://{}/", meta.request_domain)
        } else {
            meta.request_url.clone()
        };
        let source_url = meta.referrer.as_deref().unwrap_or(&url);
        let req = match adblock::request::Request::new(
            &url,
            source_url,
            resource_type_to_request_type(meta.resource),
            "get",
        ) {
            Ok(r) => r,
            Err(_) => return false,
        };
        let res = engine.check_network_request(&req);
        if !res.should_block() {
            return false;
        }
        if res.redirect.is_some() && res.filter.is_none() {
            return false;
        }
        if res.filter.is_none() && res.redirect.is_none() {
            return false;
        }
        true
    }

    /// First match in load order wins.
    pub fn check_tagged(&self, meta: &RequestMeta) -> Option<BlockTag> {
        for (i, list) in self.lists.iter().enumerate() {
            if Self::check_one(&list.engine, meta) {
                let verdict = match list.category {
                    ListCategory::Ads => Verdict::BlockAd,
                    ListCategory::Tracking | ListCategory::Fingerprinting => Verdict::BlockTracker,
                };
                return Some(BlockTag {
                    verdict,
                    category: list.category,
                    list: i,
                });
            }
        }
        None
    }

    pub fn check(&self, meta: &RequestMeta) -> Option<Verdict> {
        self.check_tagged(meta).map(|t| t.verdict)
    }

    /// Union across lists, deduped and capped. Scriptlets never surface.
    pub fn selectors_for_url(&self, url: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for list in &self.lists {
            for s in &list.engine.url_cosmetic_resources(url).hide_selectors {
                if out.len() >= SELECTOR_CAP {
                    break;
                }
                if !out.iter().any(|e| e == s) {
                    out.push(s.clone());
                }
            }
            if out.len() >= SELECTOR_CAP {
                break;
            }
        }
        out
    }

    /// Union of CSP directives across ALL lists (not just the first):
    /// same-name directives merge their source lists, distinct directives
    /// concatenate, order follows load order. A second list's directives
    /// previously vanished silently; dropping enforcement is the one
    /// direction a merge must never take.
    pub fn get_csp(&self, req: &adblock::request::Request) -> Option<String> {
        let mut order: Vec<String> = Vec::new();
        let mut sources: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for list in &self.lists {
            let Some(directives) = list.engine.get_csp_directives(req) else {
                continue;
            };
            for clause in directives.split(';') {
                let clause = clause.trim();
                if clause.is_empty() {
                    continue;
                }
                let mut words = clause.split_whitespace();
                let Some(name) = words.next() else {
                    continue;
                };
                let entry = sources.entry(name.to_owned()).or_insert_with(|| {
                    order.push(name.to_owned());
                    Vec::new()
                });
                for src in words {
                    if !entry.iter().any(|e| e == src) {
                        entry.push(src.to_owned());
                    }
                }
            }
        }
        if order.is_empty() {
            return None;
        }
        let merged = order
            .iter()
            .map(|name| {
                let srcs = sources.get(name).cloned().unwrap_or_default();
                if srcs.is_empty() {
                    name.clone()
                } else {
                    format!("{name} {}", srcs.join(" "))
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some(merged)
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

    fn ads<'a>(name: &'a str, text: &'a str) -> FilterList<'a> {
        FilterList {
            name,
            category: ListCategory::Ads,
            text,
            expected_sha256: None,
        }
    }

    fn tracking<'a>(name: &'a str, text: &'a str) -> FilterList<'a> {
        FilterList {
            name,
            category: ListCategory::Tracking,
            text,
            expected_sha256: None,
        }
    }

    #[test]
    fn load_and_check() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[
            ads("easylist", "||ads.example^\n"),
            tracking("easyprivacy", "||tracker.test^"),
        ])
        .unwrap();
        // Ads list tags ads even for subresources.
        assert_eq!(
            e.check(&RequestMeta::new("ads.example", ResourceType::Script)),
            Some(Verdict::BlockAd)
        );
        // Tracking list tags trackers even for images.
        assert_eq!(
            e.check(&RequestMeta::new("tracker.test", ResourceType::Image)),
            Some(Verdict::BlockTracker)
        );
        assert_eq!(
            e.check(&RequestMeta::new("example.com", ResourceType::Document)),
            None
        );
    }

    #[test]
    fn list_stats_counts_rules_not_comments() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[ads(
            "easylist",
            "! title comment\n[Adblock]\n\n||ads.example^\n@@||ok.example^\n",
        )])
        .unwrap();
        let stats = e.list_stats();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].0, "easylist");
        assert_eq!(
            stats[0].2, 2,
            "two rule lines, comments/blank/header excluded"
        );
        assert!(stats[0].1 > 0, "source bytes recorded");
    }

    #[test]
    fn tag_names_the_matching_list() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[
            ads("easylist", "||ads.example^\n"),
            tracking("easyprivacy", "||tracker.test^\n||ads.example^\n"),
        ])
        .unwrap();
        // First match in load order wins.
        let tag = e
            .check_tagged(&RequestMeta::new("ads.example", ResourceType::Script))
            .unwrap();
        assert_eq!(tag.verdict, Verdict::BlockAd);
        assert_eq!(tag.category, ListCategory::Ads);
        assert_eq!(e.list_name(tag.list), "easylist");
        let tag = e
            .check_tagged(&RequestMeta::new("tracker.test", ResourceType::Other))
            .unwrap();
        assert_eq!(tag.verdict, Verdict::BlockTracker);
        assert_eq!(tag.category, ListCategory::Tracking);
        assert_eq!(e.list_name(tag.list), "easyprivacy");
        assert_eq!(e.list_name(99), "");
    }

    #[test]
    fn fingerprinting_enforces_as_tracker() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[FilterList {
            name: "fingerprinting",
            category: ListCategory::Fingerprinting,
            text: "||fp.test^\n",
            expected_sha256: None,
        }])
        .unwrap();
        let tag = e
            .check_tagged(&RequestMeta::new("fp.test", ResourceType::Script))
            .unwrap();
        assert_eq!(tag.verdict, Verdict::BlockTracker);
        assert_eq!(tag.category, ListCategory::Fingerprinting);
    }

    #[test]
    fn path_rules_require_real_url() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[ads("t", "||example.com/ads/*")]).unwrap();
        assert_eq!(
            e.check(&RequestMeta::new("example.com", ResourceType::Image)),
            None
        );
        // Real URL with the path matches.
        let meta = RequestMeta::new("example.com", ResourceType::Image)
            .with_url("https://example.com/ads/banner.png");
        assert_eq!(e.check(&meta), Some(Verdict::BlockAd));
        let clean = RequestMeta::new("example.com", ResourceType::Image)
            .with_url("https://example.com/content/photo.png");
        assert_eq!(e.check(&clean), None);
    }

    #[test]
    fn csp_unions_across_lists() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[
            ads("a", "||example.com^$csp=script-src 'none'"),
            tracking("b", "||example.com^$csp=style-src 'none'"),
        ])
        .unwrap();
        let req = adblock::request::Request::new(
            "https://example.com/",
            "https://example.com/",
            "document",
            "get",
        )
        .unwrap();
        let csp = e.get_csp(&req).expect("both lists contribute");
        assert!(csp.contains("script-src"), "first list kept: {csp}");
        assert!(csp.contains("style-src"), "second list kept: {csp}");
    }

    #[test]
    fn media_and_font_map_by_list() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[ads("t", "||cdn.example^\n||fonts.example^")])
            .unwrap();
        let media = RequestMeta::new("cdn.example", ResourceType::Media)
            .with_url("https://cdn.example/clip.mp4");
        assert_eq!(e.check(&media), Some(Verdict::BlockAd));
        let font = RequestMeta::new("fonts.example", ResourceType::Font)
            .with_url("https://fonts.example/a.woff2");
        assert_eq!(e.check(&font), Some(Verdict::BlockAd));
    }

    #[test]
    fn cosmetic_merges_lists_and_ignores_scriptlets() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[
            ads("a", "example.com##.ad-banner\n"),
            tracking(
                "b",
                "example.com##.sponsor\n\
                 example.com##+js(abort-on-property-read, adblock_detected)\n",
            ),
        ])
        .unwrap();
        let sels = e.selectors_for_url("https://example.com/");
        assert!(sels.contains(&".ad-banner".to_owned()));
        assert!(sels.contains(&".sponsor".to_owned()));
        assert!(!sels.iter().any(|s| s.contains("abort-on-property-read")));
        assert!(!sels.iter().any(|s| s.contains("js(")));
    }

    #[test]
    fn integrity_mismatch_rejects_whole_swap() {
        let mut e = FilterEngine::new().unwrap();
        e.load(&[ads("good", "||ads.test^\n")]).unwrap();
        assert!(
            e.load(&[
                ads("good", "||ads.test^\n"),
                FilterList {
                    name: "evil",
                    category: ListCategory::Tracking,
                    text: "||x.test^\n",
                    expected_sha256: Some(
                        "0000000000000000000000000000000000000000000000000000000000000000"
                    ),
                },
            ])
            .is_err()
        );
        assert_eq!(
            e.check(&RequestMeta::new("ads.test", ResourceType::Script)),
            Some(Verdict::BlockAd)
        );
        assert_eq!(e.list_name(0), "good");
    }

    #[test]
    fn integrity_normalizes_trailing_newlines() {
        let mut e = FilterEngine::new().unwrap();
        let text = "||ads.test^";
        let good: String = Sha256::digest(format!("{text}\n").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(
            e.load(&[FilterList {
                name: "t",
                category: ListCategory::Ads,
                text,
                expected_sha256: Some(&good),
            }])
            .is_ok()
        );
        assert!(
            e.load(&[FilterList {
                name: "t",
                category: ListCategory::Ads,
                text: &format!("{text}\n\n"),
                expected_sha256: Some(&good),
            }])
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
