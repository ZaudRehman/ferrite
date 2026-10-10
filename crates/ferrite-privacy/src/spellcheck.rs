//! Native spellcheck with bundled dictionaries. Page-initiated dictionary
//! CDN fetches are blocked at the proxy. Ferrite provisions its own copies
//! at first run through the direct client (see TELEMETRY.md); no checker
//! is built here.

/// Delivery hosts, exact match only. CONNECT hides the path, so these
/// block whole host. Page content from them is blocked as well.
const DICT_HOSTS: &[&str] = &["redirector.gvt1.com", "edgedl.me.gvt1.com"];

/// Mixed use hosts. Only dictionary shaped paths match here.
const DICT_PATH_HOSTS: &[&str] = &["www.gstatic.com", "storage.googleapis.com", "dl.google.com"];

pub const MAX_LANGS: usize = 8;
pub const MAX_LANG_LEN: usize = 12;
pub const MAX_WORD_LEN: usize = 64;

fn norm_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// True for delivery hosts. Exact match, so subdomains never match.
pub fn is_dict_host(host: &str) -> bool {
    let h = norm_host(host);
    DICT_HOSTS.iter().any(|d| h == *d)
}

/// True for dictionary fetches. Unparseable input reads as false.
pub fn is_dict_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let h = norm_host(host);
    if DICT_HOSTS.iter().any(|d| h == *d) {
        return true;
    }
    if !DICT_PATH_HOSTS.iter().any(|d| h == *d) {
        return false;
    }
    let path = parsed.path().to_ascii_lowercase();
    path.contains("/dict/") || path.ends_with(".bdic") || path.contains("hunspell")
}

/// Default active language when no preference is stored.
pub fn default_langs() -> Vec<String> {
    vec!["en-US".to_owned()]
}

fn norm_tag(tag: &str) -> Option<String> {
    let t = tag.trim().trim_matches(',');
    if t.is_empty() || t.len() > MAX_LANG_LEN {
        return None;
    }
    if !t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if t.starts_with('-') || t.ends_with('-') || t.contains("--") {
        return None;
    }
    if !t.bytes().any(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut parts = t.split('-');
    let first = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest: Vec<String> = parts
        .map(|p| {
            if p.len() == 4 {
                let mut c = p.chars();
                match c.next() {
                    Some(f) => {
                        f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase()
                    }
                    None => String::new(),
                }
            } else {
                p.to_ascii_uppercase()
            }
        })
        .collect();
    if rest.is_empty() {
        Some(first)
    } else {
        Some(format!("{first}-{}", rest.join("-")))
    }
}

/// Normalize free text into BCP47 tags. Deduped, capped, junk dropped.
/// Empty input falls back to the default, so one dict always applies.
pub fn normalize_langs(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for piece in raw.split([',', ' ', '\t', '\n', ';']) {
        if let Some(tag) = norm_tag(piece)
            && !out.contains(&tag)
        {
            out.push(tag);
            if out.len() >= MAX_LANGS {
                break;
            }
        }
    }
    if out.is_empty() { default_langs() } else { out }
}

/// Single word selections for the native menu. None for multi word or empty.
pub fn spelling_single_word(selection: &str) -> Option<String> {
    let w: String = selection.trim().chars().take(MAX_WORD_LEN + 1).collect();
    if w.is_empty() || w.len() > MAX_WORD_LEN || w.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(w)
}

/// Installer. Applies stored prefs to editable fields. Explicit page
/// `spellcheck="false"` is never overridden. Off forces false everywhere.
pub fn spellcheck_js() -> String {
    r#"(function(){
  if (window.__ferriteSpellcheckInstalled) return;
  window.__ferriteSpellcheckInstalled = true;
  var enabled = true, langs = ["en-US"];
  function applyTo(root) {
    var els = null;
    try {
      els = (root || document).querySelectorAll(
        'input[type="text"],input[type="search"],input:not([type]),textarea,[contenteditable="true"],[contenteditable=""]'
      );
    } catch(e) { return; }
    for (var i = 0; i < els.length; i++) {
      (function(el){
        try {
          if (!enabled) {
            if (el.getAttribute("spellcheck") !== "false") el.setAttribute("spellcheck", "false");
            return;
          }
          if (!el.hasAttribute("spellcheck")) el.setAttribute("spellcheck", "true");
          if (langs.length && !el.hasAttribute("lang")) el.setAttribute("lang", langs[0]);
        } catch(e) {}
      })(els[i]);
    }
  }
  window.__ferriteSpellcheckApply = function(on, list) {
    enabled = !!on;
    if (Object.prototype.toString.call(list) === "[object Array]") {
      langs = list.filter(function(l){ return typeof l === "string" && /^[A-Za-z0-9-]{2,12}$/.test(l); }).slice(0, 8);
      if (!langs.length) langs = ["en-US"];
    }
    applyTo(document);
  };
  try {
    var obs = new MutationObserver(function(muts){
      for (var i = 0; i < muts.length; i++) {
        for (var j = 0; j < muts[i].addedNodes.length; j++) {
          var n = muts[i].addedNodes[j];
          if (n.nodeType === 1) applyTo(n.nodeType === 1 && n.querySelectorAll ? n : document);
        }
      }
    });
    if (document.documentElement) obs.observe(document.documentElement, {childList: true, subtree: true});
  } catch(e) {}
  applyTo(document);
})();"#
        .to_string()
}

/// One shot apply for nav commits and toggles. No installer means no op.
pub fn spellcheck_apply_js(enabled: bool, langs: &[String]) -> String {
    let list: Vec<String> = if langs.is_empty() {
        default_langs()
    } else {
        langs.iter().take(MAX_LANGS).cloned().collect()
    };
    let quoted = list
        .iter()
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("window.__ferriteSpellcheckApply&&__ferriteSpellcheckApply({enabled},[{quoted}])")
}

/// Bundle dir, engine profile sibling. Empty degrades to fewer suggestions.
pub fn dict_bundle_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("ferrite-profile/Dictionaries")
}

/// Engine dictionary endpoint shape (same URL the engine itself would
/// fetch when unblocked; verified against the block test below).
pub fn dict_url(tag: &str) -> Option<String> {
    let name = dict_file_name(tag)?;
    Some(format!("https://www.gstatic.com/chrome/dict/{name}"))
}

/// File name for a language tag. `None` for anything outside the tag
/// grammar, so a hostile language pref can never escape the bundle dir
/// (`..`, `/`, and overlong tags are refused here, not at the write).
pub fn dict_file_name(tag: &str) -> Option<String> {
    if tag.is_empty() || tag.len() > MAX_LANG_LEN {
        return None;
    }
    if !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if tag.starts_with('-') || tag.ends_with('-') || tag.contains("--") {
        return None;
    }
    Some(format!("{tag}.bdic"))
}

/// Languages in `langs` with no dictionary file in `dir`. Pure over the
/// directory so tests never touch the real profile.
pub fn missing_dicts_in(dir: &std::path::Path, langs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for lang in langs {
        let Some(name) = dict_file_name(lang) else {
            continue;
        };
        if !dir.join(&name).is_file() && !out.contains(lang) {
            out.push(lang.clone());
        }
    }
    out
}

/// Transport-level validation for a fetched dictionary. Guarantees status
/// was 200 upstream (checked by the fetcher), size within bounds, and not
/// an HTML/JSON error page. It does NOT authenticate the blob (no pinned
/// hash exists upstream) and does NOT validate the `.bdic` format: the
/// engine is the format authority and ignores corrupt files on load.
/// Stated plainly so nobody mistakes this gate for provenance.
pub fn valid_dict_bytes(bytes: &[u8]) -> bool {
    const MIN_DICT_BYTES: usize = 1024;
    const MAX_DICT_BYTES: usize = 16 * 1024 * 1024;
    if bytes.len() < MIN_DICT_BYTES || bytes.len() > MAX_DICT_BYTES {
        return false;
    }
    // Error pages and structured error bodies: markup opens with `<`,
    // JSON/XML errors with `{`, `[`, or `<` (after ASCII whitespace
    // trim). A binary dictionary blob never opens with those bytes.
    let first = bytes.iter().find(|b| !b.is_ascii_whitespace());
    if matches!(first, Some(b'<') | Some(b'{') | Some(b'[') | None) {
        return false;
    }
    true
}

/// Atomically store a fetched dictionary: validate, write `.part`, rename.
/// Creates the bundle dir. Returns false (no file left behind) on any
/// validation or IO failure.
pub fn store_dict_bytes(dir: &std::path::Path, tag: &str, bytes: &[u8]) -> bool {
    if !valid_dict_bytes(bytes) {
        return false;
    }
    let Some(name) = dict_file_name(tag) else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let part = dir.join(format!("{name}.part"));
    if std::fs::write(&part, bytes).is_err() {
        let _ = std::fs::remove_file(&part);
        return false;
    }
    std::fs::rename(&part, dir.join(&name)).is_ok()
}

/// Bundled `.bdic` count. Missing dir reads as 0.
pub fn bundled_dict_count() -> usize {
    let Ok(entries) = std::fs::read_dir(dict_bundle_dir()) else {
        return 0;
    };
    entries
        .take(64)
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("bdic"))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_host_is_exact_and_case_insensitive() {
        assert!(is_dict_host("redirector.gvt1.com"));
        assert!(is_dict_host("EDGEDL.ME.GVT1.COM"));
        assert!(is_dict_host("redirector.gvt1.com."));
        assert!(!is_dict_host("myredirector.gvt1.com"));
        assert!(!is_dict_host("gvt1.com"));
        assert!(!is_dict_host("www.gstatic.com"));
        assert!(!is_dict_host(""));
    }

    #[test]
    fn dict_url_matches_delivery_paths_only() {
        assert!(is_dict_url(
            "https://redirector.gvt1.com/edgedl/chrome/dict/en-us.bdic"
        ));
        assert!(is_dict_url("https://edgedl.me.gvt1.com/anything"));
        assert!(is_dict_url(
            "https://www.gstatic.com/chrome/dict/en-US.bdic"
        ));
        assert!(is_dict_url(
            "https://storage.googleapis.com/chrome/hunspell/en.dict"
        ));
        assert!(!is_dict_url("https://www.gstatic.com/fonts/roboto.woff2"));
        assert!(!is_dict_url(
            "https://storage.googleapis.com/other/file.zip"
        ));
        assert!(!is_dict_url("https://example.com/dict/en.bdic"));
        assert!(!is_dict_url("not a url"));
        assert!(!is_dict_url(""));
    }

    #[test]
    fn langs_normalize_dedupe_and_fall_back() {
        assert_eq!(normalize_langs("en-US, de"), vec!["en-US", "de"]);
        assert_eq!(normalize_langs("EN-us, en-US"), vec!["en-US"]);
        assert_eq!(normalize_langs("zh-hant-hk"), vec!["zh-Hant-HK"]);
        assert_eq!(normalize_langs(""), default_langs());
        assert_eq!(normalize_langs("!!!, ,"), default_langs());
        assert_eq!(normalize_langs("en--US"), default_langs());
        assert_eq!(normalize_langs("123"), default_langs());
        let many = normalize_langs("aa,bb,cc,dd,ee,ff,gg,hh,ii,jj");
        assert_eq!(many.len(), MAX_LANGS);
    }

    #[test]
    fn single_word_gate() {
        assert_eq!(spelling_single_word("recieve"), Some("recieve".to_owned()));
        assert_eq!(
            spelling_single_word("  recieve  "),
            Some("recieve".to_owned())
        );
        assert!(spelling_single_word("two words").is_none());
        assert!(spelling_single_word("").is_none());
        assert!(spelling_single_word("   ").is_none());
        assert!(spelling_single_word(&"x".repeat(65)).is_none());
        assert_eq!(spelling_single_word(&"y".repeat(64)), Some("y".repeat(64)));
    }

    #[test]
    fn installer_and_apply_shape() {
        let js = spellcheck_js();
        assert!(js.contains("__ferriteSpellcheckInstalled"));
        assert!(js.contains("__ferriteSpellcheckApply"));
        assert!(js.contains("MutationObserver"));
        assert!(js.contains("spellcheck"));
        assert!(!js.contains("preventDefault"));
        let apply = spellcheck_apply_js(false, &["de".to_owned()]);
        assert!(apply.contains("__ferriteSpellcheckApply"));
        assert!(apply.contains("false"));
        assert!(apply.contains("\"de\""));
        assert!(spellcheck_apply_js(true, &[]).contains("\"en-US\""));
    }

    #[test]
    fn bundle_dir_is_profile_sibling() {
        assert_eq!(
            dict_bundle_dir(),
            std::path::PathBuf::from("ferrite-profile/Dictionaries")
        );
        assert_eq!(bundled_dict_count(), 0);
    }

    #[test]
    fn dict_filenames_are_confined() {
        assert_eq!(dict_file_name("en-US"), Some("en-US.bdic".to_owned()));
        assert_eq!(dict_file_name("de"), Some("de.bdic".to_owned()));
        assert!(dict_file_name("").is_none());
        assert!(dict_file_name("../evil").is_none());
        assert!(dict_file_name("a/b").is_none());
        assert!(dict_file_name("-en").is_none());
        assert!(dict_file_name("en--US").is_none());
        assert!(dict_file_name(&"x".repeat(13)).is_none());
        assert_eq!(
            dict_url("en-US"),
            Some("https://www.gstatic.com/chrome/dict/en-US.bdic".to_owned())
        );
        assert!(dict_url("../evil").is_none());
    }

    #[test]
    fn missing_lists_only_absent_valid_tags() {
        let dir = std::env::temp_dir().join(format!("ferrite-dict-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            missing_dicts_in(&dir, &["en-US".to_owned(), "de".to_owned()]),
            vec!["en-US".to_owned(), "de".to_owned()]
        );
        assert!(missing_dicts_in(&dir, &["../evil".to_owned()]).is_empty());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("en-US.bdic"), vec![0u8; 2048]).unwrap();
        assert_eq!(
            missing_dicts_in(
                &dir,
                &["en-US".to_owned(), "en-US".to_owned(), "de".to_owned()]
            ),
            vec!["de".to_owned()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dict_bytes_gate_rejects_markup_and_bounds() {
        assert!(!valid_dict_bytes(&[]));
        assert!(!valid_dict_bytes(&vec![0u8; 512]));
        assert!(!valid_dict_bytes(b"<!DOCTYPE html>"));
        assert!(!valid_dict_bytes(b"   <html>"));
        let mut big_json = b"{\"error\": 404, \"msg\": \"".to_vec();
        big_json.extend(std::iter::repeat_n(b'x', 2048));
        big_json.extend_from_slice(b"\"}");
        assert!(!valid_dict_bytes(&big_json));
        let mut blob = vec![0x42u8; 2048];
        blob[0] = 0x1F;
        assert!(valid_dict_bytes(&blob));
        assert!(!valid_dict_bytes(&vec![0u8; 17 * 1024 * 1024]));
    }

    #[test]
    fn store_is_atomic_and_validated() {
        let dir = std::env::temp_dir().join(format!("ferrite-dict-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!store_dict_bytes(&dir, "en-US", b"<html>"));
        assert!(!dir.join("en-US.bdic").exists());
        assert!(!store_dict_bytes(&dir, "../evil", &vec![9u8; 2048]));
        let blob = vec![7u8; 2048];
        assert!(store_dict_bytes(&dir, "en-US", &blob));
        assert_eq!(std::fs::read(dir.join("en-US.bdic")).unwrap(), blob);
        assert!(!dir.join("en-US.bdic.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
